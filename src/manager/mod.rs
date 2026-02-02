//! Service manager
//!
//! Loads, starts, stops, and monitors services and targets.

mod conditions;
mod deps;
mod dynamic_user;
mod enable;
mod generators;
mod mount_ops;
mod notify;
mod process;
mod runtime;
pub mod sandbox;
pub mod scope;
mod slice_ops;
mod socket_ops;
mod socket_watcher;
mod state;
mod timer_ops;
mod timer_scheduler;
mod path_ops;
mod path_watcher;
mod virtualization;

pub use deps::{CycleError, DepGraph};
pub use notify::{AsyncNotifyListener, NotifyMessage, NOTIFY_SOCKET_PATH};
pub use process::{SpawnError, SpawnOptions};
pub use sandbox::apply_sandbox;
pub use scope::ScopeManager;
pub use socket_watcher::SocketActivation;
pub use state::{ActiveState, ServiceState, SubState};
pub use timer_scheduler::TimerFired;
pub use virtualization::VirtualizationType;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use tokio::process::Child;
use tokio::sync::mpsc;

use crate::cgroups::{CgroupLimits, CgroupManager};
use crate::units::{self, KillMode, Service, ServiceType, Unit};

/// Message sent when a oneshot command completes
#[derive(Debug)]
pub struct OneshotCompletion {
    /// Service name
    pub service_name: String,
    /// Command index (0-based)
    pub cmd_idx: usize,
    /// Total number of commands
    pub total_cmds: usize,
    /// Exit code (None if killed by signal)
    pub exit_code: Option<i32>,
    /// Error message if failed
    pub error: Option<String>,
    /// Whether this service has RemainAfterExit set
    pub remain_after_exit: bool,
}

/// Service manager that tracks and controls units (services and targets)
pub struct Manager {
    /// Loaded unit definitions (services and targets)
    units: HashMap<String, Unit>,
    /// Runtime state for each unit
    states: HashMap<String, ServiceState>,
    /// Running child processes (only for services)
    processes: HashMap<String, Child>,
    /// Unit search paths
    unit_paths: Vec<PathBuf>,
    /// Notify socket listener for Type=notify services
    notify_listener: Option<AsyncNotifyListener>,
    /// Receiver for notify messages
    notify_rx: Option<mpsc::Receiver<NotifyMessage>>,
    /// Map of PIDs waiting for READY notification
    waiting_ready: HashMap<u32, String>,
    /// Cgroup manager (None if cgroups unavailable)
    cgroup_manager: Option<CgroupManager>,
    /// Active cgroup paths for services
    cgroup_paths: HashMap<String, PathBuf>,
    /// PIDFile paths for Type=forking services
    pid_files: HashMap<String, PathBuf>,
    /// Count of active jobs (for Type=idle)
    active_jobs: u32,
    /// Services waiting for D-Bus name acquisition (bus_name -> service_name)
    waiting_bus_name: HashMap<String, String>,
    /// Watchdog deadlines for services (service_name -> deadline)
    watchdog_deadlines: HashMap<String, std::time::Instant>,
    /// Active listening sockets (socket unit name -> file descriptors)
    socket_fds: HashMap<String, Vec<RawFd>>,
    /// Channel for socket activation messages
    socket_activation_tx: mpsc::Sender<socket_watcher::SocketActivation>,
    /// Receiver for socket activation messages
    socket_activation_rx: Option<mpsc::Receiver<socket_watcher::SocketActivation>>,
    /// Channel for timer fired messages
    timer_tx: mpsc::Sender<timer_scheduler::TimerFired>,
    /// Receiver for timer fired messages
    timer_rx: Option<mpsc::Receiver<timer_scheduler::TimerFired>>,
    /// Channel for path triggered messages
    path_tx: mpsc::Sender<path_watcher::PathTriggered>,
    /// Receiver for path triggered messages
    path_rx: Option<mpsc::Receiver<path_watcher::PathTriggered>>,
    /// Boot time for monotonic timer calculations
    boot_time: std::time::Instant,
    /// Scope manager for transient scopes (logind sessions)
    scope_manager: ScopeManager,
    /// M19: Dynamic user manager for DynamicUser= services
    dynamic_user_manager: dynamic_user::DynamicUserManager,
    /// M19: Allocated dynamic UIDs (service_name -> uid)
    dynamic_uids: HashMap<String, u32>,
    /// M19: Stored file descriptors for FileDescriptorStoreMax= services
    /// Map of service_name -> Vec<(fd_name, raw_fd)>
    fd_store: HashMap<String, Vec<(String, RawFd)>>,
    /// Path to sysd-executor binary for sd-executor pattern
    executor_path: String,
    /// Map of PID -> service name for tracking which process belongs to which service
    pid_to_service: HashMap<u32, String>,
    /// Channel for oneshot completion messages
    oneshot_completion_tx: mpsc::Sender<OneshotCompletion>,
    /// Receiver for oneshot completion messages
    oneshot_completion_rx: Option<mpsc::Receiver<OneshotCompletion>>,
    /// Pending oneshot services (services waiting for next command to start)
    /// Map of service_name -> (next_cmd_idx, total_cmds, remain_after_exit)
    pending_oneshot_cmds: HashMap<String, (usize, usize, bool)>,
    /// Imported environment variables (for user session management)
    user_environment: HashMap<String, String>,
    /// Whether running in user mode (vs system mode)
    user_mode: bool,
}

impl Manager {
    /// Create a new service manager for system mode
    pub fn new() -> Self {
        Self::with_mode(false)
    }

    /// Create a new service manager for user mode
    pub fn new_user() -> Self {
        Self::with_mode(true)
    }

    /// Create a service manager with explicit mode
    fn with_mode(user_mode: bool) -> Self {
        // Try to initialize cgroup manager (may fail if not root or cgroups unavailable)
        let cgroup_manager = if user_mode {
            // User mode doesn't typically have cgroup write access
            None
        } else {
            match CgroupManager::new() {
                Ok(mgr) => {
                    log::debug!("Cgroup manager initialized");
                    Some(mgr)
                }
                Err(e) => {
                    log::debug!(
                        "Cgroup manager unavailable: {} (running without cgroups)",
                        e
                    );
                    None
                }
            }
        };

        // Create socket activation channel
        let (socket_activation_tx, socket_activation_rx) = mpsc::channel(32);

        // Create timer fired channel
        let (timer_tx, timer_rx) = mpsc::channel(32);

        // Create path triggered channel
        let (path_tx, path_rx) = mpsc::channel(32);

        // Create oneshot completion channel
        let (oneshot_completion_tx, oneshot_completion_rx) = mpsc::channel(32);

        // Set unit paths based on mode
        let unit_paths = if user_mode {
            Self::user_unit_paths()
        } else {
            vec![
                PathBuf::from("/etc/systemd/system"),
                PathBuf::from("/usr/lib/systemd/system"),
            ]
        };

        // Clone for scope_manager before moving into struct
        let scope_manager = ScopeManager::new(cgroup_manager.clone());

        // Find executor binary (next to current binary, or fallback to PATH)
        let executor_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("sysd-executor")))
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "sysd-executor".to_string());

        Self {
            units: HashMap::new(),
            states: HashMap::new(),
            processes: HashMap::new(),
            unit_paths,
            notify_listener: None,
            notify_rx: None,
            waiting_ready: HashMap::new(),
            cgroup_manager,
            cgroup_paths: HashMap::new(),
            pid_files: HashMap::new(),
            active_jobs: 0,
            waiting_bus_name: HashMap::new(),
            watchdog_deadlines: HashMap::new(),
            socket_fds: HashMap::new(),
            socket_activation_tx,
            socket_activation_rx: Some(socket_activation_rx),
            timer_tx,
            timer_rx: Some(timer_rx),
            path_tx,
            path_rx: Some(path_rx),
            boot_time: std::time::Instant::now(),
            scope_manager,
            dynamic_user_manager: dynamic_user::DynamicUserManager::new(),
            dynamic_uids: HashMap::new(),
            fd_store: HashMap::new(),
            executor_path,
            pid_to_service: HashMap::new(),
            oneshot_completion_tx,
            oneshot_completion_rx: Some(oneshot_completion_rx),
            pending_oneshot_cmds: HashMap::new(),
            user_environment: HashMap::new(),
            user_mode,
        }
    }

    /// Get unit search paths for user mode
    fn user_unit_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // User-specific config directory (highest priority)
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("systemd/user"));
        }
        // Also check XDG_CONFIG_HOME or fallback to ~/.config
        if let Ok(home) = std::env::var("HOME") {
            let user_config = PathBuf::from(&home).join(".config/systemd/user");
            if !paths.contains(&user_config) {
                paths.push(user_config);
            }
        }

        // System-wide user unit directories
        paths.push(PathBuf::from("/etc/systemd/user"));
        paths.push(PathBuf::from("/usr/lib/systemd/user"));

        // XDG data directories for user units
        if let Some(data_dir) = dirs::data_dir() {
            paths.push(data_dir.join("systemd/user"));
        }

        paths
    }

    /// Check if user has lingering enabled
    pub fn is_lingering(username: &str) -> bool {
        std::path::Path::new(&format!("/var/lib/systemd/linger/{}", username)).exists()
    }

    /// Get the current user's runtime directory
    pub fn user_runtime_dir() -> Option<PathBuf> {
        std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                let uid = unsafe { libc::getuid() };
                let path = PathBuf::from(format!("/run/user/{}", uid));
                if path.exists() {
                    Some(path)
                } else {
                    None
                }
            })
    }

    /// Ensure XDG_RUNTIME_DIR exists and has correct permissions
    pub fn ensure_runtime_dir() -> std::io::Result<PathBuf> {
        let uid = unsafe { libc::getuid() };
        let runtime_dir = PathBuf::from(format!("/run/user/{}", uid));

        if !runtime_dir.exists() {
            std::fs::create_dir_all(&runtime_dir)?;
            // Set permissions to 0700 (owner only)
            std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))?;
        }

        // Set environment variable if not already set
        if std::env::var("XDG_RUNTIME_DIR").is_err() {
            std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        }

        Ok(runtime_dir)
    }

    /// Check if cgroup management is available
    pub fn cgroups_available(&self) -> bool {
        self.cgroup_manager.is_some()
    }

    /// Check if running in user mode
    pub fn is_user_mode(&self) -> bool {
        self.user_mode
    }

    /// Get the directory for enable/disable symlinks
    ///
    /// In system mode: /etc/systemd/system
    /// In user mode: ~/.config/systemd/user (first user path)
    pub fn enable_dir(&self) -> PathBuf {
        if self.user_mode {
            // Use first user path (highest priority, typically ~/.config/systemd/user)
            self.unit_paths
                .first()
                .cloned()
                .unwrap_or_else(|| PathBuf::from("/etc/systemd/user"))
        } else {
            PathBuf::from("/etc/systemd/system")
        }
    }

    /// Initialize the notify socket listener
    pub fn init_notify_socket(&mut self) -> std::io::Result<()> {
        let socket_path = self.notify_socket_path_for_mode();
        let (listener, rx) = AsyncNotifyListener::new(std::path::Path::new(&socket_path))?;
        self.notify_listener = Some(listener);
        self.notify_rx = Some(rx);
        log::info!("Notify socket listening at {}", socket_path);
        Ok(())
    }

    /// Get the notify socket path based on mode (system vs user)
    fn notify_socket_path_for_mode(&self) -> String {
        if self.user_mode {
            // User mode: /run/user/<uid>/sysd/notify
            let uid = nix::unistd::getuid().as_raw();
            format!("/run/user/{}/sysd/notify", uid)
        } else {
            // System mode: /run/sysd/notify
            NOTIFY_SOCKET_PATH.to_string()
        }
    }

    /// Get the notify socket path (if initialized)
    pub fn notify_socket_path(&self) -> Option<&std::path::Path> {
        self.notify_listener.as_ref().map(|l| l.socket_path())
    }

    /// Load a unit (service or target) by name
    /// Load a unit by name, returning the canonical name it was stored under
    /// (may differ from input if the unit file is a symlink)
    pub async fn load(&mut self, name: &str) -> Result<String, ManagerError> {
        // Normalize the name
        let mut name = self.normalize_name(name);

        // Handle bare templates (e.g., "foo@.service") with DefaultInstance
        if units::is_bare_template(&name) {
            // Load the template to check for DefaultInstance
            let path = self.find_unit(&name)?;
            let unit = units::load_unit(&path)
                .await
                .map_err(|e| ManagerError::Parse(e.to_string()))?;

            // Check for DefaultInstance in the unit's Install section
            let default_instance = match &unit {
                units::Unit::Service(s) => s.install.default_instance.clone(),
                units::Unit::Socket(s) => s.install.default_instance.clone(),
                units::Unit::Timer(t) => t.install.default_instance.clone(),
                _ => None,
            };

            if let Some(instance) = default_instance {
                // Substitute the default instance into the template name
                if let Some(instantiated) = units::instantiate_template(&name, &instance) {
                    log::debug!(
                        "Template {} has DefaultInstance={}, loading {}",
                        name,
                        instance,
                        instantiated
                    );
                    name = instantiated;
                    // Continue to load the instantiated unit below
                }
            } else {
                // No DefaultInstance - store the template as-is
                let stored_name = name.clone();
                self.states.insert(name.clone(), ServiceState::new());
                self.units.insert(name, unit);
                return Ok(stored_name);
            }
        }

        // Already loaded?
        if self.units.contains_key(&name) {
            return Ok(name);
        }

        // Find the unit file
        let path = self.find_unit(&name)?;

        // Resolve symlinks to get canonical name (e.g., dbus.service -> dbus-broker.service)
        // Also detect masked units (symlinks to /dev/null)
        let canonical_name = if path.is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                // Check if unit is masked (symlink to /dev/null)
                if target.as_os_str() == "/dev/null" {
                    log::debug!("{} is masked, skipping", name);
                    return Err(ManagerError::Masked(name));
                }
                // Get just the filename from the symlink target
                let target_name = target
                    .file_name()
                    .and_then(|f| f.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or(name.clone());

                // If the original name has an instance and the target is a template,
                // substitute the instance into the target.
                // e.g., autovt@tty2.service -> getty@.service becomes getty@tty2.service
                if let Some(instance) = units::extract_instance(&name) {
                    if units::is_bare_template(&target_name) {
                        units::instantiate_template(&target_name, &instance)
                            .unwrap_or(target_name)
                    } else {
                        target_name
                    }
                } else {
                    target_name
                }
            } else {
                name.clone()
            }
        } else {
            name.clone()
        };

        // Check if the resolved name is already loaded
        if self.units.contains_key(&canonical_name) {
            return Ok(canonical_name);
        }

        // Parse it
        let mut unit = units::load_unit(&path)
            .await
            .map_err(|e| ManagerError::Parse(e.to_string()))?;

        // If we loaded a template file for an instantiated unit (e.g., modprobe@drm.service
        // from modprobe@.service), update the unit's name to the requested instantiated name.
        // This ensures the instance specifier (%i) gets properly substituted.
        if unit.name() != canonical_name {
            log::debug!(
                "Updating unit name from {} to {} (template instantiation)",
                unit.name(),
                canonical_name
            );
            unit.set_name(canonical_name.clone());
        }

        // Initialize state under the canonical name
        self.states.insert(canonical_name.clone(), ServiceState::new());
        self.units.insert(canonical_name.clone(), unit);

        Ok(canonical_name)
    }

    /// Load a unit from a specific path
    pub async fn load_from_path(&mut self, path: &std::path::Path) -> Result<(), ManagerError> {
        let unit = units::load_unit(path)
            .await
            .map_err(|e| ManagerError::Parse(e.to_string()))?;

        let name = unit.name().to_string();
        self.states.insert(name.clone(), ServiceState::new());
        self.units.insert(name, unit);

        Ok(())
    }

    /// Find a unit file in search paths
    fn find_unit(&self, name: &str) -> Result<PathBuf, ManagerError> {
        // First, try to find exact match
        for base in &self.unit_paths {
            let path = base.join(name);
            if path.exists() {
                return Ok(path);
            }
            // Also follow symlinks
            if path.is_symlink() {
                if let Ok(target) = std::fs::read_link(&path) {
                    if target.exists() {
                        return Ok(path);
                    }
                }
            }
        }

        // For template instances (foo@bar.service), try the template file (foo@.service)
        if let Some(template_name) = units::get_template_name(name) {
            for base in &self.unit_paths {
                let path = base.join(&template_name);
                if path.exists() {
                    return Ok(path);
                }
                if path.is_symlink() {
                    if let Ok(target) = std::fs::read_link(&path) {
                        if target.exists() {
                            return Ok(path);
                        }
                    }
                }
            }
        }

        Err(ManagerError::NotFound(name.to_string()))
    }

    /// Start a single service (no dependency resolution)
    pub async fn start(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);
        match self.start_single(&name).await {
            Ok(()) => Ok(()),
            Err(ManagerError::IsTarget(_)) => {
                // Targets are synchronization points - just mark as active
                if let Some(state) = self.states.get_mut(&name) {
                    state.set_running(0);
                }
                log::debug!("Target {} reached", name);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Start a unit with all its dependencies
    pub async fn start_with_deps(&mut self, name: &str) -> Result<Vec<String>, ManagerError> {
        let name = self.normalize_name(name);

        // Load the target unit and discover all dependencies
        let order = self.resolve_start_order(&name).await?;

        log::info!("Start order for {}: {:?}", name, order);

        // Start units in order
        let mut started = Vec::new();
        for unit_name in &order {
            // Skip if already running
            if let Some(state) = self.states.get(unit_name) {
                if state.is_active() {
                    log::debug!("{} already running, skipping", unit_name);
                    continue;
                }
            }

            match self.start_single(unit_name).await {
                Ok(()) => {
                    started.push(unit_name.clone());
                }
                Err(ManagerError::IsTarget(_)) => {
                    // Targets don't need to be started, just mark as active
                    if let Some(state) = self.states.get_mut(unit_name) {
                        state.set_running(0);
                    }
                    log::debug!("Target {} reached", unit_name);
                }
                Err(e) => {
                    // Check if this is a hard dependency (Requires)
                    let is_required = self
                        .units
                        .get(&name)
                        .map(|u| u.unit_section().requires.contains(unit_name))
                        .unwrap_or(false);

                    if is_required {
                        log::error!("Required dependency {} failed: {}", unit_name, e);
                        return Err(e);
                    } else {
                        // Soft dependency (Wants) - log and continue
                        log::warn!("Optional dependency {} failed: {}", unit_name, e);
                    }
                }
            }
        }

        Ok(started)
    }

    /// Resolve start order for a unit and its dependencies
    async fn resolve_start_order(&mut self, name: &str) -> Result<Vec<String>, ManagerError> {
        // Load the target unit first
        if !self.units.contains_key(name) {
            let _ = self.load(name).await?;
        }

        // Collect all dependencies transitively
        // Also track symlink aliases (requested name -> canonical name)
        let mut to_load: Vec<String> = vec![name.to_string()];
        let mut loaded: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut aliases: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        while let Some(unit_name) = to_load.pop() {
            if loaded.contains(&unit_name) || aliases.contains_key(&unit_name) {
                continue;
            }

            // Try to load the unit (may resolve symlinks to a different name)
            let actual_name = if self.units.contains_key(&unit_name) {
                unit_name.clone()
            } else {
                match self.load(&unit_name).await {
                    Ok(canonical) => canonical,
                    Err(e) => {
                        log::warn!("Could not load dependency {}: {}", unit_name, e);
                        continue;
                    }
                }
            };

            // Track alias if symlink was resolved to a different name
            if unit_name != actual_name {
                aliases.insert(unit_name.clone(), actual_name.clone());
            }

            loaded.insert(actual_name.clone());

            // Queue its dependencies (only Requires/Wants pull units, not After)
            // After= is ONLY an ordering constraint, not a dependency
            if let Some(unit) = self.units.get(&actual_name) {
                let section = unit.unit_section();
                // Debug: log dependencies being found
                if !section.requires.is_empty() || !section.wants.is_empty() || !unit.wants_dir().is_empty() {
                    log::debug!(
                        "{}: Requires={:?}, Wants={:?}, wants_dir={:?}",
                        actual_name, section.requires, section.wants, unit.wants_dir()
                    );
                }
                // Requires= pulls units (hard dependency - fail if missing)
                for dep in &section.requires {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
                // Wants= pulls units (soft dependency - continue if missing)
                for dep in &section.wants {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
                // .wants directory entries for targets
                for dep in unit.wants_dir() {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
            }
        }

        // Build dependency graph from loaded units
        let mut graph = deps::DepGraph::new();

        // Register all symlink aliases so edges resolve to canonical names
        for (alias, canonical) in &aliases {
            graph.add_alias(alias, canonical);
        }

        // Pre-register all loaded units as nodes first
        // This ensures add_edge only creates edges to actually-loaded units
        // Use the HashMap key (instance name) not unit.name() (which may be template name)
        for (key, _unit) in &self.units {
            if loaded.contains(key) {
                graph.add_node(key);
            }
        }

        // Now add units (which adds edges to existing nodes only)
        for (key, unit) in &self.units {
            if loaded.contains(key) {
                graph.add_unit_with_name(key, unit);
            }
        }

        // Get topological order
        graph
            .start_order_for(name)
            .map_err(|e| ManagerError::Cycle(e.nodes))
    }

    /// Start a single unit (internal, assumes already loaded)
    async fn start_single(&mut self, name: &str) -> Result<(), ManagerError> {
        log::debug!("start_single({})", name);
        // Debug: log when dbus-related units start
        if name.contains("dbus") {
            log::info!(">>> start_single({}) - socket_fds keys: {:?}", name,
                self.socket_fds.keys().collect::<Vec<_>>());
        }

        // Load if not already loaded (may resolve symlink to different name)
        let actual_name = if self.units.contains_key(name) {
            name.to_string()
        } else {
            self.load(name).await?
        };

        let unit = self
            .units
            .get(&actual_name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        // Targets are synchronization points, no process to start
        if unit.is_target() {
            return Err(ManagerError::IsTarget(actual_name.to_string()));
        }

        // Handle slice units (create cgroup dir and mark active)
        if let Some(slice) = unit.as_slice().cloned() {
            return self.start_slice(&actual_name, &slice).await;
        }

        // Handle socket units (create listening sockets)
        if let Some(socket) = unit.as_socket().cloned() {
            return self.start_socket(&actual_name, &socket).await;
        }

        // Handle timer units (schedule service activation)
        if let Some(timer) = unit.as_timer().cloned() {
            return self.start_timer(&actual_name, &timer).await;
        }

        // Handle path units (set up filesystem watches)
        if let Some(path_unit) = unit.as_path().cloned() {
            return self.start_path(&actual_name, &path_unit).await;
        }

        // Check conditions before starting
        if let Some(reason) = self.check_conditions(unit) {
            log::info!("{}: condition failed: {}", actual_name, reason);
            return Err(ManagerError::ConditionFailed(actual_name.to_string(), reason));
        }

        // Handle mount units
        if let Some(mnt) = unit.as_mount().cloned() {
            return self.start_mount(&actual_name, &mnt).await;
        }

        let service = unit
            .as_service()
            .ok_or_else(|| ManagerError::NotFound(actual_name.to_string()))?;

        let state = self
            .states
            .get_mut(&actual_name)
            .ok_or_else(|| ManagerError::NotFound(actual_name.to_string()))?;

        // Check if already running
        if state.is_active() {
            return Err(ManagerError::AlreadyActive(actual_name.to_string()));
        }

        // Update state to starting
        state.set_starting();
        self.active_jobs += 1;

        // Type=idle: wait for job queue to be empty (or timeout)
        let is_idle = service.service.service_type == ServiceType::Idle;
        if is_idle {
            // Wait up to 5 seconds for other jobs to complete
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while self.active_jobs > 1 && std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            if self.active_jobs > 1 {
                log::debug!("{}: idle timeout, proceeding anyway", actual_name);
            }
        }

        // Prepare spawn options
        let is_notify = service.service.service_type == ServiceType::Notify;
        let watchdog_usec = service.service.watchdog_sec.map(|d| d.as_micros() as u64);
        let socket_fds = self.get_socket_fds(&service.name);
        let socket_fd_names = self.get_socket_fd_names(&service.name);

        // DEBUG: Log socket FDs being passed
        if !socket_fds.is_empty() {
            log::info!("{}: passing socket FDs {:?} names {:?}", actual_name, socket_fds, socket_fd_names);
        } else if !service.service.sockets.is_empty() {
            log::warn!("{}: has Sockets={:?} but got NO socket FDs! socket_fds keys: {:?}",
                actual_name, service.service.sockets, self.socket_fds.keys().collect::<Vec<_>>());
        }

        // M19: DynamicUser= - allocate ephemeral UID/GID
        let (dynamic_uid, dynamic_gid) = if service.service.dynamic_user {
            match self.dynamic_user_manager.allocate(&actual_name) {
                Ok((uid, gid)) => {
                    self.dynamic_uids.insert(actual_name.to_string(), uid);
                    log::info!("Allocated dynamic UID/GID {} for {}", uid, actual_name);
                    (Some(uid), Some(gid))
                }
                Err(e) => {
                    log::error!("Failed to allocate dynamic user for {}: {}", actual_name, e);
                    return Err(ManagerError::StartFailed(e.to_string()));
                }
            }
        } else {
            (None, None)
        };

        // M19: Get stored FDs for restart (FileDescriptorStoreMax=)
        let stored_fds: Vec<RawFd> = self
            .fd_store
            .get(&actual_name)
            .map(|fds| fds.iter().map(|(_, fd)| *fd).collect())
            .unwrap_or_default();

        let options = SpawnOptions {
            notify_socket: if is_notify || watchdog_usec.is_some() {
                self.notify_socket_path()
                    .map(|p| p.to_string_lossy().to_string())
            } else {
                None
            },
            watchdog_usec,
            socket_fds,
            socket_fd_names,
            dynamic_uid,
            dynamic_gid,
            stored_fds,
            user_environment: self.user_environment.clone(),
        };

        // Debug: log NOTIFY_SOCKET for Type=notify services
        if is_notify {
            log::debug!(
                "{}: Type=notify, NOTIFY_SOCKET={:?}",
                actual_name,
                options.notify_socket
            );
        }

        // Type=oneshot services run all ExecStart commands in sequence
        // Commands are run asynchronously to avoid blocking socket activation
        if service.service.service_type == ServiceType::Oneshot {
            let num_commands = service.service.exec_start.len();
            log::info!("Starting oneshot {} ({} command{})", actual_name, num_commands,
                       if num_commands == 1 { "" } else { "s" });

            // Spawn first command
            let child = process::spawn_service_via_executor(service, &options, &self.executor_path, 0)?;
            log::debug!("{}: spawn returned, getting PID", actual_name);
            let pid = child.id().unwrap_or(0);
            log::debug!("{}: PID is {}", actual_name, pid);

            // Set up cgroup for the process
            let limits = CgroupLimits {
                memory_max: service.service.memory_max,
                cpu_quota: service.service.cpu_quota,
                tasks_max: service.service.tasks_max,
            };
            let slice = service.service.slice.as_deref();
            let delegate = service.service.delegate;

            if let Some(ref cgroup_mgr) = self.cgroup_manager {
                match cgroup_mgr.setup_service_cgroup(&actual_name, pid, &limits, slice) {
                    Ok(cgroup_path) => {
                        log::debug!("Created cgroup {} for {}", cgroup_path.display(), actual_name);
                        log::info!("Created cgroup: {}", cgroup_path.display());
                        if delegate {
                            if let Err(e) = cgroup_mgr.enable_delegation(&cgroup_path) {
                                log::warn!("Failed to enable cgroup delegation for {}: {}", actual_name, e);
                            }
                        }
                        self.cgroup_paths.insert(actual_name.to_string(), cgroup_path);
                    }
                    Err(e) => {
                        log::warn!("Failed to set up cgroup for {}: {}", actual_name, e);
                    }
                }
            }

            log::info!("Started {} (PID {})", actual_name, pid);

            // Spawn async task to wait for command completion
            // This allows the manager lock to be released so socket activation can proceed
            let service_name_clone = actual_name.clone();
            let total_cmds = num_commands;
            let remain_after_exit = service.service.remain_after_exit;
            let tx = self.oneshot_completion_tx.clone();

            tokio::spawn(async move {
                let result = child.wait_with_output().await;
                let (exit_code, error) = match result {
                    Ok(output) => {
                        let code = output.status.code().unwrap_or(-1);
                        if code == 0 {
                            (Some(0), None)
                        } else {
                            (Some(code), Some(format!("exit code {}", code)))
                        }
                    }
                    Err(e) => (None, Some(e.to_string())),
                };

                let _ = tx
                    .send(OneshotCompletion {
                        service_name: service_name_clone,
                        cmd_idx: 0,
                        total_cmds,
                        exit_code,
                        error,
                        remain_after_exit,
                    })
                    .await;
            });

            // Service is now activating - completion will be handled by background task
            return Ok(());
        }

        // Non-oneshot services: spawn single process (first ExecStart command)
        let child = process::spawn_service_via_executor(service, &options, &self.executor_path, 0)?;
        log::debug!("{}: spawn returned, getting PID", actual_name);
        let pid = child.id().unwrap_or(0);
        log::debug!("{}: PID is {}", actual_name, pid);

        // Set up cgroup for the process (if available)
        // Note: DeviceAllow/DevicePolicy is handled via mount namespace in sandbox.rs
        let limits = CgroupLimits {
            memory_max: service.service.memory_max,
            cpu_quota: service.service.cpu_quota,
            tasks_max: service.service.tasks_max,
        };
        let has_resource_limits = limits.memory_max.is_some()
            || limits.cpu_quota.is_some()
            || limits.tasks_max.is_some();

        // M18: Slice= - explicit cgroup slice placement
        let slice = service.service.slice.as_deref();

        // M19: Delegate= - allow service to manage own cgroup subtree
        let delegate = service.service.delegate;

        if let Some(ref cgroup_mgr) = self.cgroup_manager {
            match cgroup_mgr.setup_service_cgroup(&actual_name, pid, &limits, slice) {
                Ok(cgroup_path) => {
                    log::debug!("Created cgroup {} for {}", cgroup_path.display(), actual_name);

                    // M19: Enable delegation if requested
                    if delegate {
                        if let Err(e) = cgroup_mgr.enable_delegation(&cgroup_path) {
                            log::warn!("Failed to enable cgroup delegation for {}: {}", actual_name, e);
                        }
                    }

                    self.cgroup_paths.insert(actual_name.to_string(), cgroup_path);
                }
                Err(e) => {
                    if has_resource_limits {
                        log::error!(
                            "Failed to set up cgroup for {} (resource limits NOT enforced): {}",
                            actual_name,
                            e
                        );
                    } else {
                        log::warn!("Failed to set up cgroup for {}: {}", actual_name, e);
                    }
                }
            }
        } else if has_resource_limits {
            log::error!(
                "Service {} requests resource limits but cgroups unavailable - limits NOT enforced",
                actual_name
            );
        }

        // Store the child process and track PID -> service mapping
        self.processes.insert(actual_name.to_string(), child);
        self.pid_to_service.insert(pid, actual_name.to_string());

        let is_forking = service.service.service_type == ServiceType::Forking;
        let is_dbus = service.service.service_type == ServiceType::Dbus;
        let pid_file = service.service.pid_file.clone();
        let bus_name = service.service.bus_name.clone();

        if is_notify {
            // Type=notify: stay in starting state until READY=1 received
            self.waiting_ready.insert(pid, actual_name.to_string());
            log::info!("Started {} (PID {}), waiting for READY", actual_name, pid);
        } else if is_dbus {
            // Type=dbus: wait for BusName to appear on D-Bus
            if let Some(ref bn) = bus_name {
                self.waiting_bus_name.insert(bn.clone(), actual_name.to_string());
                log::info!(
                    "Started {} (PID {}), waiting for D-Bus name {}",
                    actual_name,
                    pid,
                    bn
                );
            } else {
                // No BusName specified - treat like simple
                log::warn!(
                    "{} is Type=dbus but has no BusName=, treating as simple",
                    actual_name
                );
                if let Some(state) = self.states.get_mut(&actual_name) {
                    state.set_running(pid);
                }
                self.active_jobs = self.active_jobs.saturating_sub(1);
                log::info!("Started {} (PID {})", actual_name, pid);
            }
        } else if is_forking {
            // Type=forking: wait for parent to exit, then read PIDFile
            log::info!("Started {} (PID {}), waiting for fork", actual_name, pid);
            // Store PIDFile path for later use in reap()
            if let Some(pf) = pid_file {
                log::debug!("{} will read PID from {}", actual_name, pf.display());
                self.pid_files.insert(actual_name.to_string(), pf);
            }
        } else {
            // Type=simple/idle: immediately mark as running
            if let Some(state) = self.states.get_mut(&actual_name) {
                state.set_running(pid);
            }
            self.active_jobs = self.active_jobs.saturating_sub(1);
            // Set watchdog deadline if configured
            if let Some(wd) = service.service.watchdog_sec {
                self.watchdog_deadlines
                    .insert(actual_name.to_string(), std::time::Instant::now() + wd);
            }
            log::info!("Started {} (PID {})", actual_name, pid);
        }

        Ok(())
    }

    /// Stop a service
    pub async fn stop(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);

        // Handle mount units
        if let Some(mount) = self.units.get(&name).and_then(|u| u.as_mount()).cloned() {
            return self.stop_mount(&name, &mount).await;
        }

        // Handle slice units
        if let Some(slice) = self.units.get(&name).and_then(|u| u.as_slice()).cloned() {
            return self.stop_slice(&name, &slice).await;
        }

        // Handle socket units
        if let Some(socket) = self.units.get(&name).and_then(|u| u.as_socket()).cloned() {
            return self.stop_socket(&name, &socket).await;
        }

        // Handle timer units
        if self.units.get(&name).is_some_and(|u| u.is_timer()) {
            return self.stop_timer(&name).await;
        }

        // Handle path units
        if self.units.get(&name).is_some_and(|u| u.is_path()) {
            return self.stop_path(&name).await;
        }

        let state = self
            .states
            .get_mut(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name));
        }

        state.set_stopping();

        // Get kill mode and send_sighup from service config
        let (kill_mode, send_sighup) = self
            .units
            .get(&name)
            .and_then(|u| u.as_service())
            .map(|s| (s.service.kill_mode.clone(), s.service.send_sighup))
            .unwrap_or((KillMode::default(), false));

        // Get the child process
        if let Some(mut child) = self.processes.remove(&name) {
            if let Some(pid) = child.id() {
                log::info!("Stopping {} (PID {}, KillMode={:?})", name, pid, kill_mode);

                // M18: SendSIGHUP= - send SIGHUP before SIGTERM
                if send_sighup {
                    log::debug!("Sending SIGHUP to {} (PID {})", name, pid);
                    unsafe {
                        libc::kill(pid as i32, libc::SIGHUP);
                    }
                }

                match kill_mode {
                    KillMode::None => {
                        // Don't send any signals, just wait (or timeout)
                    }
                    KillMode::Process => {
                        // Only kill the main process
                        unsafe {
                            libc::kill(pid as i32, libc::SIGTERM);
                        }
                    }
                    KillMode::Mixed | KillMode::ControlGroup => {
                        // SIGTERM to main process first
                        unsafe {
                            libc::kill(pid as i32, libc::SIGTERM);
                        }
                        // For cgroup killing, we'd also send to all cgroup members
                        // This requires cgroup iteration which we'll skip for now
                    }
                }
            }

            // Wait for exit (with timeout)
            let timeout_sec = self
                .units
                .get(&name)
                .and_then(|u| u.as_service())
                .and_then(|s| s.service.timeout_stop_sec)
                .unwrap_or(std::time::Duration::from_secs(10));

            match tokio::time::timeout(timeout_sec, child.wait()).await {
                Ok(Ok(status)) => {
                    let code = status.code().unwrap_or(-1);
                    if let Some(state) = self.states.get_mut(&name) {
                        state.set_stopped(code);
                    }
                    log::info!("Stopped {} (exit code {})", name, code);
                }
                Ok(Err(e)) => {
                    if let Some(state) = self.states.get_mut(&name) {
                        state.set_failed(e.to_string());
                    }
                }
                Err(_) => {
                    // Timeout - send SIGKILL
                    log::warn!("Timeout stopping {}, sending SIGKILL", name);
                    if let Some(pid) = child.id() {
                        unsafe {
                            libc::kill(pid as i32, libc::SIGKILL);
                        }
                    }
                    let _ = child.wait().await;
                    if let Some(state) = self.states.get_mut(&name) {
                        state.set_stopped(-9);
                    }
                }
            }
        } else {
            if let Some(state) = self.states.get_mut(&name) {
                state.set_stopped(0);
            }
        }

        // Clean up cgroup (if we created one)
        if self.cgroup_paths.remove(&name).is_some() {
            // Get slice from service config for cleanup
            let slice = self
                .units
                .get(&name)
                .and_then(|u| u.as_service())
                .and_then(|s| s.service.slice.as_deref());
            if let Some(ref cgroup_mgr) = self.cgroup_manager {
                if let Err(e) = cgroup_mgr.cleanup_service_cgroup(&name, slice) {
                    log::debug!("Failed to clean up cgroup for {}: {}", name, e);
                }
            }
        }

        // Clean up runtime directories (M17: RuntimeDirectoryPreserve)
        if let Some(service) = self.units.get(&name) {
            if let crate::units::Unit::Service(svc) = service {
                use crate::units::RuntimeDirectoryPreserve;
                match svc.service.runtime_directory_preserve {
                    RuntimeDirectoryPreserve::No => {
                        // Remove runtime directories
                        cleanup_runtime_directories(&svc.service, &name);
                    }
                    RuntimeDirectoryPreserve::Yes => {
                        // Keep directories
                    }
                    RuntimeDirectoryPreserve::Restart => {
                        // Keep only during restart - for now treat as No since we can't
                        // easily distinguish stop from restart at this point
                        // TODO: Add restart tracking to properly implement this
                        cleanup_runtime_directories(&svc.service, &name);
                    }
                }
            }
        }

        // Clean up watchdog
        self.watchdog_deadlines.remove(&name);

        // M19: Release dynamic UID if allocated
        if let Some(uid) = self.dynamic_uids.remove(&name) {
            self.dynamic_user_manager.release(uid);
            log::debug!("Released dynamic UID {} for {}", uid, name);
        }

        // M19: Close and remove stored FDs
        if let Some(fds) = self.fd_store.remove(&name) {
            for (fd_name, fd) in fds {
                log::debug!("Closing stored FD {} ('{}') for {}", fd, fd_name, name);
                unsafe { libc::close(fd) };
            }
        }

        // M18: ExecStopPost= - run post-stop commands
        if let Some(service) = self.units.get(&name) {
            if let crate::units::Unit::Service(svc) = service {
                for cmd_line in &svc.service.exec_stop_post {
                    log::debug!("Running ExecStopPost for {}: {}", name, cmd_line);
                    if let Err(e) = run_simple_command(cmd_line).await {
                        log::warn!("ExecStopPost failed for {}: {}", name, e);
                        // Continue with other commands even if one fails
                    }
                }
            }
        }

        Ok(())
    }

    /// Restart a service (stop then start)
    pub async fn restart(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);

        // Stop if running (ignore NotActive error)
        match self.stop(&name).await {
            Ok(()) => {}
            Err(ManagerError::NotActive(_)) => {}
            Err(e) => return Err(e),
        }

        // Start
        self.start(&name).await
    }

    /// Get service status
    pub fn status(&self, name: &str) -> Option<&ServiceState> {
        let name = self.normalize_name(name);
        self.states.get(&name)
    }

    /// Get service definition
    pub fn get_service(&self, name: &str) -> Option<&Service> {
        let name = self.normalize_name(name);
        self.units.get(&name).and_then(|u| u.as_service())
    }

    /// Get unit definition
    pub fn get_unit(&self, name: &str) -> Option<&Unit> {
        let name = self.normalize_name(name);
        self.units.get(&name)
    }

    /// List loaded units
    pub fn list(&self) -> impl Iterator<Item = (&String, &ServiceState)> {
        self.states.iter()
    }

    /// List all loaded units with their types and states
    pub fn list_units(&self) -> Vec<(&String, &Unit, Option<&ServiceState>)> {
        self.units
            .iter()
            .map(|(name, unit)| (name, unit, self.states.get(name)))
            .collect()
    }

    /// Set the D-Bus connection for scope registration
    pub fn set_dbus_connection(&mut self, conn: zbus::Connection) {
        self.scope_manager.set_dbus_connection(conn);
    }

    /// Get the cgroup manager
    pub fn cgroup_manager(&self) -> Option<&CgroupManager> {
        self.cgroup_manager.as_ref()
    }

    /// Get the scope manager
    pub fn scope_manager(&self) -> &ScopeManager {
        &self.scope_manager
    }

    /// Get the scope manager mutably
    pub fn scope_manager_mut(&mut self) -> &mut ScopeManager {
        &mut self.scope_manager
    }

    /// Register a transient scope (called by D-Bus StartTransientUnit)
    pub async fn register_scope(
        &mut self,
        name: &str,
        slice: Option<&str>,
        description: Option<&str>,
        pids: &[u32],
    ) -> Result<PathBuf, ManagerError> {
        let path = self.scope_manager.register(name, slice, description, pids).await?;
        // Track in states for list/status queries
        self.states.insert(name.to_string(), ServiceState::running_scope());
        Ok(path)
    }

    /// Unregister a scope (called when scope is abandoned or empty)
    pub async fn unregister_scope(&mut self, name: &str) -> Result<(), ManagerError> {
        self.states.remove(name);
        self.scope_manager.unregister(name).await
    }

    /// Normalize unit name (add .service suffix if no suffix present)
    fn normalize_name(&self, name: &str) -> String {
        if name.ends_with(".service")
            || name.ends_with(".target")
            || name.ends_with(".mount")
            || name.ends_with(".socket")
            || name.ends_with(".path")
            || name.ends_with(".slice")
            || name.ends_with(".timer")
        {
            name.to_string()
        } else {
            format!("{}.service", name)
        }
    }

    /// M20: Get boot plan without starting (for dry-run)
    pub async fn get_boot_plan(&mut self, target: &str) -> Result<Vec<String>, ManagerError> {
        let name = self.normalize_name(target);
        self.resolve_start_order(&name).await
    }

    /// M20: Reload all unit files from disk
    pub async fn reload_units(&mut self) -> Result<usize, ManagerError> {
        let unit_names: Vec<String> = self.units.keys().cloned().collect();
        let mut reloaded = 0;

        for name in unit_names {
            // Skip scopes (transient, not from files)
            if name.ends_with(".scope") {
                continue;
            }

            // Find the unit file
            let path = match self.find_unit(&name) {
                Ok(p) => p,
                Err(_) => {
                    log::debug!("Unit {} no longer exists on disk, keeping in memory", name);
                    continue;
                }
            };

            // Re-parse it
            match units::load_unit(&path).await {
                Ok(new_unit) => {
                    self.units.insert(name.clone(), new_unit);
                    reloaded += 1;
                    log::debug!("Reloaded {}", name);
                }
                Err(e) => {
                    log::warn!("Failed to reload {}: {}", name, e);
                }
            }
        }

        log::info!("Reloaded {} unit files", reloaded);
        Ok(reloaded)
    }

    /// M20: Sync units - reload files and restart changed services
    pub async fn sync_units(&mut self) -> Result<Vec<String>, ManagerError> {
        // Snapshot current unit definitions (hash of key fields)
        let old_hashes: std::collections::HashMap<String, u64> = self
            .units
            .iter()
            .filter_map(|(name, unit)| {
                if let Some(svc) = unit.as_service() {
                    // Hash relevant fields that would require restart
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    svc.service.exec_start.hash(&mut hasher);
                    svc.service.environment.hash(&mut hasher);
                    svc.service.environment_file.hash(&mut hasher);
                    svc.service.working_directory.hash(&mut hasher);
                    svc.service.user.hash(&mut hasher);
                    svc.service.group.hash(&mut hasher);
                    Some((name.clone(), hasher.finish()))
                } else {
                    None
                }
            })
            .collect();

        // Reload all units
        self.reload_units().await?;

        // Find changed services
        let mut restarted = Vec::new();
        for (name, old_hash) in old_hashes {
            if let Some(unit) = self.units.get(&name) {
                if let Some(svc) = unit.as_service() {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    svc.service.exec_start.hash(&mut hasher);
                    svc.service.environment.hash(&mut hasher);
                    svc.service.environment_file.hash(&mut hasher);
                    svc.service.working_directory.hash(&mut hasher);
                    svc.service.user.hash(&mut hasher);
                    svc.service.group.hash(&mut hasher);
                    let new_hash = hasher.finish();

                    if new_hash != old_hash {
                        // Config changed - restart if running
                        if let Some(state) = self.states.get(&name) {
                            if state.is_active() {
                                log::info!("{} config changed, restarting", name);
                                if let Err(e) = self.restart(&name).await {
                                    log::warn!("Failed to restart {}: {}", name, e);
                                } else {
                                    restarted.push(name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(restarted)
    }

    /// M20: Switch to target, stopping units not in its dependency tree
    pub async fn switch_target(&mut self, target: &str) -> Result<Vec<String>, ManagerError> {
        let target = self.normalize_name(target);

        // Get all units needed by the target
        let needed: std::collections::HashSet<String> =
            self.resolve_start_order(&target).await?.into_iter().collect();

        // Find running units not in the needed set
        let to_stop: Vec<String> = self
            .states
            .iter()
            .filter(|(name, state)| state.is_active() && !needed.contains(*name))
            .map(|(name, _)| name.clone())
            .collect();

        // Stop unneeded units
        for name in &to_stop {
            log::info!("Stopping {} (not needed by {})", name, target);
            if let Err(e) = self.stop(name).await {
                log::warn!("Failed to stop {}: {}", name, e);
            }
        }

        // Start the target with dependencies
        self.start_with_deps(&target).await?;

        Ok(to_stop)
    }

    /// Get the default target (resolves default.target symlink)
    pub fn get_default_target(&self) -> Result<String, ManagerError> {
        // Look for default.target in unit paths
        for base in &self.unit_paths {
            let path = base.join("default.target");
            if path.exists() || path.is_symlink() {
                // Resolve the symlink to get the actual target
                if let Ok(resolved) = std::fs::read_link(&path) {
                    // Extract just the filename
                    if let Some(name) = resolved.file_name().and_then(|n| n.to_str()) {
                        return Ok(name.to_string());
                    }
                }
                // If not a symlink or can't resolve, use as-is
                return Ok("default.target".to_string());
            }
        }
        Err(ManagerError::NotFound("default.target".to_string()))
    }

    /// Import environment variables (for user session management)
    pub fn import_environment(&mut self, vars: Vec<(String, String)>) {
        for (key, value) in vars {
            self.user_environment.insert(key, value);
        }
        log::info!("Imported {} environment variables", self.user_environment.len());
    }

    /// Unset environment variables
    pub fn unset_environment(&mut self, names: &[String]) {
        for name in names {
            self.user_environment.remove(name);
        }
        log::info!("Unset {} environment variables", names.len());
    }

    /// Get imported environment variables (to be passed to spawned services)
    pub fn get_user_environment(&self) -> &HashMap<String, String> {
        &self.user_environment
    }

    /// Reset failed state of all units
    pub fn reset_failed(&mut self) {
        for (name, state) in self.states.iter_mut() {
            if state.active == ActiveState::Failed {
                log::info!("Resetting failed state of {}", name);
                state.active = ActiveState::Inactive;
                state.sub = SubState::Dead;
                state.error = None;
            }
        }
    }
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

/// M17: Clean up runtime directories when service stops
fn cleanup_runtime_directories(service: &crate::units::ServiceSection, service_name: &str) {
    let base_name = service_name
        .strip_suffix(".service")
        .unwrap_or(service_name);

    for name in &service.runtime_directory {
        let dir_name = if name.is_empty() { base_name } else { name.as_str() };
        let path = std::path::Path::new("/run").join(dir_name);
        if path.exists() {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                log::warn!("Failed to remove runtime directory {}: {}", path.display(), e);
            } else {
                log::debug!("Removed runtime directory: {}", path.display());
            }
        }
    }
}

/// Run a simple command (for ExecStopPost, etc.)
/// Parses the command line and runs it, waiting for completion
async fn run_simple_command(cmd_line: &str) -> Result<(), std::io::Error> {
    use tokio::process::Command;

    // Strip leading - (ignore errors) or + (run as root)
    let cmd_line = cmd_line
        .trim_start_matches('-')
        .trim_start_matches('+')
        .trim();

    // Split command line (simple split, doesn't handle quotes properly)
    let parts: Vec<&str> = cmd_line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(());
    }

    let program = parts[0];
    let args = &parts[1..];

    let status = Command::new(program).args(args).status().await?;

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Command exited with {}", status),
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("Unit not found: {0}")]
    NotFound(String),

    #[error("Failed to parse unit: {0}")]
    Parse(String),

    #[error("Unit already active: {0}")]
    AlreadyActive(String),

    #[error("Unit not active: {0}")]
    NotActive(String),

    #[error("Failed to spawn: {0}")]
    Spawn(#[from] SpawnError),

    #[error("Dependency cycle detected: {}", .0.join(" -> "))]
    Cycle(Vec<String>),

    #[error("Unit is a target (no process): {0}")]
    IsTarget(String),

    #[error("Condition failed for {0}: {1}")]
    ConditionFailed(String, String),

    #[error("Unit has no [Install] section: {0}")]
    NoInstallSection(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("Failed to start: {0}")]
    StartFailed(String),

    #[error("Failed to stop: {0}")]
    StopFailed(String),

    #[error("Unit is masked: {0}")]
    Masked(String),
}

impl From<std::io::Error> for ManagerError {
    fn from(e: std::io::Error) -> Self {
        ManagerError::Io(e.to_string())
    }
}
