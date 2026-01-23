//! org.freedesktop.systemd1.Manager interface
//!
//! This is the main interface logind uses to:
//! - Create session scopes (StartTransientUnit)
//! - Stop units (StopUnit)
//! - Kill processes in units (KillUnit)
//! - Subscribe to signals (Subscribe)

use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::RwLock;
use zbus::{
    fdo, interface,
    object_server::SignalEmitter,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

use super::unit_object_path;
use crate::manager::Manager;

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
    manager: Arc<RwLock<Manager>>,
    handle: Handle,
}

impl ManagerInterface {
    pub fn new(manager: Arc<RwLock<Manager>>) -> Self {
        Self {
            manager,
            handle: Handle::current(),
        }
    }

    /// Emit JobRemoved signal
    pub async fn emit_job_removed(
        ctx: &zbus::object_server::SignalEmitter<'_>,
        job_id: u32,
        unit: &str,
        result: &str,
    ) -> zbus::Result<()> {
        let job = job_path(job_id);
        Self::job_removed(ctx, job_id, job.as_ref(), unit, result).await
    }

    /// Emit UnitRemoved signal
    pub async fn emit_unit_removed(
        ctx: &zbus::object_server::SignalEmitter<'_>,
        unit: &str,
    ) -> zbus::Result<()> {
        let path = super::unit_object_path(unit);
        let obj_path = ObjectPath::try_from(path.as_str()).unwrap();
        Self::unit_removed(ctx, unit, obj_path).await
    }
}

#[interface(name = "org.freedesktop.systemd1.Manager")]
impl ManagerInterface {
    /// Start a unit by name. Returns the job object path.
    async fn start_unit(
        &self,
        #[zbus(signal_context)] ctx: zbus::object_server::SignalEmitter<'_>,
        name: &str,
        mode: &str,
    ) -> fdo::Result<OwnedObjectPath> {
        log::info!("D-Bus StartUnit: {} mode={}", name, mode);

        let job_id = next_job_id();
        let job = job_path(job_id);
        let manager = Arc::clone(&self.manager);
        let unit_name = name.to_string();
        let conn = ctx.connection().clone();

        self.handle.spawn(async move {
            // Check for special user-level services that logind expects
            // These need immediate JobRemoved emission since we don't run them
            let job_result = if unit_name.starts_with("user-runtime-dir@") {
                // Create the user runtime directory
                let uid_str = unit_name
                    .strip_prefix("user-runtime-dir@")
                    .and_then(|s| s.strip_suffix(".service"))
                    .unwrap_or("0");
                if let Ok(uid) = uid_str.parse::<u32>() {
                    let runtime_dir = format!("/run/user/{}", uid);
                    match std::fs::create_dir_all(&runtime_dir) {
                        Ok(()) => {
                            // Set permissions (mode 0700, owned by user)
                            let _ = std::fs::set_permissions(
                                &runtime_dir,
                                std::fs::Permissions::from_mode(0o700),
                            );
                            // Set ownership
                            unsafe {
                                libc::chown(
                                    std::ffi::CString::new(runtime_dir.as_str()).unwrap().as_ptr(),
                                    uid,
                                    uid,
                                );
                            }
                            log::info!("Created user runtime directory: {}", runtime_dir);
                            "done"
                        }
                        Err(e) => {
                            log::error!("Failed to create {}: {}", runtime_dir, e);
                            "failed"
                        }
                    }
                } else {
                    "failed"
                }
            } else if unit_name.starts_with("user@") {
                // user@.service manages systemd --user, which we don't support
                // Just report as "done" so logind continues
                log::info!("Skipping user@.service (systemd --user not supported)");
                "done"
            } else {
                // Regular unit start
                let mut mgr = manager.write().await;
                match mgr.start(&unit_name).await {
                    Ok(()) => "done",
                    Err(e) => {
                        log::error!("StartUnit {} failed: {}", unit_name, e);
                        "failed"
                    }
                }
            };

            // Emit JobRemoved signal
            log::info!(
                "Emitting JobRemoved for StartUnit: id={}, unit={}, result={}",
                job_id, unit_name, job_result
            );
            if let Ok(ctx) = SignalEmitter::new(&conn, "/org/freedesktop/systemd1") {
                if let Err(e) = ManagerInterface::job_removed(
                    &ctx,
                    job_id,
                    job_path(job_id).as_ref(),
                    &unit_name,
                    job_result,
                )
                .await
                {
                    log::warn!("Failed to emit JobRemoved signal: {}", e);
                }
            }
        });

        Ok(job)
    }

    /// Stop a unit by name
    async fn stop_unit(&self, name: &str, mode: &str) -> fdo::Result<OwnedObjectPath> {
        log::info!("D-Bus StopUnit: {} mode={}", name, mode);
        let manager = Arc::clone(&self.manager);
        let name = name.to_string();
        self.handle.spawn(async move {
            let mut mgr = manager.write().await;
            if let Err(e) = mgr.stop(&name).await {
                log::error!("StopUnit {} failed: {}", name, e);
            }
        });
        Ok(job_path(next_job_id()))
    }

    /// Kill processes in a unit (whom: "main", "control", "all")
    async fn kill_unit(&self, name: &str, whom: &str, signal: i32) -> fdo::Result<()> {
        log::info!("D-Bus KillUnit: {} whom={} signal={}", name, whom, signal);
        // Get the process and send signal
        let manager = self.manager.read().await;
        if let Some(state) = manager.status(name) {
            if let Some(pid) = state.main_pid {
                unsafe {
                    libc::kill(pid as i32, signal);
                }
            }
        }
        Ok(())
    }

    /// Create and start a transient unit (used by logind for session scopes)
    ///
    /// Logind uses this to create session scopes like "session-1.scope".
    /// Returns immediately and creates the scope asynchronously (matching systemd behavior).
    async fn start_transient_unit(
        &self,
        #[zbus(signal_context)] ctx: zbus::object_server::SignalEmitter<'_>,
        name: &str,
        mode: &str,
        properties: Vec<(String, OwnedValue)>,
        _aux: Vec<(String, Vec<(String, OwnedValue)>)>,
    ) -> fdo::Result<OwnedObjectPath> {
        let (slice, description, pids) = parse_scope_properties(&properties);

        log::info!(
            "StartTransientUnit: name={} mode={} slice={:?} desc={:?} pids={:?}",
            name,
            mode,
            slice,
            description,
            pids
        );

        // Generate job ID and path
        let job_id = next_job_id();
        let job = job_path(job_id);
        let unit_name = name.to_string();

        // Clone what we need for the async task
        let manager = Arc::clone(&self.manager);
        let slice_owned = slice.clone();
        let desc_owned = description.clone();
        let conn = ctx.connection().clone();

        // Spawn the scope creation asynchronously (matching systemd's job model)
        // This returns the job path immediately while the scope is created in the background
        self.handle.spawn(async move {
            // Create the scope
            let result = {
                let mut mgr = manager.write().await;
                mgr.register_scope(
                    &unit_name,
                    slice_owned.as_deref(),
                    desc_owned.as_deref(),
                    &pids,
                )
                .await
            };

            // Emit JobRemoved signal when done
            let job_result = if let Err(e) = result {
                log::error!("Failed to register scope {}: {}", unit_name, e);
                "failed"
            } else {
                "done"
            };

            // Emit JobRemoved signal
            log::info!(
                "Emitting JobRemoved: id={}, job={}, unit={}, result={}",
                job_id,
                job_path(job_id).as_str(),
                unit_name,
                job_result
            );
            if let Ok(ctx) = SignalEmitter::new(&conn, "/org/freedesktop/systemd1") {
                if let Err(e) = ManagerInterface::job_removed(
                    &ctx,
                    job_id,
                    job_path(job_id).as_ref(),
                    &unit_name,
                    job_result,
                )
                .await
                {
                    log::warn!("Failed to emit JobRemoved signal: {}", e);
                } else {
                    log::info!("Scope {} created, JobRemoved emitted successfully", unit_name);
                }
            } else {
                log::error!("Failed to create SignalEmitter for JobRemoved");
            }
        });

        Ok(job)
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
            "PIDFDs" => {
                // PIDFDs are file descriptors passed over D-Bus
                // Convert each PIDFD to a PID via /proc/self/fdinfo
                if let Value::Array(arr) = value.downcast_ref().unwrap_or(&Value::U32(0)) {
                    for v in arr.iter() {
                        // zvariant stores FDs as Fd type, downcast_ref returns Result
                        if let Ok(fd) = v.downcast_ref::<zbus::zvariant::Fd>() {
                            match pidfd_to_pid(fd.as_raw_fd()) {
                                Ok(pid) => {
                                    log::info!("PIDFDs: converted fd {} to pid {}", fd.as_raw_fd(), pid);
                                    pids.push(pid);
                                }
                                Err(e) => {
                                    log::warn!("PIDFDs: failed to convert fd {}: {}", fd.as_raw_fd(), e);
                                }
                            }
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

/// Convert a PIDFD (process file descriptor) to a PID
fn pidfd_to_pid(pidfd: std::os::unix::io::RawFd) -> Result<u32, std::io::Error> {
    // Read /proc/self/fdinfo/<fd> and parse the Pid: line
    let path = format!("/proc/self/fdinfo/{}", pidfd);
    let content = std::fs::read_to_string(&path)?;
    for line in content.lines() {
        if let Some(pid_str) = line.strip_prefix("Pid:\t") {
            if let Ok(pid) = pid_str.parse::<u32>() {
                return Ok(pid);
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Pid not found in fdinfo",
    ))
}
