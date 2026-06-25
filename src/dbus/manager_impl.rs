// org.freedesktop.systemd1.Manager interface
//
// This is the main interface logind uses to:
// - Create session scopes (StartTransientUnit)
// - Stop units (StopUnit)
// - Kill processes in units (KillUnit)
// - Subscribe to signals (Subscribe)

use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
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
            let job_result = resolve_start_unit_result(manager, &unit_name).await;
            emit_job_removed_signal(&conn, job_id, &unit_name, job_result, "StartUnit").await;
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
        log_scope_start(name, mode, slice.as_deref(), description.as_deref(), &pids);

        let job_id = next_job_id();
        let job = job_path(job_id);
        let unit_name = name.to_string();
        let manager = Arc::clone(&self.manager);
        let conn = ctx.connection().clone();

        self.handle.spawn(async move {
            let job_result = register_scope_job(
                manager,
                &unit_name,
                slice.as_deref(),
                description.as_deref(),
                &pids,
            )
            .await;
            emit_job_removed_signal(&conn, job_id, &unit_name, job_result, "StartTransientUnit")
                .await;
            if job_result == "done" {
                log::info!("Scope {} created, JobRemoved emitted successfully", unit_name);
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

const USER_RUNTIME_DIR_PREFIX: &str = "user-runtime-dir@";
const USER_MANAGER_PREFIX: &str = "user@";
const SYSTEMD_SERVICE_SUFFIX: &str = ".service";

async fn resolve_start_unit_result(manager: Arc<RwLock<Manager>>, unit_name: &str) -> &'static str {
    if let Some(result) = start_special_user_unit(unit_name) {
        return result;
    }
    start_regular_unit(manager, unit_name).await
}

fn start_special_user_unit(unit_name: &str) -> Option<&'static str> {
    if unit_name.starts_with(USER_RUNTIME_DIR_PREFIX) {
        return Some(start_user_runtime_dir(unit_name));
    }
    if unit_name.starts_with(USER_MANAGER_PREFIX) {
        return Some(start_user_manager_unit(unit_name));
    }
    None
}

fn start_user_runtime_dir(unit_name: &str) -> &'static str {
    let Some(uid) = parse_uid_from_unit(unit_name, USER_RUNTIME_DIR_PREFIX) else {
        log::error!("Invalid uid in {}: {}", USER_RUNTIME_DIR_PREFIX, unit_name);
        return "failed";
    };

    let runtime_dir = format!("/run/user/{}", uid);
    if let Err(e) = std::fs::create_dir_all(&runtime_dir) {
        log::error!("Failed to create {}: {}", runtime_dir, e);
        return "failed";
    }

    if let Err(e) =
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
    {
        log::warn!("Failed to set permissions for {}: {}", runtime_dir, e);
    }

    match std::ffi::CString::new(runtime_dir.as_str()) {
        Ok(runtime_dir_cstr) => {
            unsafe { libc::chown(runtime_dir_cstr.as_ptr(), uid, uid) };
        }
        Err(e) => {
            log::error!("Failed to build runtime path for chown: {}", e);
            return "failed";
        }
    }

    log::info!("Created user runtime directory: {}", runtime_dir);
    "done"
}

fn start_user_manager_unit(unit_name: &str) -> &'static str {
    let Some(uid) = parse_uid_from_unit(unit_name, USER_MANAGER_PREFIX) else {
        log::error!("Invalid uid in {}: {}", USER_MANAGER_PREFIX, unit_name);
        return "failed";
    };

    let runtime_dir = format!("/run/user/{}", uid);
    let sysd_socket = format!("{}/sysd.sock", runtime_dir);
    if std::path::Path::new(&sysd_socket).exists() {
        log::info!("User sysd already running for uid {}", uid);
        return "done";
    }

    let bus_path = format!("{}/bus", runtime_dir);
    if !ensure_user_session_bus(uid, &runtime_dir, &bus_path) {
        return "failed";
    }
    start_user_sysd(uid, &runtime_dir, &sysd_socket, &bus_path)
}

fn parse_uid_from_unit(unit_name: &str, prefix: &str) -> Option<u32> {
    unit_name
        .strip_prefix(prefix)
        .and_then(|s| s.strip_suffix(SYSTEMD_SERVICE_SUFFIX))
        .and_then(|uid| uid.parse::<u32>().ok())
}

fn ensure_user_session_bus(uid: u32, runtime_dir: &str, bus_path: &str) -> bool {
    if std::path::Path::new(bus_path).exists() {
        return true;
    }

    let address = format!("unix:path={}", bus_path);
    let spawn_result = std::process::Command::new("dbus-daemon")
        .args([
            "--session",
            "--address",
            &address,
            "--nofork",
            "--nopidfile",
        ])
        .uid(uid)
        .gid(uid)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match spawn_result {
        Ok(child) => {
            log::info!(
                "Started user D-Bus daemon for uid {} (PID {}) at {}",
                uid,
                child.id(),
                bus_path
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
            true
        }
        Err(e) => {
            log::error!("Failed to start user D-Bus for uid {}: {}", uid, e);
            false
        }
    }
}

fn start_user_sysd(uid: u32, runtime_dir: &str, sysd_socket: &str, bus_path: &str) -> &'static str {
    let dbus_addr = format!("unix:path={}", bus_path);
    let spawn_result = std::process::Command::new("/usr/bin/sysd")
        .args(["--user"])
        .uid(uid)
        .gid(uid)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", &dbus_addr)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match spawn_result {
        Ok(child) => {
            log::info!(
                "Started user sysd for uid {} (PID {}) at {}",
                uid,
                child.id(),
                sysd_socket
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
            "done"
        }
        Err(e) => {
            log::error!("Failed to start user sysd for uid {}: {}", uid, e);
            "failed"
        }
    }
}

async fn start_regular_unit(manager: Arc<RwLock<Manager>>, unit_name: &str) -> &'static str {
    let mut mgr = manager.write().await;
    match mgr.start(unit_name).await {
        Ok(()) => "done",
        Err(e) => {
            log::error!("StartUnit {} failed: {}", unit_name, e);
            "failed"
        }
    }
}

fn log_scope_start(
    name: &str,
    mode: &str,
    slice: Option<&str>,
    description: Option<&str>,
    pids: &[u32],
) {
    log::info!(
        "StartTransientUnit: name={} mode={} slice={:?} desc={:?} pids={:?}",
        name,
        mode,
        slice,
        description,
        pids
    );
}

async fn register_scope_job(
    manager: Arc<RwLock<Manager>>,
    unit_name: &str,
    slice: Option<&str>,
    description: Option<&str>,
    pids: &[u32],
) -> &'static str {
    let result = {
        let mut mgr = manager.write().await;
        mgr.register_scope(unit_name, slice, description, pids).await
    };

    match result {
        Ok(_) => "done",
        Err(e) => {
            log::error!("Failed to register scope {}: {}", unit_name, e);
            "failed"
        }
    }
}

async fn emit_job_removed_signal(
    conn: &zbus::Connection,
    job_id: u32,
    unit_name: &str,
    job_result: &str,
    source: &str,
) {
    log::info!(
        "Emitting JobRemoved for {}: id={}, unit={}, result={}",
        source,
        job_id,
        unit_name,
        job_result
    );

    let Ok(ctx) = SignalEmitter::new(conn, "/org/freedesktop/systemd1") else {
        log::error!("Failed to create SignalEmitter for JobRemoved");
        return;
    };

    if let Err(e) = ManagerInterface::job_removed(
        &ctx,
        job_id,
        job_path(job_id).as_ref(),
        unit_name,
        job_result,
    )
    .await
    {
        log::warn!("Failed to emit JobRemoved signal: {}", e);
    }
}

fn parse_string_property(value: &OwnedValue) -> Option<String> {
    let Ok(value) = value.downcast_ref::<Value<'_>>() else {
        return None;
    };
    match value {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn collect_u32_array(value: &OwnedValue, pids: &mut Vec<u32>) {
    let Ok(value) = value.downcast_ref::<Value<'_>>() else {
        return;
    };
    let Value::Array(arr) = value else {
        return;
    };

    pids.extend(arr.iter().filter_map(|entry| match entry {
        Value::U32(pid) => Some(*pid),
        _ => None,
    }));
}

fn collect_pidfds(value: &OwnedValue, pids: &mut Vec<u32>) {
    let Ok(value) = value.downcast_ref::<Value<'_>>() else {
        return;
    };
    let Value::Array(arr) = value else {
        return;
    };

    for entry in arr.iter() {
        let Ok(fd) = entry.downcast_ref::<zbus::zvariant::Fd>() else {
            continue;
        };
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

/// Parse properties from StartTransientUnit call
fn parse_scope_properties(
    properties: &[(String, OwnedValue)],
) -> (Option<String>, Option<String>, Vec<u32>) {
    let mut slice = None;
    let mut description = None;
    let mut pids = Vec::new();

    for (key, value) in properties {
        match key.as_str() {
            "Slice" => slice = parse_string_property(value),
            "Description" => description = parse_string_property(value),
            "PIDs" => collect_u32_array(value, &mut pids),
            "PIDFDs" => collect_pidfds(value, &mut pids),
            _ => log::debug!("StartTransientUnit: ignoring property {}", key),
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
