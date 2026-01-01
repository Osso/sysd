//! Typed service definitions matching systemd .service files
//!
//! Structures match directives listed in doc/DESIGN.md

use std::path::PathBuf;
use std::time::Duration;

/// Service type determines startup notification
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ServiceType {
    #[default]
    Simple, // Ready immediately after exec
    Forking, // Ready when main process exits
    Notify,  // Ready on sd_notify READY=1
    Dbus,    // Ready when D-Bus name acquired
    Oneshot, // Run once, no main process
    Idle,    // Like simple, but wait for job queue empty
}

/// Restart policy
#[derive(Debug, Clone, Default, PartialEq)]
pub enum RestartPolicy {
    #[default]
    No,
    OnFailure,
    Always,
}

/// Kill mode for stopping services
#[derive(Debug, Clone, Default, PartialEq)]
pub enum KillMode {
    #[default]
    ControlGroup, // Kill all processes in the cgroup
    Process, // Only kill the main process
    Mixed,   // SIGTERM main, SIGKILL others
    None,    // Don't kill anything
}

impl KillMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "control-group" => Some(Self::ControlGroup),
            "process" => Some(Self::Process),
            "mixed" => Some(Self::Mixed),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Output destination
#[derive(Debug, Clone, Default, PartialEq)]
pub enum StdOutput {
    #[default]
    Journal,
    Inherit,
    Null,
}

/// Input source
#[derive(Debug, Clone, Default, PartialEq)]
pub enum StdInput {
    #[default]
    Null,
    Tty,      // StandardInput=tty
    TtyForce, // StandardInput=tty-force
    TtyFail,  // StandardInput=tty-fail
}

impl StdInput {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "null" | "/dev/null" => Some(Self::Null),
            "tty" => Some(Self::Tty),
            "tty-force" => Some(Self::TtyForce),
            "tty-fail" => Some(Self::TtyFail),
            _ => None,
        }
    }
}

/// NotifyAccess= controls who can send sd_notify messages
#[derive(Debug, Clone, Default, PartialEq)]
pub enum NotifyAccess {
    /// Reject all notifications
    None,
    /// Only accept from main process (default for Type=notify, otherwise none)
    #[default]
    Main,
    /// Accept from main process and any exec commands (ExecStart, ExecStop, etc.)
    Exec,
    /// Accept from any process in the service cgroup
    All,
}

impl NotifyAccess {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "none" => Some(Self::None),
            "main" => Some(Self::Main),
            "exec" => Some(Self::Exec),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// DevicePolicy= controls device access restrictions
#[derive(Debug, Clone, Default, PartialEq)]
pub enum DevicePolicy {
    /// No device restrictions (default)
    #[default]
    Auto,
    /// Only pseudo devices (null, zero, random, urandom, tty, etc.)
    Closed,
    /// No devices at all by default
    Strict,
}

impl DevicePolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "closed" => Some(Self::Closed),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

/// RuntimeDirectoryPreserve= controls /run directory cleanup
#[derive(Debug, Clone, Default, PartialEq)]
pub enum RuntimeDirectoryPreserve {
    /// Remove on service stop (default)
    #[default]
    No,
    /// Keep after service stop
    Yes,
    /// Keep only on restart, remove on stop
    Restart,
}

impl RuntimeDirectoryPreserve {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "no" | "false" | "0" => Some(Self::No),
            "yes" | "true" | "1" => Some(Self::Yes),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }
}

/// ProtectSystem= settings
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ProtectSystem {
    #[default]
    No,      // No protection (default)
    Yes,     // /usr and /boot read-only
    Full,    // /usr, /boot, and /etc read-only
    Strict,  // Entire filesystem read-only except /dev, /proc, /sys
}

impl ProtectSystem {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "no" | "false" | "0" => Some(Self::No),
            "yes" | "true" | "1" => Some(Self::Yes),
            "full" => Some(Self::Full),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

/// ProtectHome= settings
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ProtectHome {
    #[default]
    No,       // No protection (default)
    Yes,      // /home, /root, /run/user inaccessible
    ReadOnly, // /home, /root, /run/user read-only
    Tmpfs,    // /home, /root, /run/user as empty tmpfs
}

impl ProtectHome {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "no" | "false" | "0" => Some(Self::No),
            "yes" | "true" | "1" => Some(Self::Yes),
            "read-only" => Some(Self::ReadOnly),
            "tmpfs" => Some(Self::Tmpfs),
            _ => None,
        }
    }
}

/// ProtectProc= settings for /proc visibility
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ProtectProc {
    #[default]
    Default,     // Normal /proc visibility
    Invisible,   // Hide processes of other users
    Ptraceable,  // Only show ptraceable processes
    NoAccess,    // /proc completely inaccessible
}

impl ProtectProc {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "default" => Some(Self::Default),
            "invisible" => Some(Self::Invisible),
            "ptraceable" => Some(Self::Ptraceable),
            "noaccess" => Some(Self::NoAccess),
            _ => None,
        }
    }
}

/// [Unit] section
#[derive(Debug, Clone)]
pub struct UnitSection {
    pub description: Option<String>,
    pub after: Vec<String>,
    pub before: Vec<String>,
    pub requires: Vec<String>,
    pub wants: Vec<String>,
    pub conflicts: Vec<String>,
    /// BindsTo= - Hard dependency, stop this unit when bound unit stops
    pub binds_to: Vec<String>,
    pub condition_path_exists: Vec<String>,
    pub condition_directory_not_empty: Vec<String>,
    /// ConditionVirtualization= - check for VM/container environment
    pub condition_virtualization: Vec<String>,
    /// ConditionCapability= - check for process capabilities
    pub condition_capability: Vec<String>,
    /// ConditionKernelCommandLine= - check /proc/cmdline
    pub condition_kernel_command_line: Vec<String>,
    /// ConditionSecurity= - check security framework (selinux, apparmor, etc)
    pub condition_security: Vec<String>,
    /// ConditionFirstBoot= - check if first boot
    pub condition_first_boot: Option<bool>,
    /// ConditionNeedsUpdate= - check if /etc or /var needs updates
    pub condition_needs_update: Vec<String>,
    /// If true (default), add implicit deps on basic.target, shutdown.target
    pub default_dependencies: bool,
    /// IgnoreOnIsolate= - Don't stop this unit during isolate operations
    pub ignore_on_isolate: bool,
}

impl Default for UnitSection {
    fn default() -> Self {
        Self {
            description: None,
            after: Vec::new(),
            before: Vec::new(),
            requires: Vec::new(),
            wants: Vec::new(),
            conflicts: Vec::new(),
            binds_to: Vec::new(),
            condition_path_exists: Vec::new(),
            condition_directory_not_empty: Vec::new(),
            condition_virtualization: Vec::new(),
            condition_capability: Vec::new(),
            condition_kernel_command_line: Vec::new(),
            condition_security: Vec::new(),
            condition_first_boot: None,
            condition_needs_update: Vec::new(),
            default_dependencies: true, // systemd default
            ignore_on_isolate: false,
        }
    }
}

/// [Service] section
#[derive(Debug, Clone)]
pub struct ServiceSection {
    pub service_type: ServiceType,

    // Execution
    pub exec_start: Vec<String>,
    pub exec_start_pre: Vec<String>,
    pub exec_start_post: Vec<String>,
    pub exec_stop: Vec<String>,
    pub exec_reload: Vec<String>,

    // Restart
    pub restart: RestartPolicy,
    pub restart_sec: Duration, // Default: 100ms per systemd docs
    pub timeout_start_sec: Option<Duration>,
    pub timeout_stop_sec: Option<Duration>,
    pub remain_after_exit: bool, // For Type=oneshot: stay active after exit

    // Watchdog
    pub watchdog_sec: Option<Duration>, // Watchdog timeout (service must ping)

    // Notification
    pub notify_access: NotifyAccess, // Who can send sd_notify messages

    // Type=forking
    pub pid_file: Option<PathBuf>, // PIDFile= for Type=forking

    // Type=dbus
    pub bus_name: Option<String>, // BusName= for Type=dbus

    // Stop behavior
    pub kill_mode: KillMode,

    // Credentials
    pub user: Option<String>,
    pub group: Option<String>,
    pub working_directory: Option<PathBuf>,

    // Environment
    pub environment: Vec<(String, String)>,
    pub environment_file: Vec<PathBuf>,
    pub unset_environment: Vec<String>, // UnsetEnvironment=

    // I/O
    pub standard_output: StdOutput,
    pub standard_error: StdOutput,
    pub standard_input: StdInput,

    // TTY handling (for getty and similar)
    pub tty_path: Option<PathBuf>,
    pub tty_reset: bool,

    // Resource limits (cgroup v2)
    pub memory_max: Option<u64>, // bytes
    pub cpu_quota: Option<u32>,  // percentage (100 = 1 core)
    pub tasks_max: Option<u32>,

    // Process limits (setrlimit)
    pub limit_nofile: Option<u64>, // LimitNOFILE= (max open files)
    pub limit_nproc: Option<u64>,  // LimitNPROC= (max processes)
    pub limit_core: Option<u64>,   // LimitCORE= (core dump size, 0=disabled)

    // M17: Auto-created directories
    pub state_directory: Vec<String>,         // StateDirectory= (/var/lib/<name>)
    pub runtime_directory: Vec<String>,       // RuntimeDirectory= (/run/<name>)
    pub configuration_directory: Vec<String>, // ConfigurationDirectory= (/etc/<name>)
    pub logs_directory: Vec<String>,          // LogsDirectory= (/var/log/<name>)
    pub cache_directory: Vec<String>,         // CacheDirectory= (/var/cache/<name>)
    pub runtime_directory_preserve: RuntimeDirectoryPreserve, // RuntimeDirectoryPreserve=
    pub dynamic_user: bool,                   // DynamicUser= (allocate ephemeral UID/GID)

    // OOM killer
    pub oom_score_adjust: Option<i32>, // OOMScoreAdjust= (-1000 to 1000)

    // Security sandboxing
    pub no_new_privileges: bool,              // NoNewPrivileges=
    pub protect_system: ProtectSystem,        // ProtectSystem=
    pub protect_home: ProtectHome,            // ProtectHome=
    pub private_tmp: bool,                    // PrivateTmp=
    pub private_devices: bool,                // PrivateDevices=
    pub private_network: bool,                // PrivateNetwork=
    pub protect_kernel_modules: bool,         // ProtectKernelModules=
    pub protect_proc: ProtectProc,            // ProtectProc=

    // Capabilities
    pub capability_bounding_set: Vec<String>, // CapabilityBoundingSet=
    pub ambient_capabilities: Vec<String>,    // AmbientCapabilities=

    // Namespace restrictions (None = not set, Some(empty) = all blocked)
    pub restrict_namespaces: Option<Vec<String>>, // RestrictNamespaces=

    // Path restrictions
    pub read_write_paths: Vec<PathBuf>,   // ReadWritePaths=
    pub read_only_paths: Vec<PathBuf>,    // ReadOnlyPaths=
    pub inaccessible_paths: Vec<PathBuf>, // InaccessiblePaths=

    // Seccomp
    pub system_call_filter: Vec<String>,          // SystemCallFilter=
    pub system_call_error_number: Option<i32>,    // SystemCallErrorNumber= (errno for blocked calls)
    pub system_call_architectures: Vec<String>,   // SystemCallArchitectures= (native, x86, etc.)

    // Device access control (mount namespace isolation)
    pub device_policy: DevicePolicy, // DevicePolicy= (auto, closed, strict)
    pub device_allow: Vec<String>,   // DeviceAllow= (format: "/dev/null rw" or "char-pts r")

    // M16: Extended security hardening
    pub restrict_realtime: bool,           // RestrictRealtime= - block realtime scheduling
    pub protect_control_groups: bool,      // ProtectControlGroups= - /sys/fs/cgroup read-only
    pub memory_deny_write_execute: bool,   // MemoryDenyWriteExecute= - block W+X memory
    pub lock_personality: bool,            // LockPersonality= - lock execution domain
    pub protect_kernel_tunables: bool,     // ProtectKernelTunables= - /proc/sys, /sys read-only
    pub protect_kernel_logs: bool,         // ProtectKernelLogs= - block /dev/kmsg
    pub protect_clock: bool,               // ProtectClock= - block clock_settime, etc.
    pub protect_hostname: bool,            // ProtectHostname= - block sethostname
    pub ignore_sigpipe: bool,              // IgnoreSIGPIPE= - set SIG_IGN for SIGPIPE
    pub restrict_suid_sgid: bool,          // RestrictSUIDSGID= - block setuid/setgid files
    pub restrict_address_families: Option<Vec<String>>, // RestrictAddressFamilies=

    // M18: Process control & dependencies
    pub start_limit_burst: Option<u32>,       // StartLimitBurst= - max restarts in interval
    pub start_limit_interval_sec: Option<Duration>, // StartLimitIntervalSec= - rate limit window
    pub sockets: Vec<String>,                 // Sockets= - associated socket units
    pub send_sighup: bool,                    // SendSIGHUP= - send SIGHUP before SIGTERM
    pub slice: Option<String>,                // Slice= - explicit cgroup slice
    pub delegate: bool,                       // Delegate= - allow service to manage own cgroup
    pub exec_stop_post: Vec<String>,          // ExecStopPost= - post-stop commands
    pub file_descriptor_store_max: Option<u32>, // FileDescriptorStoreMax= - FD store size
    pub restart_prevent_exit_status: Vec<i32>, // RestartPreventExitStatus= - don't restart on these
}

impl Default for ServiceSection {
    fn default() -> Self {
        Self {
            service_type: ServiceType::default(),
            exec_start: Vec::new(),
            exec_start_pre: Vec::new(),
            exec_start_post: Vec::new(),
            exec_stop: Vec::new(),
            exec_reload: Vec::new(),
            restart: RestartPolicy::default(),
            restart_sec: Duration::from_millis(100), // systemd default
            timeout_start_sec: None,
            timeout_stop_sec: None,
            remain_after_exit: false,
            watchdog_sec: None,
            notify_access: NotifyAccess::default(),
            pid_file: None,
            bus_name: None,
            kill_mode: KillMode::default(),
            user: None,
            group: None,
            working_directory: None,
            environment: Vec::new(),
            environment_file: Vec::new(),
            unset_environment: Vec::new(),
            standard_output: StdOutput::default(),
            standard_error: StdOutput::default(),
            standard_input: StdInput::default(),
            tty_path: None,
            tty_reset: false,
            memory_max: None,
            cpu_quota: None,
            tasks_max: None,
            limit_nofile: None,
            limit_nproc: None,
            limit_core: None,
            // M17: Auto-created directories
            state_directory: Vec::new(),
            runtime_directory: Vec::new(),
            configuration_directory: Vec::new(),
            logs_directory: Vec::new(),
            cache_directory: Vec::new(),
            runtime_directory_preserve: RuntimeDirectoryPreserve::No,
            dynamic_user: false,
            oom_score_adjust: None,
            // Security sandboxing defaults (all disabled)
            no_new_privileges: false,
            protect_system: ProtectSystem::default(),
            protect_home: ProtectHome::default(),
            private_tmp: false,
            private_devices: false,
            private_network: false,
            protect_kernel_modules: false,
            protect_proc: ProtectProc::default(),
            capability_bounding_set: Vec::new(),
            ambient_capabilities: Vec::new(),
            restrict_namespaces: None,
            read_write_paths: Vec::new(),
            read_only_paths: Vec::new(),
            inaccessible_paths: Vec::new(),
            system_call_filter: Vec::new(),
            system_call_error_number: None,
            system_call_architectures: Vec::new(),
            device_policy: DevicePolicy::Auto,
            device_allow: Vec::new(),
            // M16 defaults (all disabled)
            restrict_realtime: false,
            protect_control_groups: false,
            memory_deny_write_execute: false,
            lock_personality: false,
            protect_kernel_tunables: false,
            protect_kernel_logs: false,
            protect_clock: false,
            protect_hostname: false,
            ignore_sigpipe: false,
            restrict_suid_sgid: false,
            restrict_address_families: None,
            // M18 defaults
            start_limit_burst: None,
            start_limit_interval_sec: None,
            sockets: Vec::new(),
            send_sighup: false,
            slice: None,
            delegate: false,
            exec_stop_post: Vec::new(),
            file_descriptor_store_max: None,
            restart_prevent_exit_status: Vec::new(),
        }
    }
}

/// [Install] section
#[derive(Debug, Clone, Default)]
pub struct InstallSection {
    pub wanted_by: Vec<String>,
    pub required_by: Vec<String>,
    /// Additional units to enable/disable together with this unit
    pub also: Vec<String>,
    /// Symlink aliases for this unit
    pub alias: Vec<String>,
    /// Default instance for template units (foo@.service)
    pub default_instance: Option<String>,
}

/// Complete parsed service unit
#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    /// Instance name for template units (the part after @ in foo@bar.service)
    pub instance: Option<String>,
    pub unit: UnitSection,
    pub service: ServiceSection,
    pub install: InstallSection,
}

impl Service {
    pub fn new(name: String) -> Self {
        let instance = extract_instance(&name);
        Self {
            name,
            instance,
            unit: UnitSection::default(),
            service: ServiceSection::default(),
            install: InstallSection::default(),
        }
    }
}

/// Extract instance name from a unit name (e.g., "foo@bar.service" -> Some("bar"))
pub fn extract_instance(name: &str) -> Option<String> {
    // Find @ in the name (before any suffix like .service)
    let at_pos = name.find('@')?;

    // Find where the instance ends (at the suffix or end)
    let suffix_start = name.rfind('.').unwrap_or(name.len());

    // Instance is between @ and the suffix
    if at_pos + 1 < suffix_start {
        Some(name[at_pos + 1..suffix_start].to_string())
    } else {
        None // Template file (foo@.service) has no instance
    }
}

/// Get the template name from an instantiated unit name
/// e.g., "foo@bar.service" -> "foo@.service"
pub fn get_template_name(name: &str) -> Option<String> {
    let at_pos = name.find('@')?;
    let suffix_start = name.rfind('.')?;

    // Template is everything before @ plus @ plus the suffix
    Some(format!("{}@{}", &name[..at_pos], &name[suffix_start..]))
}

/// Check if a name is a bare template (e.g., "foo@.service" with no instance)
/// Returns true if the name has "@." pattern (nothing between @ and suffix)
pub fn is_bare_template(name: &str) -> bool {
    name.contains("@.")
}

/// Create an instantiated unit name from a template and instance
/// e.g., ("foo@.service", "bar") -> "foo@bar.service"
pub fn instantiate_template(template: &str, instance: &str) -> Option<String> {
    if !is_bare_template(template) {
        return None;
    }
    Some(template.replace("@.", &format!("@{}.", instance)))
}

// Parsing helpers

impl ServiceType {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "simple" => Some(Self::Simple),
            "forking" => Some(Self::Forking),
            "notify" => Some(Self::Notify),
            "dbus" => Some(Self::Dbus),
            "oneshot" => Some(Self::Oneshot),
            "idle" => Some(Self::Idle),
            _ => None,
        }
    }
}

impl RestartPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "no" => Some(Self::No),
            "on-failure" => Some(Self::OnFailure),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

impl StdOutput {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "journal" => Some(Self::Journal),
            "inherit" => Some(Self::Inherit),
            "null" | "/dev/null" => Some(Self::Null),
            _ => None,
        }
    }
}

/// Parse duration from systemd format (e.g., "5s", "100ms", "1min", "1d", "1w")
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();

    // Try common suffixes (order matters: check longer suffixes first)
    if let Some(n) = s.strip_suffix("ms") {
        n.parse().ok().map(Duration::from_millis)
    } else if let Some(n) = s.strip_suffix("min") {
        n.parse::<u64>().ok().map(|m| Duration::from_secs(m * 60))
    } else if let Some(n) = s.strip_suffix("sec") {
        n.parse().ok().map(Duration::from_secs)
    } else if let Some(n) = s.strip_suffix("week") {
        n.parse::<u64>().ok().map(|w| Duration::from_secs(w * 7 * 86400))
    } else if let Some(n) = s.strip_suffix('s') {
        n.parse().ok().map(Duration::from_secs)
    } else if let Some(n) = s.strip_suffix('h') {
        n.parse::<u64>().ok().map(|h| Duration::from_secs(h * 3600))
    } else if let Some(n) = s.strip_suffix('d') {
        n.parse::<u64>().ok().map(|d| Duration::from_secs(d * 86400))
    } else if let Some(n) = s.strip_suffix('w') {
        n.parse::<u64>().ok().map(|w| Duration::from_secs(w * 7 * 86400))
    } else {
        // Bare number = seconds
        s.parse().ok().map(Duration::from_secs)
    }
}

/// Parse memory size (e.g., "512M", "1G", "1073741824")
pub fn parse_memory(s: &str) -> Option<u64> {
    let s = s.trim();

    if let Some(n) = s.strip_suffix('G') {
        n.parse::<u64>().ok().map(|g| g * 1024 * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix('M') {
        n.parse::<u64>().ok().map(|m| m * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix('K') {
        n.parse::<u64>().ok().map(|k| k * 1024)
    } else {
        s.parse().ok()
    }
}

/// Parse CPU quota (e.g., "50%", "200%")
pub fn parse_cpu_quota(s: &str) -> Option<u32> {
    s.strip_suffix('%')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ServiceType tests
    #[test]
    fn test_service_type_parse() {
        assert_eq!(ServiceType::parse("simple"), Some(ServiceType::Simple));
        assert_eq!(ServiceType::parse("SIMPLE"), Some(ServiceType::Simple));
        assert_eq!(ServiceType::parse("Simple"), Some(ServiceType::Simple));
        assert_eq!(ServiceType::parse("forking"), Some(ServiceType::Forking));
        assert_eq!(ServiceType::parse("notify"), Some(ServiceType::Notify));
        assert_eq!(ServiceType::parse("dbus"), Some(ServiceType::Dbus));
        assert_eq!(ServiceType::parse("oneshot"), Some(ServiceType::Oneshot));
        assert_eq!(ServiceType::parse("invalid"), None);
        assert_eq!(ServiceType::parse(""), None);
    }

    #[test]
    fn test_service_type_default() {
        assert_eq!(ServiceType::default(), ServiceType::Simple);
    }

    // RestartPolicy tests
    #[test]
    fn test_restart_policy_parse() {
        assert_eq!(RestartPolicy::parse("no"), Some(RestartPolicy::No));
        assert_eq!(RestartPolicy::parse("NO"), Some(RestartPolicy::No));
        assert_eq!(
            RestartPolicy::parse("on-failure"),
            Some(RestartPolicy::OnFailure)
        );
        assert_eq!(
            RestartPolicy::parse("ON-FAILURE"),
            Some(RestartPolicy::OnFailure)
        );
        assert_eq!(RestartPolicy::parse("always"), Some(RestartPolicy::Always));
        assert_eq!(RestartPolicy::parse("ALWAYS"), Some(RestartPolicy::Always));
        assert_eq!(RestartPolicy::parse("invalid"), None);
        assert_eq!(RestartPolicy::parse(""), None);
    }

    #[test]
    fn test_restart_policy_default() {
        assert_eq!(RestartPolicy::default(), RestartPolicy::No);
    }

    // StdOutput tests
    #[test]
    fn test_std_output_parse() {
        assert_eq!(StdOutput::parse("journal"), Some(StdOutput::Journal));
        assert_eq!(StdOutput::parse("JOURNAL"), Some(StdOutput::Journal));
        assert_eq!(StdOutput::parse("inherit"), Some(StdOutput::Inherit));
        assert_eq!(StdOutput::parse("null"), Some(StdOutput::Null));
        assert_eq!(StdOutput::parse("/dev/null"), Some(StdOutput::Null));
        assert_eq!(StdOutput::parse("invalid"), None);
    }

    #[test]
    fn test_std_output_default() {
        assert_eq!(StdOutput::default(), StdOutput::Journal);
    }

    // Duration parsing tests
    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("100ms"), Some(Duration::from_millis(100)));
        assert_eq!(parse_duration("2min"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_parse_duration_edge_cases() {
        assert_eq!(parse_duration("0"), Some(Duration::from_secs(0)));
        assert_eq!(parse_duration("0s"), Some(Duration::from_secs(0)));
        assert_eq!(parse_duration("0ms"), Some(Duration::from_millis(0)));
        assert_eq!(parse_duration("  5s  "), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("invalid"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("5x"), None);
    }

    // Memory parsing tests
    #[test]
    fn test_parse_memory() {
        assert_eq!(parse_memory("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_memory("512M"), Some(512 * 1024 * 1024));
        assert_eq!(parse_memory("1024K"), Some(1024 * 1024));
        assert_eq!(parse_memory("1048576"), Some(1048576));
    }

    #[test]
    fn test_parse_memory_edge_cases() {
        assert_eq!(parse_memory("0"), Some(0));
        assert_eq!(parse_memory("  1G  "), Some(1024 * 1024 * 1024));
        assert_eq!(parse_memory("invalid"), None);
        assert_eq!(parse_memory(""), None);
        assert_eq!(parse_memory("1T"), None); // T not supported
    }

    // CPU quota tests
    #[test]
    fn test_parse_cpu_quota() {
        assert_eq!(parse_cpu_quota("50%"), Some(50));
        assert_eq!(parse_cpu_quota("200%"), Some(200));
        assert_eq!(parse_cpu_quota("0%"), Some(0));
        assert_eq!(parse_cpu_quota("100"), None);
        assert_eq!(parse_cpu_quota(""), None);
        assert_eq!(parse_cpu_quota("invalid%"), None);
    }

    // ServiceSection default tests
    #[test]
    fn test_service_section_default() {
        let section = ServiceSection::default();
        assert_eq!(section.service_type, ServiceType::Simple);
        assert_eq!(section.restart, RestartPolicy::No);
        assert_eq!(section.restart_sec, Duration::from_millis(100));
        assert!(section.exec_start.is_empty());
        assert!(section.user.is_none());
    }

    // Service tests
    #[test]
    fn test_service_new() {
        let svc = Service::new("test.service".to_string());
        assert_eq!(svc.name, "test.service");
        assert!(svc.unit.description.is_none());
        assert!(svc.unit.after.is_empty());
        assert!(svc.install.wanted_by.is_empty());
    }

    // Template unit tests
    #[test]
    fn test_extract_instance() {
        assert_eq!(extract_instance("foo@bar.service"), Some("bar".to_string()));
        assert_eq!(
            extract_instance("getty@tty1.service"),
            Some("tty1".to_string())
        );
        assert_eq!(extract_instance("foo@.service"), None); // Template file
        assert_eq!(extract_instance("foo.service"), None); // Not a template
        assert_eq!(extract_instance("foo@bar"), Some("bar".to_string()));
    }

    #[test]
    fn test_get_template_name() {
        assert_eq!(
            get_template_name("foo@bar.service"),
            Some("foo@.service".to_string())
        );
        assert_eq!(
            get_template_name("getty@tty1.service"),
            Some("getty@.service".to_string())
        );
        assert_eq!(
            get_template_name("foo@.service"),
            Some("foo@.service".to_string())
        );
        assert_eq!(get_template_name("foo.service"), None); // Not a template
    }

    #[test]
    fn test_service_new_with_instance() {
        let svc = Service::new("getty@tty1.service".to_string());
        assert_eq!(svc.name, "getty@tty1.service");
        assert_eq!(svc.instance, Some("tty1".to_string()));

        let svc2 = Service::new("foo.service".to_string());
        assert_eq!(svc2.instance, None);
    }

    #[test]
    fn test_is_bare_template() {
        assert!(is_bare_template("foo@.service"));
        assert!(is_bare_template("getty@.service"));
        assert!(!is_bare_template("foo@bar.service"));
        assert!(!is_bare_template("foo.service"));
    }

    #[test]
    fn test_instantiate_template() {
        assert_eq!(
            instantiate_template("foo@.service", "bar"),
            Some("foo@bar.service".to_string())
        );
        assert_eq!(
            instantiate_template("getty@.service", "tty1"),
            Some("getty@tty1.service".to_string())
        );
        // Not a bare template - returns None
        assert_eq!(instantiate_template("foo@bar.service", "baz"), None);
        assert_eq!(instantiate_template("foo.service", "bar"), None);
    }
}
