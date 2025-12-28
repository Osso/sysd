//! Typed service definitions matching systemd .service files
//!
//! Structures match directives listed in doc/DESIGN.md

use std::path::PathBuf;
use std::time::Duration;

/// Service type determines startup notification
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ServiceType {
    #[default]
    Simple,   // Ready immediately after exec
    Forking,  // Ready when main process exits
    Notify,   // Ready on sd_notify READY=1
    Dbus,     // Ready when D-Bus name acquired
    Oneshot,  // Run once, no main process
}

/// Restart policy
#[derive(Debug, Clone, Default, PartialEq)]
pub enum RestartPolicy {
    #[default]
    No,
    OnFailure,
    Always,
}

/// Output destination
#[derive(Debug, Clone, Default, PartialEq)]
pub enum StdOutput {
    #[default]
    Journal,
    Inherit,
    Null,
}

/// [Unit] section
#[derive(Debug, Clone, Default)]
pub struct UnitSection {
    pub description: Option<String>,
    pub after: Vec<String>,
    pub before: Vec<String>,
    pub requires: Vec<String>,
    pub wants: Vec<String>,
    pub conflicts: Vec<String>,
    pub condition_path_exists: Vec<String>,
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
    pub restart_sec: Duration,  // Default: 100ms per systemd docs
    pub timeout_start_sec: Option<Duration>,
    pub timeout_stop_sec: Option<Duration>,

    // Credentials
    pub user: Option<String>,
    pub group: Option<String>,
    pub working_directory: Option<PathBuf>,

    // Environment
    pub environment: Vec<(String, String)>,
    pub environment_file: Vec<PathBuf>,

    // I/O
    pub standard_output: StdOutput,
    pub standard_error: StdOutput,

    // Resource limits (cgroup v2)
    pub memory_max: Option<u64>,      // bytes
    pub cpu_quota: Option<u32>,       // percentage (100 = 1 core)
    pub tasks_max: Option<u32>,
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
            user: None,
            group: None,
            working_directory: None,
            environment: Vec::new(),
            environment_file: Vec::new(),
            standard_output: StdOutput::default(),
            standard_error: StdOutput::default(),
            memory_max: None,
            cpu_quota: None,
            tasks_max: None,
        }
    }
}

/// [Install] section
#[derive(Debug, Clone, Default)]
pub struct InstallSection {
    pub wanted_by: Vec<String>,
    pub required_by: Vec<String>,
}

/// Complete parsed service unit
#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub unit: UnitSection,
    pub service: ServiceSection,
    pub install: InstallSection,
}

impl Service {
    pub fn new(name: String) -> Self {
        Self {
            name,
            unit: UnitSection::default(),
            service: ServiceSection::default(),
            install: InstallSection::default(),
        }
    }
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

/// Parse duration from systemd format (e.g., "5s", "100ms", "1min")
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();

    // Try common suffixes
    if let Some(n) = s.strip_suffix("ms") {
        n.parse().ok().map(Duration::from_millis)
    } else if let Some(n) = s.strip_suffix("s") {
        n.parse().ok().map(Duration::from_secs)
    } else if let Some(n) = s.strip_suffix("min") {
        n.parse::<u64>().ok().map(|m| Duration::from_secs(m * 60))
    } else if let Some(n) = s.strip_suffix("h") {
        n.parse::<u64>().ok().map(|h| Duration::from_secs(h * 3600))
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

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("100ms"), Some(Duration::from_millis(100)));
        assert_eq!(parse_duration("2min"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_parse_memory() {
        assert_eq!(parse_memory("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_memory("512M"), Some(512 * 1024 * 1024));
        assert_eq!(parse_memory("1024K"), Some(1024 * 1024));
        assert_eq!(parse_memory("1048576"), Some(1048576));
    }

    #[test]
    fn test_parse_cpu_quota() {
        assert_eq!(parse_cpu_quota("50%"), Some(50));
        assert_eq!(parse_cpu_quota("200%"), Some(200));
        assert_eq!(parse_cpu_quota("100"), None);
    }
}
