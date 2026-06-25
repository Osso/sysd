// Service manager
//
// Loads, starts, stops, and monitors services and targets.

mod conditions;
mod deps;
mod dynamic_user;
mod enable;
mod generators;
mod mount_ops;
mod notify;
mod path_ops;
mod path_watcher;
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

use std::collections::{HashMap, HashSet};
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

enum LoadNameResolution {
    Continue(String),
    AlreadyLoaded(String),
}


include!("mod_impl/part1.rs");
include!("mod_impl/part2.rs");
#[cfg(test)]
#[path = "mod_impl/part2_tests.rs"]
mod part2_tests;
include!("mod_impl/part3.rs");
#[cfg(test)]
#[path = "mod_impl/part3_tests.rs"]
mod part3_tests;
