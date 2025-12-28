//! org.freedesktop.systemd1.Manager interface
//!
//! This is the main interface logind uses to:
//! - Create session scopes (StartTransientUnit)
//! - Stop units (StopUnit)
//! - Kill processes in units (KillUnit)
//! - Subscribe to signals (Subscribe)

use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::{interface, fdo, object_server::SignalEmitter, zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value}};

use crate::runtime::RuntimeInfo;
use super::unit_object_path;

/// Job counter for generating unique job IDs
static JOB_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn next_job_id() -> u32 {
    JOB_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

fn job_path(id: u32) -> OwnedObjectPath {
    ObjectPath::try_from(format!("/org/freedesktop/systemd1/job/{}", id))
        .unwrap()
        .into()
}

pub struct ManagerInterface {
    runtime: Arc<RwLock<RuntimeInfo>>,
}

impl ManagerInterface {
    pub fn new(runtime: Arc<RwLock<RuntimeInfo>>) -> Self {
        Self { runtime }
    }
}

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl ManagerInterface {
    /// Start a unit by name. Returns the job object path.
    async fn start_unit(&self, name: &str, mode: &str) -> fdo::Result<OwnedObjectPath> {
        let _runtime = self.runtime.read().await;
        log::info!("StartUnit: {} mode={}", name, mode);
        Ok(job_path(next_job_id()))
    }

    /// Stop a unit by name
    async fn stop_unit(&self, name: &str, mode: &str) -> fdo::Result<OwnedObjectPath> {
        let _runtime = self.runtime.read().await;
        log::info!("StopUnit: {} mode={}", name, mode);
        Ok(job_path(next_job_id()))
    }

    /// Kill processes in a unit (whom: "main", "control", "all")
    async fn kill_unit(&self, name: &str, whom: &str, signal: i32) -> fdo::Result<()> {
        let _runtime = self.runtime.read().await;
        log::info!("KillUnit: {} whom={} signal={}", name, whom, signal);
        // TODO: Find unit, get PIDs from cgroup, send signal
        Ok(())
    }

    /// Create and start a transient unit (used by logind for session scopes)
    async fn start_transient_unit(
        &self,
        name: &str,
        mode: &str,
        properties: Vec<(String, OwnedValue)>,
        _aux: Vec<(String, Vec<(String, OwnedValue)>)>,
    ) -> fdo::Result<OwnedObjectPath> {
        log::info!("StartTransientUnit: {} mode={}", name, mode);

        let (slice, description, pids) = parse_scope_properties(&properties);

        log::info!(
            "Creating scope: name={} slice={:?} desc={:?} pids={:?}",
            name, slice, description, pids
        );

        // TODO:
        // 1. Create cgroup: /sys/fs/cgroup/{slice}/{name}/
        // 2. Move PIDs into cgroup
        // 3. Create Scope unit in runtime
        // 4. Register D-Bus object for the scope
        // 5. Emit JobRemoved signal when done

        Ok(job_path(next_job_id()))
    }

    /// Subscribe to Manager signals
    async fn subscribe(&self) -> fdo::Result<()> {
        log::info!("Subscribe called");
        Ok(())
    }

    /// Reload daemon configuration
    async fn reload(&self) -> fdo::Result<()> {
        log::info!("Reload called");
        Ok(())
    }

    /// Get unit by name, returns object path
    async fn get_unit(&self, name: &str) -> fdo::Result<OwnedObjectPath> {
        let path = unit_object_path(name);
        Ok(ObjectPath::try_from(path).unwrap().into())
    }

    /// Load a unit file
    async fn load_unit(&self, name: &str) -> fdo::Result<OwnedObjectPath> {
        let path = unit_object_path(name);
        Ok(ObjectPath::try_from(path).unwrap().into())
    }

    // ==================== Signals ====================

    /// Emitted when a job completes
    #[zbus(signal)]
    async fn job_removed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        job: ObjectPath<'_>,
        unit: &str,
        result: &str,
    ) -> zbus::Result<()>;

    /// Emitted when a unit is removed/unloaded
    #[zbus(signal)]
    async fn unit_removed(
        emitter: &SignalEmitter<'_>,
        unit: &str,
        path: ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Emitted when daemon is reloading
    #[zbus(signal)]
    async fn reloading(emitter: &SignalEmitter<'_>, active: bool) -> zbus::Result<()>;

    // ==================== Properties ====================

    #[zbus(property)]
    async fn version(&self) -> String {
        "sysd 0.1.0".to_string()
    }
}

/// Parse properties from StartTransientUnit call
fn parse_scope_properties(
    properties: &[(String, OwnedValue)],
) -> (Option<String>, Option<String>, Vec<u32>) {
    let mut slice = None;
    let mut description = None;
    let mut pids = Vec::new();

    for (key, value) in properties {
        match key.as_str() {
            "Slice" => {
                if let Value::Str(s) = value.downcast_ref().unwrap_or(&Value::U32(0)) {
                    slice = Some(s.to_string());
                }
            }
            "Description" => {
                if let Value::Str(s) = value.downcast_ref().unwrap_or(&Value::U32(0)) {
                    description = Some(s.to_string());
                }
            }
            "PIDs" => {
                if let Value::Array(arr) = value.downcast_ref().unwrap_or(&Value::U32(0)) {
                    for v in arr.iter() {
                        if let Value::U32(pid) = v {
                            pids.push(*pid);
                        }
                    }
                }
            }
            _ => {
                log::debug!("StartTransientUnit: ignoring property {}", key);
            }
        }
    }

    (slice, description, pids)
}
