//! Service manager
//!
//! Loads, starts, stops, and monitors services and targets.

mod deps;
mod notify;
mod process;
pub mod sandbox;
mod socket_watcher;
mod state;
mod timer_scheduler;

pub use deps::{CycleError, DepGraph};
pub use notify::{AsyncNotifyListener, NotifyMessage, NOTIFY_SOCKET_PATH};
pub use process::{SpawnError, SpawnOptions};
pub use sandbox::apply_sandbox;
pub use socket_watcher::SocketActivation;
pub use state::{ActiveState, ServiceState, SubState};
pub use timer_scheduler::TimerFired;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::PathBuf;
use tokio::process::Child;
use tokio::sync::mpsc;

use crate::cgroups::{CgroupLimits, CgroupManager};
use crate::units::{self, KillMode, ListenType, Mount, RestartPolicy, Service, ServiceType, Socket, Timer, Unit};

/// Detected virtualization type
#[derive(Debug, Clone, PartialEq)]
pub enum VirtualizationType {
    // Containers
    Docker,
    Podman,
    Lxc,
    Lxd,
    SystemdNspawn,
    Container, // Generic container

    // Virtual machines
    Qemu,
    VirtualBox,
    VMware,
    Xen,
    HyperV,
    Bochs,
    Vm, // Generic VM
}

impl VirtualizationType {
    /// Check if this is a container type
    pub fn is_container(&self) -> bool {
        matches!(
            self,
            Self::Docker
                | Self::Podman
                | Self::Lxc
                | Self::Lxd
                | Self::SystemdNspawn
                | Self::Container
        )
    }

    /// Check if this is a VM type
    pub fn is_vm(&self) -> bool {
        matches!(
            self,
            Self::Qemu
                | Self::VirtualBox
                | Self::VMware
                | Self::Xen
                | Self::HyperV
                | Self::Bochs
                | Self::Vm
        )
    }

    /// Check if this matches a specific type name
    pub fn matches(&self, name: &str) -> bool {
        let name_lower = name.to_lowercase();
        match self {
            Self::Docker => name_lower == "docker",
            Self::Podman => name_lower == "podman",
            Self::Lxc => name_lower == "lxc",
            Self::Lxd => name_lower == "lxd" || name_lower == "lxc-libvirt",
            Self::SystemdNspawn => name_lower == "systemd-nspawn",
            Self::Container => name_lower == "container",
            Self::Qemu => name_lower == "qemu" || name_lower == "kvm",
            Self::VirtualBox => name_lower == "oracle" || name_lower == "virtualbox",
            Self::VMware => name_lower == "vmware",
            Self::Xen => name_lower == "xen",
            Self::HyperV => name_lower == "microsoft" || name_lower == "hyper-v",
            Self::Bochs => name_lower == "bochs",
            Self::Vm => name_lower == "vm",
        }
    }

    /// Parse from container= environment variable
    pub fn from_container_env(val: &str) -> Self {
        match val.to_lowercase().as_str() {
            "docker" => Self::Docker,
            "podman" => Self::Podman,
            "lxc" => Self::Lxc,
            "lxd" | "lxc-libvirt" => Self::Lxd,
            "systemd-nspawn" => Self::SystemdNspawn,
            _ => Self::Container,
        }
    }
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
    /// Boot time for monotonic timer calculations
    boot_time: std::time::Instant,
}

impl Manager {
    /// Create a new service manager
    pub fn new() -> Self {
        // Try to initialize cgroup manager (may fail if not root or cgroups unavailable)
        let cgroup_manager = match CgroupManager::new() {
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
        };

        // Create socket activation channel
        let (socket_activation_tx, socket_activation_rx) = mpsc::channel(32);

        // Create timer fired channel
        let (timer_tx, timer_rx) = mpsc::channel(32);

        Self {
            units: HashMap::new(),
            states: HashMap::new(),
            processes: HashMap::new(),
            unit_paths: vec![
                PathBuf::from("/etc/systemd/system"),
                PathBuf::from("/usr/lib/systemd/system"),
            ],
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
            boot_time: std::time::Instant::now(),
        }
    }

    /// Check if cgroup management is available
    pub fn cgroups_available(&self) -> bool {
        self.cgroup_manager.is_some()
    }

    /// Initialize the notify socket listener
    pub fn init_notify_socket(&mut self) -> std::io::Result<()> {
        let (listener, rx) = AsyncNotifyListener::new(std::path::Path::new(NOTIFY_SOCKET_PATH))?;
        self.notify_listener = Some(listener);
        self.notify_rx = Some(rx);
        log::info!("Notify socket listening at {}", NOTIFY_SOCKET_PATH);
        Ok(())
    }

    /// Get the notify socket path (if initialized)
    pub fn notify_socket_path(&self) -> Option<&std::path::Path> {
        self.notify_listener.as_ref().map(|l| l.socket_path())
    }

    /// Load a unit (service or target) by name
    pub async fn load(&mut self, name: &str) -> Result<(), ManagerError> {
        // Normalize the name
        let name = self.normalize_name(name);

        // Already loaded?
        if self.units.contains_key(&name) {
            return Ok(());
        }

        // Find the unit file
        let path = self.find_unit(&name)?;

        // Parse it
        let unit = units::load_unit(&path)
            .await
            .map_err(|e| ManagerError::Parse(e.to_string()))?;

        // Initialize state
        self.states.insert(name.clone(), ServiceState::new());
        self.units.insert(name, unit);

        Ok(())
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
        self.start_single(&name).await
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
            self.load(name).await?;
        }

        // Collect all dependencies transitively
        let mut to_load: Vec<String> = vec![name.to_string()];
        let mut loaded: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(unit_name) = to_load.pop() {
            if loaded.contains(&unit_name) {
                continue;
            }

            // Try to load the unit
            if !self.units.contains_key(&unit_name) {
                if let Err(e) = self.load(&unit_name).await {
                    log::warn!("Could not load dependency {}: {}", unit_name, e);
                    // Skip missing dependencies
                    continue;
                }
            }

            loaded.insert(unit_name.clone());

            // Queue its dependencies
            if let Some(unit) = self.units.get(&unit_name) {
                let section = unit.unit_section();
                for dep in &section.after {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
                for dep in &section.requires {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
                for dep in &section.wants {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
                // Also include .wants directory entries for targets
                for dep in unit.wants_dir() {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
            }
        }

        // Build dependency graph from loaded units
        let mut graph = deps::DepGraph::new();
        for unit in self.units.values() {
            if loaded.contains(unit.name()) {
                graph.add_unit(unit);
            }
        }

        // Get topological order
        graph
            .start_order_for(name)
            .map_err(|e| ManagerError::Cycle(e.nodes))
    }

    /// Start a single unit (internal, assumes already loaded)
    async fn start_single(&mut self, name: &str) -> Result<(), ManagerError> {
        // Load if not already loaded
        if !self.units.contains_key(name) {
            self.load(name).await?;
        }

        let unit = self
            .units
            .get(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        // Targets are synchronization points, no process to start
        if unit.is_target() {
            return Err(ManagerError::IsTarget(name.to_string()));
        }

        // Handle slice units (create cgroup dir and mark active)
        if let Some(slice) = unit.as_slice().cloned() {
            return self.start_slice(name, &slice).await;
        }

        // Handle socket units (create listening sockets)
        if let Some(socket) = unit.as_socket().cloned() {
            return self.start_socket(name, &socket).await;
        }

        // Handle timer units (schedule service activation)
        if let Some(timer) = unit.as_timer().cloned() {
            return self.start_timer(name, &timer).await;
        }

        // Check conditions before starting
        if let Some(reason) = self.check_conditions(unit) {
            log::info!("{}: condition failed: {}", name, reason);
            return Err(ManagerError::ConditionFailed(name.to_string(), reason));
        }

        // Handle mount units
        if let Some(mnt) = unit.as_mount().cloned() {
            return self.start_mount(name, &mnt).await;
        }

        let service = unit
            .as_service()
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        // Check if already running
        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
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
                log::debug!("{}: idle timeout, proceeding anyway", name);
            }
        }

        // Prepare spawn options
        let is_notify = service.service.service_type == ServiceType::Notify;
        let watchdog_usec = service.service.watchdog_sec.map(|d| d.as_micros() as u64);
        let socket_fds = self.get_socket_fds(&service.name);
        let options = SpawnOptions {
            notify_socket: if is_notify || watchdog_usec.is_some() {
                self.notify_socket_path()
                    .map(|p| p.to_string_lossy().to_string())
            } else {
                None
            },
            watchdog_usec,
            socket_fds,
        };

        // Spawn the process
        let child = process::spawn_service_with_options(service, &options)?;
        let pid = child.id().unwrap_or(0);

        // Set up cgroup for the process (if available)
        let limits = CgroupLimits {
            memory_max: service.service.memory_max,
            cpu_quota: service.service.cpu_quota,
            tasks_max: service.service.tasks_max,
        };
        let has_resource_limits =
            limits.memory_max.is_some() || limits.cpu_quota.is_some() || limits.tasks_max.is_some();

        if let Some(ref cgroup_mgr) = self.cgroup_manager {
            match cgroup_mgr.setup_service_cgroup(name, pid, &limits) {
                Ok(cgroup_path) => {
                    log::debug!("Created cgroup {} for {}", cgroup_path.display(), name);
                    self.cgroup_paths.insert(name.to_string(), cgroup_path);
                }
                Err(e) => {
                    if has_resource_limits {
                        log::error!(
                            "Failed to set up cgroup for {} (resource limits NOT enforced): {}",
                            name,
                            e
                        );
                    } else {
                        log::warn!("Failed to set up cgroup for {}: {}", name, e);
                    }
                }
            }
        } else if has_resource_limits {
            log::error!(
                "Service {} requests resource limits but cgroups unavailable - limits NOT enforced",
                name
            );
        }

        // Store the child process
        self.processes.insert(name.to_string(), child);

        let is_forking = service.service.service_type == ServiceType::Forking;
        let is_dbus = service.service.service_type == ServiceType::Dbus;
        let pid_file = service.service.pid_file.clone();
        let bus_name = service.service.bus_name.clone();

        if is_notify {
            // Type=notify: stay in starting state until READY=1 received
            self.waiting_ready.insert(pid, name.to_string());
            log::info!("Started {} (PID {}), waiting for READY", name, pid);
        } else if is_dbus {
            // Type=dbus: wait for BusName to appear on D-Bus
            if let Some(ref bn) = bus_name {
                self.waiting_bus_name.insert(bn.clone(), name.to_string());
                log::info!(
                    "Started {} (PID {}), waiting for D-Bus name {}",
                    name,
                    pid,
                    bn
                );
            } else {
                // No BusName specified - treat like simple
                log::warn!(
                    "{} is Type=dbus but has no BusName=, treating as simple",
                    name
                );
                if let Some(state) = self.states.get_mut(name) {
                    state.set_running(pid);
                }
                self.active_jobs = self.active_jobs.saturating_sub(1);
                log::info!("Started {} (PID {})", name, pid);
            }
        } else if is_forking {
            // Type=forking: wait for parent to exit, then read PIDFile
            log::info!("Started {} (PID {}), waiting for fork", name, pid);
            // Store PIDFile path for later use in reap()
            if let Some(pf) = pid_file {
                log::debug!("{} will read PID from {}", name, pf.display());
                self.pid_files.insert(name.to_string(), pf);
            }
        } else {
            // Type=simple/idle: immediately mark as running
            if let Some(state) = self.states.get_mut(name) {
                state.set_running(pid);
            }
            self.active_jobs = self.active_jobs.saturating_sub(1);
            // Set watchdog deadline if configured
            if let Some(wd) = service.service.watchdog_sec {
                self.watchdog_deadlines
                    .insert(name.to_string(), std::time::Instant::now() + wd);
            }
            log::info!("Started {} (PID {})", name, pid);
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

        let state = self
            .states
            .get_mut(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name));
        }

        state.set_stopping();

        // Get kill mode from service config
        let kill_mode = self
            .units
            .get(&name)
            .and_then(|u| u.as_service())
            .map(|s| s.service.kill_mode.clone())
            .unwrap_or_default();

        // Get the child process
        if let Some(mut child) = self.processes.remove(&name) {
            if let Some(pid) = child.id() {
                log::info!("Stopping {} (PID {}, KillMode={:?})", name, pid, kill_mode);

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
            if let Some(ref cgroup_mgr) = self.cgroup_manager {
                if let Err(e) = cgroup_mgr.cleanup_service_cgroup(&name) {
                    log::debug!("Failed to clean up cgroup for {}: {}", name, e);
                }
            }
        }

        // Clean up watchdog
        self.watchdog_deadlines.remove(&name);

        Ok(())
    }

    /// Start a mount unit (execute mount operation)
    async fn start_mount(&mut self, name: &str, mnt: &Mount) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        // Check if already mounted
        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        let mount_point = &mnt.mount.r#where;
        let what = &mnt.mount.what;
        let fs_type = mnt.mount.fs_type.as_deref().unwrap_or("auto");
        let options = mnt.mount.options.as_deref().unwrap_or("defaults");

        // Create mount point directory if needed
        if let Some(mode) = mnt.mount.directory_mode {
            if !std::path::Path::new(mount_point).exists() {
                if let Err(e) = std::fs::create_dir_all(mount_point) {
                    log::warn!("Failed to create mount point {}: {}", mount_point, e);
                } else if let Err(e) =
                    std::fs::set_permissions(mount_point, std::fs::Permissions::from_mode(mode))
                {
                    log::warn!("Failed to set permissions on {}: {}", mount_point, e);
                }
            }
        }

        // Check if already mounted (via /proc/mounts)
        if is_mounted(mount_point) {
            log::info!("{} already mounted at {}", name, mount_point);
            if let Some(state) = self.states.get_mut(name) {
                state.set_running(0);
            }
            return Ok(());
        }

        // Execute mount
        log::info!(
            "Mounting {} ({}) at {} with options {}",
            name,
            what,
            mount_point,
            options
        );

        use nix::mount::{mount, MsFlags};

        // Parse options into MsFlags
        let mut flags = MsFlags::empty();
        let mut data_options = Vec::new();

        for opt in options.split(',') {
            match opt.trim() {
                "ro" | "read-only" => flags |= MsFlags::MS_RDONLY,
                "rw" => {} // default
                "nosuid" => flags |= MsFlags::MS_NOSUID,
                "nodev" => flags |= MsFlags::MS_NODEV,
                "noexec" => flags |= MsFlags::MS_NOEXEC,
                "noatime" => flags |= MsFlags::MS_NOATIME,
                "nodiratime" => flags |= MsFlags::MS_NODIRATIME,
                "relatime" => flags |= MsFlags::MS_RELATIME,
                "strictatime" => flags |= MsFlags::MS_STRICTATIME,
                "sync" => flags |= MsFlags::MS_SYNCHRONOUS,
                "dirsync" => flags |= MsFlags::MS_DIRSYNC,
                "silent" => flags |= MsFlags::MS_SILENT,
                "bind" => flags |= MsFlags::MS_BIND,
                "move" => flags |= MsFlags::MS_MOVE,
                "remount" => flags |= MsFlags::MS_REMOUNT,
                "defaults" => {} // no special flags
                other => {
                    // Pass as data option to filesystem
                    data_options.push(other);
                }
            }
        }

        let data = if data_options.is_empty() {
            None
        } else {
            Some(data_options.join(","))
        };

        let result = mount(
            Some(what.as_str()),
            mount_point.as_str(),
            Some(fs_type),
            flags,
            data.as_deref(),
        );

        match result {
            Ok(()) => {
                log::info!("{} mounted successfully", name);
                if let Some(state) = self.states.get_mut(name) {
                    state.set_running(0);
                }
                Ok(())
            }
            Err(e) => {
                let msg = format!("mount failed: {}", e);
                log::error!("{}: {}", name, msg);
                if let Some(state) = self.states.get_mut(name) {
                    state.set_failed(msg.clone());
                }
                Err(ManagerError::Io(msg))
            }
        }
    }

    /// Stop a mount unit (execute umount operation)
    async fn stop_mount(&mut self, name: &str, mnt: &Mount) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        let mount_point = &mnt.mount.r#where;

        // Check if actually mounted
        if !is_mounted(mount_point) {
            log::debug!("{} not mounted, marking inactive", name);
            if let Some(state) = self.states.get_mut(name) {
                state.set_stopped(0);
            }
            return Ok(());
        }

        log::info!("Unmounting {}", mount_point);

        use nix::mount::{umount2, MntFlags};

        let mut flags = MntFlags::empty();
        if mnt.mount.lazy_unmount {
            flags |= MntFlags::MNT_DETACH;
        }
        if mnt.mount.force_unmount {
            flags |= MntFlags::MNT_FORCE;
        }

        let result = umount2(mount_point.as_str(), flags);

        match result {
            Ok(()) => {
                log::info!("{} unmounted successfully", name);
                if let Some(state) = self.states.get_mut(name) {
                    state.set_stopped(0);
                }
                Ok(())
            }
            Err(e) => {
                let msg = format!("umount failed: {}", e);
                log::error!("{}: {}", name, msg);
                if let Some(state) = self.states.get_mut(name) {
                    state.set_failed(msg.clone());
                }
                Err(ManagerError::Io(msg))
            }
        }
    }

    /// Start a slice unit (create cgroup directory and mark active)
    async fn start_slice(
        &mut self,
        name: &str,
        slice: &crate::units::Slice,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        // Check if already active
        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        // Create cgroup directory for the slice
        let cgroup_path = slice.cgroup_path();
        log::info!("Starting slice {} (cgroup: {})", name, cgroup_path);

        if let Some(ref cgroup_mgr) = self.cgroup_manager {
            // Create the cgroup directory
            let path = std::path::Path::new(&cgroup_path);
            if !path.exists() {
                if let Err(e) = std::fs::create_dir_all(path) {
                    log::warn!("Failed to create cgroup dir {}: {}", cgroup_path, e);
                } else {
                    log::debug!("Created cgroup directory {}", cgroup_path);
                }
            }
            // Note: We don't need to move any processes - slices just organize the hierarchy
            let _ = cgroup_mgr; // silence unused warning
        }

        // Mark as active immediately (slices have no process)
        if let Some(state) = self.states.get_mut(name) {
            state.set_running(0);
        }

        log::info!("{} reached", name);
        Ok(())
    }

    /// Stop a slice unit (mark inactive, optionally clean up cgroup)
    async fn stop_slice(
        &mut self,
        name: &str,
        slice: &crate::units::Slice,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        let cgroup_path = slice.cgroup_path();
        log::info!("Stopping slice {} (cgroup: {})", name, cgroup_path);

        // Note: We don't remove cgroup directories on slice stop
        // The cgroup may still contain running services
        // Cleanup happens when the cgroup becomes empty

        if let Some(state) = self.states.get_mut(name) {
            state.set_stopped(0);
        }

        log::info!("{} stopped", name);
        Ok(())
    }

    /// Start a socket unit (create listening sockets)
    async fn start_socket(&mut self, name: &str, socket: &Socket) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        log::info!("Starting socket {}", name);

        let mut fds = Vec::new();

        for listener in &socket.socket.listeners {
            match self.create_listener(listener, socket) {
                Ok(fd) => {
                    log::debug!(
                        "{}: created {:?} listener on {} (fd {})",
                        name,
                        listener.listen_type,
                        listener.address,
                        fd
                    );
                    fds.push(fd);
                }
                Err(e) => {
                    // Close already created sockets on failure
                    for fd in fds {
                        unsafe { libc::close(fd) };
                    }
                    if let Some(state) = self.states.get_mut(name) {
                        state.set_failed(format!("listener creation failed: {}", e));
                    }
                    return Err(ManagerError::Io(format!(
                        "Failed to create listener {}: {}",
                        listener.address, e
                    )));
                }
            }
        }

        // Store the FDs
        self.socket_fds.insert(name.to_string(), fds.clone());

        // Spawn socket watcher task for activation
        let service_name = socket.service_name();
        let socket_name = name.to_string();
        let tx = self.socket_activation_tx.clone();
        tokio::spawn(async move {
            socket_watcher::watch_socket(socket_name, service_name, fds, tx).await;
        });

        // Mark as active
        if let Some(state) = self.states.get_mut(name) {
            state.set_running(0);
        }

        log::info!("{} listening", name);
        Ok(())
    }

    /// Create a single listener socket
    fn create_listener(
        &self,
        listener: &crate::units::Listener,
        socket: &Socket,
    ) -> std::io::Result<RawFd> {
        use std::os::unix::net::UnixListener;

        match listener.listen_type {
            ListenType::Stream => {
                // Check if it's a Unix socket (path) or TCP (port number)
                if listener.address.starts_with('/') || listener.address.starts_with('@') {
                    // Unix socket
                    let path = if listener.address.starts_with('@') {
                        // Abstract socket - use null byte prefix
                        format!("\0{}", &listener.address[1..])
                    } else {
                        listener.address.clone()
                    };

                    // Remove existing socket file
                    if !listener.address.starts_with('@') {
                        let _ = std::fs::remove_file(&listener.address);

                        // Create parent directory if needed
                        if let Some(parent) = std::path::Path::new(&listener.address).parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                    }

                    if listener.address.starts_with('@') {
                        // Abstract socket - need to use libc directly
                        self.create_abstract_unix_socket(&path)
                    } else {
                        // Filesystem socket
                        let unix_listener = UnixListener::bind(&listener.address)?;

                        // Set socket mode if specified
                        if let Some(mode) = socket.socket.socket_mode {
                            let perms = std::fs::Permissions::from_mode(mode);
                            std::fs::set_permissions(&listener.address, perms)?;
                        }

                        let fd = unix_listener.as_raw_fd();
                        // Prevent FD from being closed when UnixListener drops
                        std::mem::forget(unix_listener);
                        Ok(fd)
                    }
                } else {
                    // TCP socket (port number or host:port)
                    self.create_tcp_socket(&listener.address)
                }
            }
            ListenType::Datagram => {
                // UDP socket
                if listener.address.starts_with('/') {
                    // Unix datagram socket
                    self.create_unix_dgram_socket(&listener.address, socket)
                } else {
                    // UDP network socket
                    self.create_udp_socket(&listener.address)
                }
            }
            ListenType::Fifo => {
                self.create_fifo(&listener.address, socket)
            }
            ListenType::Netlink => {
                // Netlink sockets are complex - stub for now
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "Netlink sockets not yet implemented",
                ))
            }
        }
    }

    fn create_abstract_unix_socket(&self, addr: &str) -> std::io::Result<RawFd> {
        use std::mem::size_of;

        unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }

            // Set SO_REUSEADDR
            let optval: libc::c_int = 1;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &optval as *const _ as *const libc::c_void,
                size_of::<libc::c_int>() as libc::socklen_t,
            );

            let mut sockaddr: libc::sockaddr_un = std::mem::zeroed();
            sockaddr.sun_family = libc::AF_UNIX as u16;

            // Copy address including the null byte for abstract sockets
            let bytes = addr.as_bytes();
            let len = std::cmp::min(bytes.len(), sockaddr.sun_path.len());
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                sockaddr.sun_path.as_mut_ptr() as *mut u8,
                len,
            );

            let addr_len = (size_of::<libc::sa_family_t>() + len) as libc::socklen_t;

            if libc::bind(fd, &sockaddr as *const _ as *const libc::sockaddr, addr_len) < 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }

            if libc::listen(fd, 128) < 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }

            Ok(fd)
        }
    }

    fn create_tcp_socket(&self, addr: &str) -> std::io::Result<RawFd> {
        use std::net::TcpListener;

        // Handle port-only or host:port
        let bind_addr = if addr.contains(':') {
            addr.to_string()
        } else {
            format!("0.0.0.0:{}", addr)
        };

        let listener = TcpListener::bind(&bind_addr)?;
        let fd = listener.as_raw_fd();
        std::mem::forget(listener);
        Ok(fd)
    }

    fn create_udp_socket(&self, addr: &str) -> std::io::Result<RawFd> {
        use std::net::UdpSocket;

        let bind_addr = if addr.contains(':') {
            addr.to_string()
        } else {
            format!("0.0.0.0:{}", addr)
        };

        let socket = UdpSocket::bind(&bind_addr)?;
        let fd = socket.as_raw_fd();
        std::mem::forget(socket);
        Ok(fd)
    }

    fn create_unix_dgram_socket(&self, path: &str, socket: &Socket) -> std::io::Result<RawFd> {
        use std::os::unix::net::UnixDatagram;

        // Remove existing socket
        let _ = std::fs::remove_file(path);

        // Create parent directory
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let sock = UnixDatagram::bind(path)?;

        // Set permissions
        if let Some(mode) = socket.socket.socket_mode {
            let perms = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(path, perms)?;
        }

        let fd = sock.as_raw_fd();
        std::mem::forget(sock);
        Ok(fd)
    }

    fn create_fifo(&self, path: &str, socket: &Socket) -> std::io::Result<RawFd> {
        use std::ffi::CString;

        // Remove existing FIFO
        let _ = std::fs::remove_file(path);

        // Create parent directory
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let mode = socket.socket.socket_mode.unwrap_or(0o644);
        let c_path = CString::new(path).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid path")
        })?;

        unsafe {
            if libc::mkfifo(c_path.as_ptr(), mode) < 0 {
                return Err(std::io::Error::last_os_error());
            }

            let fd = libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(fd)
        }
    }

    /// Stop a socket unit (close listening sockets)
    async fn stop_socket(&mut self, name: &str, socket: &Socket) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        log::info!("Stopping socket {}", name);

        // Close all socket FDs
        if let Some(fds) = self.socket_fds.remove(name) {
            for fd in fds {
                unsafe { libc::close(fd) };
            }
        }

        // Remove socket files if RemoveOnStop=yes
        if socket.socket.remove_on_stop {
            for listener in &socket.socket.listeners {
                if listener.address.starts_with('/') {
                    let _ = std::fs::remove_file(&listener.address);
                }
            }
        }

        if let Some(state) = self.states.get_mut(name) {
            state.set_stopped(0);
        }

        log::info!("{} stopped", name);
        Ok(())
    }

    /// Get listening socket FDs for a service (for socket activation)
    pub fn get_socket_fds(&self, service_name: &str) -> Vec<RawFd> {
        // Find socket unit that activates this service
        for (socket_name, unit) in &self.units {
            if let Some(socket) = unit.as_socket() {
                if socket.service_name() == service_name {
                    if let Some(fds) = self.socket_fds.get(socket_name) {
                        return fds.clone();
                    }
                }
            }
        }
        Vec::new()
    }

    /// Take the socket activation receiver (for use in event loops)
    pub fn take_socket_activation_rx(
        &mut self,
    ) -> Option<mpsc::Receiver<socket_watcher::SocketActivation>> {
        self.socket_activation_rx.take()
    }

    /// Process a socket activation message (start the associated service)
    pub async fn handle_socket_activation(
        &mut self,
        activation: socket_watcher::SocketActivation,
    ) -> Result<(), ManagerError> {
        log::info!(
            "Socket activation: {} triggered by {}",
            activation.service_name,
            activation.socket_name
        );

        // Check if service is already running
        if let Some(state) = self.states.get(&activation.service_name) {
            if state.is_active() {
                log::debug!(
                    "{} already running, skipping activation",
                    activation.service_name
                );
                return Ok(());
            }
        }

        // Start the service
        self.start(&activation.service_name).await
    }

    /// Start a timer unit (schedule service activation)
    async fn start_timer(&mut self, name: &str, timer: &Timer) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        log::info!("Starting timer {}", name);

        // Calculate next trigger time
        let next_trigger = timer_scheduler::calculate_next_trigger(timer, self.boot_time);

        if let Some(delay) = next_trigger {
            let service_name = timer.service_name();
            let timer_name = name.to_string();
            let tx = self.timer_tx.clone();

            log::debug!("{}: scheduling to fire in {:?}", name, delay);

            // Spawn timer watcher task
            tokio::spawn(async move {
                timer_scheduler::watch_timer(timer_name, service_name, delay, tx).await;
            });
        } else {
            log::debug!("{}: no trigger configured, timer idle", name);
        }

        // Mark as active
        if let Some(state) = self.states.get_mut(name) {
            state.set_running(0);
        }

        log::info!("{} active", name);
        Ok(())
    }

    /// Stop a timer unit
    async fn stop_timer(&mut self, name: &str) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        log::info!("Stopping timer {}", name);

        // Timer tasks will complete naturally or on next fire
        // For now, we just mark the timer as stopped

        if let Some(state) = self.states.get_mut(name) {
            state.set_stopped(0);
        }

        log::info!("{} stopped", name);
        Ok(())
    }

    /// Take the timer fired receiver (for use in event loops)
    pub fn take_timer_rx(&mut self) -> Option<mpsc::Receiver<timer_scheduler::TimerFired>> {
        self.timer_rx.take()
    }

    /// Process a timer fired message (start the associated service)
    pub async fn handle_timer_fired(
        &mut self,
        fired: timer_scheduler::TimerFired,
    ) -> Result<(), ManagerError> {
        log::info!(
            "Timer fired: {} triggered by {}",
            fired.service_name,
            fired.timer_name
        );

        // Check if service is already running
        if let Some(state) = self.states.get(&fired.service_name) {
            if state.is_active() {
                log::debug!(
                    "{} already running, skipping timer activation",
                    fired.service_name
                );
                // Reschedule the timer for next trigger
                self.reschedule_timer(&fired.timer_name).await;
                return Ok(());
            }
        }

        // Start the service
        let result = self.start(&fired.service_name).await;

        // Reschedule the timer for next trigger (for repeating timers)
        self.reschedule_timer(&fired.timer_name).await;

        result
    }

    /// Reschedule a timer after it fires
    async fn reschedule_timer(&mut self, timer_name: &str) {
        if let Some(unit) = self.units.get(timer_name).cloned() {
            if let Some(timer) = unit.as_timer() {
                // Check for repeating timer conditions (OnUnitActiveSec, OnCalendar)
                let should_repeat = timer.timer.on_unit_active_sec.is_some()
                    || !timer.timer.on_calendar.is_empty();

                if should_repeat {
                    if let Some(delay) = timer_scheduler::calculate_next_trigger(timer, self.boot_time) {
                        let service_name = timer.service_name();
                        let timer_name = timer_name.to_string();
                        let tx = self.timer_tx.clone();

                        log::debug!("{}: rescheduling to fire in {:?}", timer_name, delay);

                        tokio::spawn(async move {
                            timer_scheduler::watch_timer(timer_name, service_name, delay, tx).await;
                        });
                    }
                }
            }
        }
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

    /// Enable a unit (create symlinks based on [Install] section)
    pub async fn enable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let mut created = Vec::new();
        let mut to_enable = vec![self.normalize_name(name)];
        let mut enabled: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(unit_name) = to_enable.pop() {
            if enabled.contains(&unit_name) {
                continue;
            }
            enabled.insert(unit_name.clone());

            // Load the unit to get its Install section
            if !self.units.contains_key(&unit_name) {
                match self.load(&unit_name).await {
                    Ok(()) => {}
                    Err(ManagerError::NotFound(_)) => {
                        log::warn!("Also= unit {} not found, skipping", unit_name);
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            let unit = self
                .units
                .get(&unit_name)
                .ok_or_else(|| ManagerError::NotFound(unit_name.clone()))?;

            let install = match unit.install_section() {
                Some(i) => i,
                None => {
                    log::debug!("Unit {} has no Install section", unit_name);
                    continue;
                }
            };

            if install.wanted_by.is_empty()
                && install.required_by.is_empty()
                && install.alias.is_empty()
            {
                log::debug!("Unit {} has empty Install section", unit_name);
                continue;
            }

            // Find the unit file path
            let unit_path = self.find_unit(&unit_name)?;

            // Clone lists to avoid borrow issues
            let also_units = install.also.clone();
            let aliases = install.alias.clone();
            let wanted_by = install.wanted_by.clone();
            let required_by = install.required_by.clone();

            // Create symlinks in .wants directories
            for target in &wanted_by {
                let link = self.create_wants_link(&unit_name, target, &unit_path)?;
                created.push(link);
            }

            // Create symlinks in .requires directories
            for target in &required_by {
                let link = self.create_requires_link(&unit_name, target, &unit_path)?;
                created.push(link);
            }

            // Create alias symlinks
            for alias in &aliases {
                let link = self.create_alias_link(alias, &unit_path)?;
                created.push(link);
            }

            // Queue Also= units for enabling
            for also in also_units {
                if !enabled.contains(&also) {
                    to_enable.push(also);
                }
            }
        }

        if created.is_empty() {
            return Err(ManagerError::NoInstallSection(self.normalize_name(name)));
        }

        Ok(created)
    }

    /// Disable a unit (remove symlinks)
    pub async fn disable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let mut removed = Vec::new();
        let mut to_disable = vec![self.normalize_name(name)];
        let mut disabled: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(unit_name) = to_disable.pop() {
            if disabled.contains(&unit_name) {
                continue;
            }
            disabled.insert(unit_name.clone());

            // Load to get Install section
            if !self.units.contains_key(&unit_name) {
                match self.load(&unit_name).await {
                    Ok(()) => {}
                    Err(ManagerError::NotFound(_)) => {
                        log::debug!("Also= unit {} not found, skipping", unit_name);
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            let unit = self
                .units
                .get(&unit_name)
                .ok_or_else(|| ManagerError::NotFound(unit_name.clone()))?;

            let install = match unit.install_section() {
                Some(i) => i,
                None => {
                    log::debug!("Unit {} has no Install section", unit_name);
                    continue;
                }
            };

            // Clone lists to avoid borrow issues
            let also_units = install.also.clone();
            let aliases = install.alias.clone();
            let wanted_by = install.wanted_by.clone();
            let required_by = install.required_by.clone();

            // Remove from .wants directories
            for target in &wanted_by {
                if let Some(link) = self.remove_wants_link(&unit_name, target)? {
                    removed.push(link);
                }
            }

            // Remove from .requires directories
            for target in &required_by {
                if let Some(link) = self.remove_requires_link(&unit_name, target)? {
                    removed.push(link);
                }
            }

            // Remove alias symlinks
            for alias in &aliases {
                if let Some(link) = self.remove_alias_link(alias)? {
                    removed.push(link);
                }
            }

            // Queue Also= units for disabling
            for also in also_units {
                if !disabled.contains(&also) {
                    to_disable.push(also);
                }
            }
        }

        Ok(removed)
    }

    /// Create a symlink in target.wants/
    fn create_wants_link(
        &self,
        unit_name: &str,
        target: &str,
        unit_path: &PathBuf,
    ) -> Result<PathBuf, ManagerError> {
        let wants_dir = PathBuf::from("/etc/systemd/system").join(format!("{}.wants", target));
        std::fs::create_dir_all(&wants_dir).map_err(|e| ManagerError::Io(e.to_string()))?;

        let link_path = wants_dir.join(unit_name);
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    /// Create a symlink in target.requires/
    fn create_requires_link(
        &self,
        unit_name: &str,
        target: &str,
        unit_path: &PathBuf,
    ) -> Result<PathBuf, ManagerError> {
        let requires_dir =
            PathBuf::from("/etc/systemd/system").join(format!("{}.requires", target));
        std::fs::create_dir_all(&requires_dir).map_err(|e| ManagerError::Io(e.to_string()))?;

        let link_path = requires_dir.join(unit_name);
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    /// Remove symlink from target.wants/
    fn remove_wants_link(
        &self,
        unit_name: &str,
        target: &str,
    ) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system")
            .join(format!("{}.wants", target))
            .join(unit_name);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    /// Remove symlink from target.requires/
    fn remove_requires_link(
        &self,
        unit_name: &str,
        target: &str,
    ) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system")
            .join(format!("{}.requires", target))
            .join(unit_name);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    /// Create an alias symlink (Alias= in [Install])
    fn create_alias_link(&self, alias: &str, unit_path: &PathBuf) -> Result<PathBuf, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system").join(alias);

        // Remove existing if present
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    /// Remove an alias symlink
    fn remove_alias_link(&self, alias: &str) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system").join(alias);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    /// Check if a unit is enabled
    pub async fn is_enabled(&mut self, name: &str) -> Result<String, ManagerError> {
        let name = self.normalize_name(name);

        // Load to get Install section
        if !self.units.contains_key(&name) {
            self.load(&name).await?;
        }

        let unit = self
            .units
            .get(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        // No install section = static (can't be enabled/disabled)
        let Some(install) = unit.install_section() else {
            return Ok("static".to_string());
        };

        if install.wanted_by.is_empty()
            && install.required_by.is_empty()
            && install.alias.is_empty()
        {
            return Ok("static".to_string());
        }

        // Check if any symlinks exist
        for target in &install.wanted_by {
            let link_path = PathBuf::from("/etc/systemd/system")
                .join(format!("{}.wants", target))
                .join(&name);
            if link_path.exists() || link_path.is_symlink() {
                return Ok("enabled".to_string());
            }
        }

        for target in &install.required_by {
            let link_path = PathBuf::from("/etc/systemd/system")
                .join(format!("{}.requires", target))
                .join(&name);
            if link_path.exists() || link_path.is_symlink() {
                return Ok("enabled".to_string());
            }
        }

        // Check alias symlinks
        for alias in &install.alias {
            let link_path = PathBuf::from("/etc/systemd/system").join(alias);
            if link_path.exists() || link_path.is_symlink() {
                return Ok("enabled".to_string());
            }
        }

        Ok("disabled".to_string())
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

    /// Check if unit conditions are met
    /// Returns None if all conditions pass, or Some(reason) if a condition fails
    fn check_conditions(&self, unit: &Unit) -> Option<String> {
        let section = unit.unit_section();

        // ConditionPathExists - path must exist (or not exist if prefixed with !)
        for path in &section.condition_path_exists {
            let (negated, path) = if let Some(p) = path.strip_prefix('!') {
                (true, p)
            } else {
                (false, path.as_str())
            };

            let exists = std::path::Path::new(path).exists();
            if negated && exists {
                return Some(format!(
                    "ConditionPathExists=!{} failed (path exists)",
                    path
                ));
            }
            if !negated && !exists {
                return Some(format!(
                    "ConditionPathExists={} failed (path missing)",
                    path
                ));
            }
        }

        // ConditionDirectoryNotEmpty - directory must exist and have entries
        for path in &section.condition_directory_not_empty {
            let (negated, path) = if let Some(p) = path.strip_prefix('!') {
                (true, p)
            } else {
                (false, path.as_str())
            };

            let dir_path = std::path::Path::new(path);
            let is_not_empty = dir_path.is_dir()
                && std::fs::read_dir(dir_path)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(false);

            if negated && is_not_empty {
                return Some(format!(
                    "ConditionDirectoryNotEmpty=!{} failed (not empty)",
                    path
                ));
            }
            if !negated && !is_not_empty {
                return Some(format!(
                    "ConditionDirectoryNotEmpty={} failed (empty or missing)",
                    path
                ));
            }
        }

        // ConditionVirtualization - check if running in VM/container
        for virt in &section.condition_virtualization {
            let (negated, check) = if let Some(v) = virt.strip_prefix('!') {
                (true, v)
            } else {
                (false, virt.as_str())
            };

            let detected = self.detect_virtualization();
            let matches = match check.to_lowercase().as_str() {
                "yes" | "true" => detected.is_some(),
                "no" | "false" => detected.is_none(),
                "vm" => detected.as_ref().map(|v| v.is_vm()).unwrap_or(false),
                "container" => detected.as_ref().map(|v| v.is_container()).unwrap_or(false),
                specific => detected.as_ref().map(|v| v.matches(specific)).unwrap_or(false),
            };

            if negated && matches {
                return Some(format!(
                    "ConditionVirtualization=!{} failed (matched {:?})",
                    check, detected
                ));
            }
            if !negated && !matches {
                return Some(format!(
                    "ConditionVirtualization={} failed (detected {:?})",
                    check, detected
                ));
            }
        }

        // ConditionCapability - check if process has capability
        for cap in &section.condition_capability {
            let (negated, cap_name) = if let Some(c) = cap.strip_prefix('!') {
                (true, c)
            } else {
                (false, cap.as_str())
            };

            let has_cap = self.check_capability(cap_name);

            if negated && has_cap {
                return Some(format!(
                    "ConditionCapability=!{} failed (capability present)",
                    cap_name
                ));
            }
            if !negated && !has_cap {
                return Some(format!(
                    "ConditionCapability={} failed (capability missing)",
                    cap_name
                ));
            }
        }

        // ConditionKernelCommandLine - check /proc/cmdline
        for param in &section.condition_kernel_command_line {
            let (negated, check) = if let Some(p) = param.strip_prefix('!') {
                (true, p)
            } else {
                (false, param.as_str())
            };

            let has_param = self.check_kernel_cmdline(check);

            if negated && has_param {
                return Some(format!(
                    "ConditionKernelCommandLine=!{} failed (parameter present)",
                    check
                ));
            }
            if !negated && !has_param {
                return Some(format!(
                    "ConditionKernelCommandLine={} failed (parameter missing)",
                    check
                ));
            }
        }

        // ConditionSecurity - check security framework
        for sec in &section.condition_security {
            let (negated, framework) = if let Some(s) = sec.strip_prefix('!') {
                (true, s)
            } else {
                (false, sec.as_str())
            };

            let has_framework = self.check_security_framework(framework);

            if negated && has_framework {
                return Some(format!(
                    "ConditionSecurity=!{} failed (security framework active)",
                    framework
                ));
            }
            if !negated && !has_framework {
                return Some(format!(
                    "ConditionSecurity={} failed (security framework not active)",
                    framework
                ));
            }
        }

        // ConditionFirstBoot - check if this is first boot
        if let Some(first_boot_wanted) = section.condition_first_boot {
            let is_first_boot = self.check_first_boot();
            if first_boot_wanted && !is_first_boot {
                return Some("ConditionFirstBoot=yes failed (not first boot)".to_string());
            }
            if !first_boot_wanted && is_first_boot {
                return Some("ConditionFirstBoot=no failed (is first boot)".to_string());
            }
        }

        // ConditionNeedsUpdate - check if /etc or /var needs update
        for update in &section.condition_needs_update {
            let (negated, check) = if let Some(u) = update.strip_prefix('!') {
                (true, u)
            } else {
                (false, update.as_str())
            };

            // Handle trigger prefix (|) - passes on first boot even if no update needed
            let (trigger, path) = if let Some(p) = check.strip_prefix('|') {
                (true, p)
            } else {
                (false, check)
            };

            let needs_update = self.check_needs_update(path, trigger);

            if negated && needs_update {
                return Some(format!(
                    "ConditionNeedsUpdate=!{} failed (update needed)",
                    check
                ));
            }
            if !negated && !needs_update {
                return Some(format!(
                    "ConditionNeedsUpdate={} failed (no update needed)",
                    check
                ));
            }
        }

        None
    }

    /// Detected virtualization type
    fn detect_virtualization(&self) -> Option<VirtualizationType> {
        // Check for container environments
        if std::path::Path::new("/.dockerenv").exists() {
            return Some(VirtualizationType::Docker);
        }
        if std::path::Path::new("/run/.containerenv").exists() {
            return Some(VirtualizationType::Podman);
        }

        // Check /proc/1/environ for container markers
        if let Ok(environ) = std::fs::read_to_string("/proc/1/environ") {
            if environ.contains("container=") {
                // Parse the container type
                for part in environ.split('\0') {
                    if let Some(val) = part.strip_prefix("container=") {
                        return Some(VirtualizationType::from_container_env(val));
                    }
                }
                return Some(VirtualizationType::Container);
            }
        }

        // Check /proc/1/cgroup for systemd-nspawn
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("/machine.slice/") || cgroup.contains("machine-") {
                return Some(VirtualizationType::SystemdNspawn);
            }
        }

        // Check for VM via DMI (requires /sys/class/dmi/id/)
        if let Ok(product) = std::fs::read_to_string("/sys/class/dmi/id/product_name") {
            let product = product.trim().to_lowercase();
            if product.contains("virtualbox") {
                return Some(VirtualizationType::VirtualBox);
            }
            if product.contains("vmware") {
                return Some(VirtualizationType::VMware);
            }
            if product.contains("qemu") || product.contains("kvm") {
                return Some(VirtualizationType::Qemu);
            }
            if product.contains("bochs") {
                return Some(VirtualizationType::Bochs);
            }
            if product.contains("xen") {
                return Some(VirtualizationType::Xen);
            }
            if product.contains("hyper-v") || product.contains("microsoft") {
                return Some(VirtualizationType::HyperV);
            }
        }

        // Check for VM via sys_vendor
        if let Ok(vendor) = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor") {
            let vendor = vendor.trim().to_lowercase();
            if vendor.contains("qemu") {
                return Some(VirtualizationType::Qemu);
            }
            if vendor.contains("vmware") {
                return Some(VirtualizationType::VMware);
            }
            if vendor.contains("innotek") || vendor.contains("oracle") {
                return Some(VirtualizationType::VirtualBox);
            }
        }

        None
    }

    /// Check if process has a specific capability
    fn check_capability(&self, cap_name: &str) -> bool {
        // Map capability name to number
        let cap_num = match cap_name.to_uppercase().as_str() {
            "CAP_CHOWN" => 0,
            "CAP_DAC_OVERRIDE" => 1,
            "CAP_DAC_READ_SEARCH" => 2,
            "CAP_FOWNER" => 3,
            "CAP_FSETID" => 4,
            "CAP_KILL" => 5,
            "CAP_SETGID" => 6,
            "CAP_SETUID" => 7,
            "CAP_SETPCAP" => 8,
            "CAP_LINUX_IMMUTABLE" => 9,
            "CAP_NET_BIND_SERVICE" => 10,
            "CAP_NET_BROADCAST" => 11,
            "CAP_NET_ADMIN" => 12,
            "CAP_NET_RAW" => 13,
            "CAP_IPC_LOCK" => 14,
            "CAP_IPC_OWNER" => 15,
            "CAP_SYS_MODULE" => 16,
            "CAP_SYS_RAWIO" => 17,
            "CAP_SYS_CHROOT" => 18,
            "CAP_SYS_PTRACE" => 19,
            "CAP_SYS_PACCT" => 20,
            "CAP_SYS_ADMIN" => 21,
            "CAP_SYS_BOOT" => 22,
            "CAP_SYS_NICE" => 23,
            "CAP_SYS_RESOURCE" => 24,
            "CAP_SYS_TIME" => 25,
            "CAP_SYS_TTY_CONFIG" => 26,
            "CAP_MKNOD" => 27,
            "CAP_LEASE" => 28,
            "CAP_AUDIT_WRITE" => 29,
            "CAP_AUDIT_CONTROL" => 30,
            "CAP_SETFCAP" => 31,
            "CAP_MAC_OVERRIDE" => 32,
            "CAP_MAC_ADMIN" => 33,
            "CAP_SYSLOG" => 34,
            "CAP_WAKE_ALARM" => 35,
            "CAP_BLOCK_SUSPEND" => 36,
            "CAP_AUDIT_READ" => 37,
            "CAP_PERFMON" => 38,
            "CAP_BPF" => 39,
            "CAP_CHECKPOINT_RESTORE" => 40,
            _ => return false, // Unknown capability
        };

        // Read effective capabilities from /proc/self/status
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(hex) = line.strip_prefix("CapEff:\t") {
                    if let Ok(caps) = u64::from_str_radix(hex.trim(), 16) {
                        return (caps & (1u64 << cap_num)) != 0;
                    }
                }
            }
        }

        false
    }

    /// Check if kernel command line contains parameter
    fn check_kernel_cmdline(&self, param: &str) -> bool {
        if let Ok(cmdline) = std::fs::read_to_string("/proc/cmdline") {
            // Check for key=value or just key
            if param.contains('=') {
                // Exact match for key=value
                cmdline.split_whitespace().any(|p| p == param)
            } else {
                // Match key or key=anything
                cmdline.split_whitespace().any(|p| {
                    p == param || p.starts_with(&format!("{}=", param))
                })
            }
        } else {
            false
        }
    }

    /// Check if security framework is active
    fn check_security_framework(&self, framework: &str) -> bool {
        match framework.to_lowercase().as_str() {
            "selinux" => std::path::Path::new("/sys/fs/selinux").exists(),
            "apparmor" => std::path::Path::new("/sys/kernel/security/apparmor").exists(),
            "smack" => std::path::Path::new("/sys/fs/smackfs").exists(),
            "tomoyo" => std::path::Path::new("/sys/kernel/security/tomoyo").exists(),
            "ima" => std::path::Path::new("/sys/kernel/security/ima").exists(),
            "audit" => std::path::Path::new("/proc/self/loginuid").exists(),
            "uefi-secureboot" => {
                // Check if Secure Boot is enabled
                std::path::Path::new("/sys/firmware/efi/efivars/SecureBoot-*").exists()
                    || std::fs::read_dir("/sys/firmware/efi/efivars")
                        .map(|entries| {
                            entries.filter_map(|e| e.ok()).any(|e| {
                                e.file_name().to_string_lossy().starts_with("SecureBoot-")
                            })
                        })
                        .unwrap_or(false)
            }
            _ => false,
        }
    }

    /// Check if this is first boot
    fn check_first_boot(&self) -> bool {
        // systemd uses /run/systemd/first-boot as marker
        if std::path::Path::new("/run/systemd/first-boot").exists() {
            return true;
        }

        // Also check if /etc/machine-id is empty or uninitialized
        if let Ok(machine_id) = std::fs::read_to_string("/etc/machine-id") {
            let content = machine_id.trim();
            // Uninitialized machine-id is empty or all zeros
            if content.is_empty() || content.chars().all(|c| c == '0') {
                return true;
            }
        } else {
            // If machine-id doesn't exist, it's first boot
            return true;
        }

        false
    }

    /// Check if a directory needs update (for ConditionNeedsUpdate)
    /// Returns true if the directory mtime is newer than the update-done flag file
    fn check_needs_update(&self, path: &str, trigger: bool) -> bool {
        // Determine which directory to check
        let (check_path, flag_file) = match path.to_lowercase().as_str() {
            "/etc" => ("/etc", "/var/lib/systemd/update-done.d/etc"),
            "/var" => ("/var", "/var/lib/systemd/update-done.d/var"),
            _ => return false, // Unknown path
        };

        let flag_path = std::path::Path::new(flag_file);
        let dir_path = std::path::Path::new(check_path);

        // If flag file doesn't exist
        if !flag_path.exists() {
            // In trigger mode, this means we need to run
            // In non-trigger mode, this also means update needed
            return true;
        }

        // Compare mtimes
        let flag_mtime = match std::fs::metadata(flag_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return trigger, // Can't read flag, trigger mode determines result
        };

        let dir_mtime = match std::fs::metadata(dir_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return false, // Can't read dir, assume no update needed
        };

        // Directory is newer than flag file = update needed
        dir_mtime > flag_mtime
    }

    /// Normalize unit name (add .service suffix if no suffix present)
    fn normalize_name(&self, name: &str) -> String {
        if name.ends_with(".service")
            || name.ends_with(".target")
            || name.ends_with(".mount")
            || name.ends_with(".socket")
            || name.ends_with(".path")
            || name.ends_with(".slice")
        {
            name.to_string()
        } else {
            format!("{}.service", name)
        }
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

    /// Process pending notify messages (READY, STOPPING, WATCHDOG, etc.)
    pub async fn process_notify(&mut self) {
        let Some(rx) = &mut self.notify_rx else {
            return;
        };

        // Process all pending messages without blocking
        while let Ok(msg) = rx.try_recv() {
            if msg.is_ready() {
                // Find which service this PID belongs to
                // First check waiting_ready map, then fall back to process map
                let service_name = if let Some(pid) = msg.main_pid() {
                    self.waiting_ready.remove(&pid)
                } else {
                    // Try to find by iterating processes
                    let mut found = None;
                    for (name, child) in &self.processes {
                        if let Some(pid) = child.id() {
                            if self.waiting_ready.contains_key(&pid) {
                                found = Some((pid, name.clone()));
                                break;
                            }
                        }
                    }
                    if let Some((pid, name)) = found {
                        self.waiting_ready.remove(&pid);
                        Some(name)
                    } else {
                        None
                    }
                };

                if let Some(name) = service_name {
                    if let Some(state) = self.states.get_mut(&name) {
                        let pid = self.processes.get(&name).and_then(|c| c.id()).unwrap_or(0);
                        state.set_running(pid);
                        self.active_jobs = self.active_jobs.saturating_sub(1);
                        log::info!("{} signaled READY", name);

                        // Set watchdog deadline for Type=notify services
                        if let Some(wd) = self
                            .units
                            .get(&name)
                            .and_then(|u| u.as_service())
                            .and_then(|s| s.service.watchdog_sec)
                        {
                            self.watchdog_deadlines
                                .insert(name.clone(), std::time::Instant::now() + wd);
                        }
                    }
                }
            }

            // Handle WATCHDOG=1 ping - reset the deadline
            if msg.is_watchdog() {
                // Find service by PID
                let service_name = self
                    .processes
                    .iter()
                    .find(|(_, child)| child.id() == Some(msg.pid))
                    .map(|(name, _)| name.clone());

                if let Some(name) = service_name {
                    if let Some(wd) = self
                        .units
                        .get(&name)
                        .and_then(|u| u.as_service())
                        .and_then(|s| s.service.watchdog_sec)
                    {
                        self.watchdog_deadlines
                            .insert(name.clone(), std::time::Instant::now() + wd);
                        log::trace!("{} watchdog ping received", name);
                    }
                }
            }

            if msg.is_stopping() {
                log::debug!("Service signaled STOPPING (PID hint: {})", msg.pid);
            }

            if let Some(status) = msg.status() {
                log::debug!("Service status: {}", status);
            }
        }
    }

    /// Check on running processes and update states
    pub async fn reap(&mut self) {
        let mut exited = Vec::new();

        for (name, child) in &mut self.processes {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited.push((name.clone(), status.code().unwrap_or(-1)));
                }
                Ok(None) => {
                    // Still running
                }
                Err(e) => {
                    log::error!("Error checking {}: {}", name, e);
                }
            }
        }

        for (name, code) in exited {
            self.processes.remove(&name);

            // Get service config for restart policy
            let (restart_policy, restart_sec, remain_after_exit, is_oneshot, is_forking) = self
                .units
                .get(&name)
                .and_then(|u| u.as_service())
                .map(|s| {
                    (
                        s.service.restart.clone(),
                        s.service.restart_sec,
                        s.service.remain_after_exit,
                        s.service.service_type == ServiceType::Oneshot,
                        s.service.service_type == ServiceType::Forking,
                    )
                })
                .unwrap_or((
                    RestartPolicy::No,
                    std::time::Duration::from_millis(100),
                    false,
                    false,
                    false,
                ));

            // Handle Type=forking: parent exited, read PIDFile
            if is_forking && code == 0 {
                if let Some(pid_file) = self.pid_files.remove(&name) {
                    match std::fs::read_to_string(&pid_file) {
                        Ok(content) => {
                            if let Ok(child_pid) = content.trim().parse::<u32>() {
                                if let Some(state) = self.states.get_mut(&name) {
                                    state.set_running(child_pid);
                                    self.active_jobs = self.active_jobs.saturating_sub(1);
                                    log::info!(
                                        "{} forked, main PID {} (from {})",
                                        name,
                                        child_pid,
                                        pid_file.display()
                                    );
                                }
                                // Set watchdog deadline for Type=forking services
                                if let Some(wd) = self
                                    .units
                                    .get(&name)
                                    .and_then(|u| u.as_service())
                                    .and_then(|s| s.service.watchdog_sec)
                                {
                                    self.watchdog_deadlines
                                        .insert(name.clone(), std::time::Instant::now() + wd);
                                }
                                continue; // Don't process as normal exit
                            } else {
                                log::warn!("{}: invalid PID in {}", name, pid_file.display());
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "{}: failed to read PIDFile {}: {}",
                                name,
                                pid_file.display(),
                                e
                            );
                        }
                    }
                } else {
                    // No PIDFile - assume forked successfully, but we can't track the child
                    log::warn!("{} forked but no PIDFile configured", name);
                    if let Some(state) = self.states.get_mut(&name) {
                        state.set_running(0); // Unknown PID
                    }
                    self.active_jobs = self.active_jobs.saturating_sub(1);
                    // Set watchdog deadline even without PIDFile
                    if let Some(wd) = self
                        .units
                        .get(&name)
                        .and_then(|u| u.as_service())
                        .and_then(|s| s.service.watchdog_sec)
                    {
                        self.watchdog_deadlines
                            .insert(name.clone(), std::time::Instant::now() + wd);
                    }
                    continue;
                }
            }

            // Determine if we should restart
            let should_restart = match restart_policy {
                RestartPolicy::No => false,
                RestartPolicy::OnFailure => code != 0,
                RestartPolicy::Always => true,
            };

            if let Some(state) = self.states.get_mut(&name) {
                if code == 0 {
                    // Clean exit
                    if is_oneshot && remain_after_exit {
                        // Keep as active (exited) for oneshot with RemainAfterExit=yes
                        state.active = ActiveState::Active;
                        state.sub = SubState::Exited;
                        state.main_pid = None;
                        state.exit_code = Some(code);
                        state.reset_restart_count();
                        log::info!("{} exited (RemainAfterExit=yes)", name);
                    } else if should_restart {
                        state.set_auto_restart(restart_sec);
                        log::info!("{} exited, scheduling restart in {:?}", name, restart_sec);
                    } else {
                        state.set_stopped(code);
                        state.reset_restart_count();
                        log::info!("{} exited cleanly", name);
                    }
                } else {
                    // Failed exit
                    if should_restart {
                        state.set_auto_restart(restart_sec);
                        log::warn!(
                            "{} failed (exit {}), scheduling restart in {:?}",
                            name,
                            code,
                            restart_sec
                        );
                    } else {
                        state.set_failed(format!("Exit code {}", code));
                        log::warn!("{} failed with exit code {}", name, code);
                    }
                }
            }

            // Clean up cgroup
            if self.cgroup_paths.remove(&name).is_some() {
                if let Some(ref cgroup_mgr) = self.cgroup_manager {
                    if let Err(e) = cgroup_mgr.cleanup_service_cgroup(&name) {
                        log::debug!("Failed to clean up cgroup for {}: {}", name, e);
                    }
                }
            }

            // Clean up watchdog (will be re-set on restart)
            self.watchdog_deadlines.remove(&name);
        }
    }

    /// Process pending restarts
    pub async fn process_restarts(&mut self) {
        // Collect services due for restart
        let due: Vec<String> = self
            .states
            .iter()
            .filter(|(_, state)| state.sub == SubState::AutoRestart && state.restart_due())
            .map(|(name, _)| name.clone())
            .collect();

        for name in due {
            log::info!("Restarting {}", name);
            if let Some(state) = self.states.get_mut(&name) {
                state.clear_restart();
            }
            if let Err(e) = self.start_single(&name).await {
                log::error!("Failed to restart {}: {}", name, e);
                if let Some(state) = self.states.get_mut(&name) {
                    state.set_failed(format!("Restart failed: {}", e));
                }
            }
        }
    }

    /// Check if any Type=dbus services have acquired their bus name
    pub async fn process_dbus_ready(&mut self) {
        if self.waiting_bus_name.is_empty() {
            return;
        }

        // Try to connect to system bus
        let conn = match zbus::Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                log::debug!("Cannot check D-Bus names (no connection): {}", e);
                return;
            }
        };

        // Check each waited name
        let mut ready = Vec::new();
        for (bus_name, service_name) in &self.waiting_bus_name {
            // Use the fdo DBus interface to check if the name has an owner
            match conn
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetNameOwner",
                    &(bus_name.as_str(),),
                )
                .await
            {
                Ok(_) => {
                    // Name has an owner - service is ready
                    ready.push((bus_name.clone(), service_name.clone()));
                }
                Err(_) => {
                    // Name not owned yet
                }
            }
        }

        // Mark ready services as running
        for (bus_name, service_name) in ready {
            self.waiting_bus_name.remove(&bus_name);
            if let Some(state) = self.states.get_mut(&service_name) {
                let pid = self
                    .processes
                    .get(&service_name)
                    .and_then(|c| c.id())
                    .unwrap_or(0);
                state.set_running(pid);
                self.active_jobs = self.active_jobs.saturating_sub(1);
                log::info!("{} acquired D-Bus name {}", service_name, bus_name);

                // Set watchdog deadline for Type=dbus services
                if let Some(wd) = self
                    .units
                    .get(&service_name)
                    .and_then(|u| u.as_service())
                    .and_then(|s| s.service.watchdog_sec)
                {
                    self.watchdog_deadlines
                        .insert(service_name.clone(), std::time::Instant::now() + wd);
                }
            }
        }
    }

    /// Check for watchdog timeouts and restart services that missed their deadline
    pub async fn process_watchdog(&mut self) {
        let now = std::time::Instant::now();
        let mut timed_out = Vec::new();

        for (name, deadline) in &self.watchdog_deadlines {
            if now > *deadline {
                timed_out.push(name.clone());
            }
        }

        for name in timed_out {
            self.watchdog_deadlines.remove(&name);
            log::warn!("{} watchdog timeout - restarting", name);

            // Kill the service and let restart policy handle it
            if let Some(mut child) = self.processes.remove(&name) {
                if let Some(pid) = child.id() {
                    // Send SIGABRT (standard watchdog signal) then SIGKILL
                    unsafe {
                        libc::kill(pid as i32, libc::SIGABRT);
                    }
                    // Give it a moment, then force kill
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let _ = child.kill().await;
                }
            }

            // Update state to failed with watchdog reason
            if let Some(state) = self.states.get_mut(&name) {
                state.set_failed("Watchdog timeout".to_string());
            }

            // Schedule restart based on policy
            let restart_policy = self
                .units
                .get(&name)
                .and_then(|u| u.as_service())
                .map(|s| s.service.restart.clone())
                .unwrap_or(RestartPolicy::No);

            if restart_policy == RestartPolicy::Always || restart_policy == RestartPolicy::OnFailure
            {
                let restart_sec = self
                    .units
                    .get(&name)
                    .and_then(|u| u.as_service())
                    .map(|s| s.service.restart_sec)
                    .unwrap_or(std::time::Duration::from_millis(100));

                if let Some(state) = self.states.get_mut(&name) {
                    state.set_auto_restart(restart_sec);
                    log::info!("{} scheduling watchdog restart in {:?}", name, restart_sec);
                }
            }
        }
    }
}

/// Check if a path is currently mounted (by reading /proc/mounts)
fn is_mounted(path: &str) -> bool {
    let Ok(content) = std::fs::read_to_string("/proc/mounts") else {
        return false;
    };

    // Normalize path (remove trailing slashes except for root)
    let normalized = if path == "/" {
        path.to_string()
    } else {
        path.trim_end_matches('/').to_string()
    };

    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let mount_point = parts[1];
            // Handle escaped characters in mount points
            let mount_point = mount_point
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");

            let mount_normalized = if mount_point == "/" {
                mount_point
            } else {
                mount_point.trim_end_matches('/').to_string()
            };

            if mount_normalized == normalized {
                return true;
            }
        }
    }

    false
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
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
}
