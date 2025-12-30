//! Unit file parsing and type definitions
//!
//! Parses systemd .service and .target files into typed Rust structures.

mod parser;
mod service;
mod target;
mod unit;

pub use parser::{parse_file, parse_unit_file, ParseError, ParsedFile};
pub use service::*;
pub use target::Target;
pub use unit::Unit;

use std::path::Path;

/// Convert parsed INI data into a typed Service
pub fn parse_service(name: &str, parsed: &ParsedFile) -> Result<Service, ParseError> {
    let mut svc = Service::new(name.to_string());

    // [Unit] section
    if let Some(unit) = parsed.get("[Unit]") {
        if let Some(vals) = unit.get("DESCRIPTION") {
            svc.unit.description = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = unit.get("AFTER") {
            svc.unit.after = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("BEFORE") {
            svc.unit.before = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("REQUIRES") {
            svc.unit.requires = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("WANTS") {
            svc.unit.wants = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONFLICTS") {
            svc.unit.conflicts = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONPATHEXISTS") {
            svc.unit.condition_path_exists = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONDIRECTORYNOTEMPTY") {
            svc.unit.condition_directory_not_empty = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("DEFAULTDEPENDENCIES") {
            if let Some((_, s)) = vals.first() {
                svc.unit.default_dependencies =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
    }

    // [Service] section
    if let Some(service) = parsed.get("[Service]") {
        // Type
        if let Some(vals) = service.get("TYPE") {
            if let Some((_, t)) = vals.first() {
                svc.service.service_type = ServiceType::parse(t).unwrap_or_default();
            }
        }

        // Exec commands
        if let Some(vals) = service.get("EXECSTART") {
            svc.service.exec_start = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = service.get("EXECSTARTPRE") {
            svc.service.exec_start_pre = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = service.get("EXECSTARTPOST") {
            svc.service.exec_start_post = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = service.get("EXECSTOP") {
            svc.service.exec_stop = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = service.get("EXECRELOAD") {
            svc.service.exec_reload = vals.iter().map(|(_, v)| v.clone()).collect();
        }

        // Restart
        if let Some(vals) = service.get("RESTART") {
            if let Some((_, r)) = vals.first() {
                svc.service.restart = RestartPolicy::parse(r).unwrap_or_default();
            }
        }
        if let Some(vals) = service.get("RESTARTSEC") {
            if let Some((_, s)) = vals.first() {
                svc.service.restart_sec =
                    parse_duration(s).unwrap_or(std::time::Duration::from_millis(100));
            }
        }
        if let Some(vals) = service.get("TIMEOUTSTARTSEC") {
            if let Some((_, s)) = vals.first() {
                svc.service.timeout_start_sec = parse_duration(s);
            }
        }
        if let Some(vals) = service.get("TIMEOUTSTOPSEC") {
            if let Some((_, s)) = vals.first() {
                svc.service.timeout_stop_sec = parse_duration(s);
            }
        }
        if let Some(vals) = service.get("REMAINAFTEREXIT") {
            if let Some((_, s)) = vals.first() {
                svc.service.remain_after_exit =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = service.get("WATCHDOGSEC") {
            if let Some((_, s)) = vals.first() {
                svc.service.watchdog_sec = parse_duration(s);
            }
        }
        if let Some(vals) = service.get("PIDFILE") {
            svc.service.pid_file = vals.first().map(|(_, v)| v.into());
        }
        if let Some(vals) = service.get("BUSNAME") {
            svc.service.bus_name = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = service.get("KILLMODE") {
            if let Some((_, s)) = vals.first() {
                svc.service.kill_mode = KillMode::parse(s).unwrap_or_default();
            }
        }

        // Credentials
        if let Some(vals) = service.get("USER") {
            svc.service.user = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = service.get("GROUP") {
            svc.service.group = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = service.get("WORKINGDIRECTORY") {
            svc.service.working_directory = vals.first().map(|(_, v)| v.into());
        }

        // Environment
        if let Some(vals) = service.get("ENVIRONMENT") {
            for (_, v) in vals {
                if let Ok(pairs) = parser::parse_environment(v) {
                    svc.service.environment.extend(pairs);
                }
            }
        }
        if let Some(vals) = service.get("ENVIRONMENTFILE") {
            svc.service.environment_file = vals.iter().map(|(_, v)| v.into()).collect();
        }

        // I/O
        if let Some(vals) = service.get("STANDARDOUTPUT") {
            if let Some((_, s)) = vals.first() {
                svc.service.standard_output = StdOutput::parse(s).unwrap_or_default();
            }
        }
        if let Some(vals) = service.get("STANDARDERROR") {
            if let Some((_, s)) = vals.first() {
                svc.service.standard_error = StdOutput::parse(s).unwrap_or_default();
            }
        }
        if let Some(vals) = service.get("STANDARDINPUT") {
            if let Some((_, s)) = vals.first() {
                svc.service.standard_input = StdInput::parse(s).unwrap_or_default();
            }
        }

        // TTY handling (for getty and similar)
        if let Some(vals) = service.get("TTYPATH") {
            svc.service.tty_path = vals.first().map(|(_, v)| v.into());
        }
        if let Some(vals) = service.get("TTYRESET") {
            if let Some((_, s)) = vals.first() {
                svc.service.tty_reset =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }

        // Resource limits
        if let Some(vals) = service.get("MEMORYMAX") {
            if let Some((_, s)) = vals.first() {
                svc.service.memory_max = parse_memory(s);
            }
        }
        if let Some(vals) = service.get("CPUQUOTA") {
            if let Some((_, s)) = vals.first() {
                svc.service.cpu_quota = parse_cpu_quota(s);
            }
        }
        if let Some(vals) = service.get("TASKSMAX") {
            if let Some((_, s)) = vals.first() {
                svc.service.tasks_max = s.parse().ok();
            }
        }

        // Process limits (setrlimit)
        if let Some(vals) = service.get("LIMITNOFILE") {
            if let Some((_, s)) = vals.first() {
                // LimitNOFILE can be "infinity" or a number
                svc.service.limit_nofile = if s.to_lowercase() == "infinity" {
                    Some(u64::MAX)
                } else {
                    s.parse().ok()
                };
            }
        }

        // OOM killer adjustment
        if let Some(vals) = service.get("OOMSCOREADJUST") {
            if let Some((_, s)) = vals.first() {
                svc.service.oom_score_adjust = s.parse().ok();
            }
        }
    }

    // [Install] section
    if let Some(install) = parsed.get("[Install]") {
        if let Some(vals) = install.get("WANTEDBY") {
            svc.install.wanted_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("REQUIREDBY") {
            svc.install.required_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("ALSO") {
            svc.install.also = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("ALIAS") {
            svc.install.alias = vals.iter().map(|(_, v)| v.clone()).collect();
        }
    }

    Ok(svc)
}

/// Parse a service file from disk
pub async fn load_service(path: &Path) -> Result<Service, ParseError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut parsed = parse_unit_file(path).await?;

    // Load and merge drop-in files
    load_dropins(path, &mut parsed).await;

    parse_service(name, &parsed)
}

/// Find and load drop-in configuration files (.d/*.conf)
/// Drop-ins are read from <unit>.d/ directories in /etc/systemd/system and /usr/lib/systemd/system
async fn load_dropins(unit_path: &Path, parsed: &mut ParsedFile) {
    let unit_name = match unit_path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return,
    };

    // Look for drop-in directories
    let dropin_dirs = [
        Path::new("/etc/systemd/system").join(format!("{}.d", unit_name)),
        Path::new("/usr/lib/systemd/system").join(format!("{}.d", unit_name)),
        // Also check relative to the unit file location
        unit_path
            .parent()
            .map(|p| p.join(format!("{}.d", unit_name)))
            .unwrap_or_default(),
    ];

    let mut conf_files: Vec<std::path::PathBuf> = Vec::new();

    for dir in &dropin_dirs {
        if dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map(|e| e == "conf").unwrap_or(false) {
                        conf_files.push(path);
                    }
                }
            }
        }
    }

    // Sort by filename to ensure deterministic order
    conf_files.sort();

    // Load and merge each drop-in
    for conf_path in conf_files {
        match parse_unit_file(&conf_path).await {
            Ok(dropin) => {
                log::debug!("Loaded drop-in: {}", conf_path.display());
                merge_parsed_files(parsed, &dropin);
            }
            Err(e) => {
                log::warn!("Failed to parse drop-in {}: {}", conf_path.display(), e);
            }
        }
    }
}

/// Merge a drop-in ParsedFile into the main ParsedFile
/// Drop-in values are appended to base values (for list directives like After=)
/// or replace them (for scalar directives, the last value wins during conversion)
fn merge_parsed_files(base: &mut ParsedFile, dropin: &ParsedFile) {
    for (section_name, section_values) in dropin {
        let base_section = base.entry(section_name.clone()).or_default();

        for (key, values) in section_values {
            // Append all values from drop-in
            // Note: For scalar directives, parse_service uses .first() so the last
            // value from the base takes precedence (systemd uses last value instead)
            // For list directives (After=, Wants=, etc.), all values are collected
            base_section
                .entry(key.clone())
                .or_default()
                .extend(values.clone());
        }
    }
}

/// Convert parsed INI data into a typed Target
pub fn parse_target(name: &str, parsed: &ParsedFile) -> Result<Target, ParseError> {
    let mut target = Target::new(name.to_string());

    // [Unit] section - same as Service
    if let Some(unit) = parsed.get("[Unit]") {
        if let Some(vals) = unit.get("DESCRIPTION") {
            target.unit.description = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = unit.get("AFTER") {
            target.unit.after = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("BEFORE") {
            target.unit.before = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("REQUIRES") {
            target.unit.requires = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("WANTS") {
            target.unit.wants = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONFLICTS") {
            target.unit.conflicts = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONPATHEXISTS") {
            target.unit.condition_path_exists = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONDIRECTORYNOTEMPTY") {
            target.unit.condition_directory_not_empty =
                vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("DEFAULTDEPENDENCIES") {
            if let Some((_, s)) = vals.first() {
                target.unit.default_dependencies =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
    }

    Ok(target)
}

/// Parse a target file from disk, including .wants directory
pub async fn load_target(path: &Path) -> Result<Target, ParseError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut parsed = parse_unit_file(path).await?;

    // Load and merge drop-in files
    load_dropins(path, &mut parsed).await;

    let mut target = parse_target(name, &parsed)?;

    // Look for .wants directory in same location
    let wants_dir = path.with_extension("target.wants");
    if wants_dir.is_dir() {
        target.wants_dir = read_wants_dir(&wants_dir);
    }

    // Also check /etc/systemd/system/<target>.wants
    let etc_wants = Path::new("/etc/systemd/system").join(format!("{}.wants", name));
    if etc_wants.is_dir() {
        target.wants_dir.extend(read_wants_dir(&etc_wants));
    }

    Ok(target)
}

/// Read unit names from a .wants directory
fn read_wants_dir(path: &Path) -> Vec<String> {
    let mut units = Vec::new();

    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                // Include all unit types we might encounter
                // For now we only fully support .service and .target
                // but we should track .path, .socket, .mount etc for deps
                if name.ends_with(".service")
                    || name.ends_with(".target")
                    || name.ends_with(".path")
                    || name.ends_with(".socket")
                    || name.ends_with(".mount")
                {
                    units.push(name.to_string());
                }
            }
        }
    }

    units
}

/// Load a unit file (service or target) from disk
pub async fn load_unit(path: &Path) -> Result<Unit, ParseError> {
    let ext = path.extension().and_then(|e| e.to_str());

    match ext {
        Some("service") => {
            let service = load_service(path).await?;
            Ok(Unit::Service(service))
        }
        Some("target") => {
            let target = load_target(path).await?;
            Ok(Unit::Target(target))
        }
        _ => Err(ParseError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Unknown unit type: {:?}", path),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_docker_service() {
        let content = r#"
[Unit]
Description=Docker Application Container Engine
After=network-online.target docker.socket firewalld.service
Wants=network-online.target
Requires=docker.socket

[Service]
Type=notify
ExecStart=/usr/bin/dockerd -H fd://
ExecReload=/bin/kill -s HUP $MAINPID
TimeoutStartSec=0
RestartSec=2
Restart=always
MemoryMax=2G

[Install]
WantedBy=multi-user.target
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("docker.service", &parsed).unwrap();

        assert_eq!(
            svc.unit.description,
            Some("Docker Application Container Engine".into())
        );
        assert!(svc.unit.after.contains(&"network-online.target".into()));
        assert!(svc.unit.wants.contains(&"network-online.target".into()));
        assert!(svc.unit.requires.contains(&"docker.socket".into()));

        assert_eq!(svc.service.service_type, ServiceType::Notify);
        assert_eq!(svc.service.restart, RestartPolicy::Always);
        assert_eq!(svc.service.restart_sec, std::time::Duration::from_secs(2));
        assert_eq!(svc.service.memory_max, Some(2 * 1024 * 1024 * 1024));

        assert!(svc.install.wanted_by.contains(&"multi-user.target".into()));
    }

    #[test]
    fn test_parse_simple_service() {
        let content = r#"
[Unit]
Description=My App

[Service]
Type=simple
ExecStart=/usr/bin/myapp --flag
User=nobody
WorkingDirectory=/var/lib/myapp
Environment=FOO=bar BAZ=qux

[Install]
WantedBy=multi-user.target
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("myapp.service", &parsed).unwrap();

        assert_eq!(svc.service.service_type, ServiceType::Simple);
        assert_eq!(svc.service.user, Some("nobody".into()));
        assert_eq!(svc.service.working_directory, Some("/var/lib/myapp".into()));
        assert!(svc
            .service
            .environment
            .contains(&("FOO".into(), "bar".into())));
        assert!(svc
            .service
            .environment
            .contains(&("BAZ".into(), "qux".into())));
    }

    #[test]
    fn test_parse_oneshot_remain_after_exit() {
        let content = r#"
[Unit]
Description=Run once at boot

[Service]
Type=oneshot
ExecStart=/usr/bin/setup-something
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("setup.service", &parsed).unwrap();

        assert_eq!(svc.service.service_type, ServiceType::Oneshot);
        assert!(svc.service.remain_after_exit);
    }

    #[test]
    fn test_parse_restart_on_failure() {
        let content = r#"
[Service]
Type=simple
ExecStart=/usr/bin/myapp
Restart=on-failure
RestartSec=5s
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("myapp.service", &parsed).unwrap();

        assert_eq!(svc.service.restart, RestartPolicy::OnFailure);
        assert_eq!(svc.service.restart_sec, std::time::Duration::from_secs(5));
    }

    #[test]
    fn test_parse_dbus_service() {
        let content = r#"
[Unit]
Description=D-Bus Activated Service

[Service]
Type=dbus
BusName=org.example.MyService
ExecStart=/usr/bin/my-dbus-service

[Install]
WantedBy=multi-user.target
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("my-dbus.service", &parsed).unwrap();

        assert_eq!(svc.service.service_type, ServiceType::Dbus);
        assert_eq!(svc.service.bus_name, Some("org.example.MyService".into()));
    }

    #[test]
    fn test_parse_idle_service() {
        let content = r#"
[Unit]
Description=Getty on tty1

[Service]
Type=idle
ExecStart=/sbin/agetty -o '-p -- \\u' --noclear - $TERM

[Install]
WantedBy=multi-user.target
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("getty@tty1.service", &parsed).unwrap();

        assert_eq!(svc.service.service_type, ServiceType::Idle);
    }

    #[test]
    fn test_parse_default_dependencies() {
        // DefaultDependencies=yes (default)
        let content = r#"
[Unit]
Description=Normal service

[Service]
ExecStart=/bin/true
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("normal.service", &parsed).unwrap();
        assert!(svc.unit.default_dependencies);

        // DefaultDependencies=no
        let content = r#"
[Unit]
Description=Early boot service
DefaultDependencies=no

[Service]
ExecStart=/bin/true
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("early.service", &parsed).unwrap();
        assert!(!svc.unit.default_dependencies);
    }

    #[test]
    fn test_parse_condition_directory_not_empty() {
        let content = r#"
[Unit]
Description=Runs if directory has files
ConditionDirectoryNotEmpty=/etc/modules-load.d

[Service]
ExecStart=/bin/true
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("conditional.service", &parsed).unwrap();

        assert_eq!(
            svc.unit.condition_directory_not_empty,
            vec!["/etc/modules-load.d"]
        );
    }

    #[test]
    fn test_parse_install_also_and_alias() {
        let content = r#"
[Unit]
Description=Socket activation service

[Service]
ExecStart=/usr/bin/myservice

[Install]
WantedBy=multi-user.target
Also=myservice.socket
Alias=myservice-alt.service
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("myservice.service", &parsed).unwrap();

        assert!(svc.install.wanted_by.contains(&"multi-user.target".into()));
        assert!(svc.install.also.contains(&"myservice.socket".into()));
        assert!(svc.install.alias.contains(&"myservice-alt.service".into()));
    }

    #[test]
    fn test_merge_dropins() {
        // Base service
        let base_content = r#"
[Unit]
Description=Base service
After=network.target

[Service]
ExecStart=/usr/bin/myservice
"#;
        let mut base = parse_file(base_content).unwrap();

        // Drop-in that adds more dependencies
        let dropin_content = r#"
[Unit]
After=remote-fs.target

[Service]
Environment=FOO=bar
"#;
        let dropin = parse_file(dropin_content).unwrap();

        merge_parsed_files(&mut base, &dropin);

        // After should now have both values
        let unit_section = base.get("[Unit]").unwrap();
        let after_vals = unit_section.get("AFTER").unwrap();
        assert_eq!(after_vals.len(), 2);
        assert!(after_vals.iter().any(|(_, v)| v == "network.target"));
        assert!(after_vals.iter().any(|(_, v)| v == "remote-fs.target"));

        // Environment should be added
        let svc_section = base.get("[Service]").unwrap();
        let env_vals = svc_section.get("ENVIRONMENT").unwrap();
        assert_eq!(env_vals.len(), 1);
        assert_eq!(env_vals[0].1, "FOO=bar");
    }

    #[test]
    fn test_merge_dropins_append() {
        // Test that drop-in values are appended
        // Note: Reset via empty value (ExecStart=) is not supported by the parser
        let base_content = r#"
[Service]
ExecStart=/usr/bin/main
"#;
        let mut base = parse_file(base_content).unwrap();

        // Drop-in that adds ExecStartPre
        let dropin_content = r#"
[Service]
ExecStartPre=/usr/bin/setup
"#;
        let dropin = parse_file(dropin_content).unwrap();

        merge_parsed_files(&mut base, &dropin);

        let svc_section = base.get("[Service]").unwrap();

        // Original ExecStart preserved
        let exec_start = svc_section.get("EXECSTART").unwrap();
        assert_eq!(exec_start.len(), 1);
        assert!(exec_start[0].1.contains("/usr/bin/main"));

        // Drop-in ExecStartPre added
        let exec_pre = svc_section.get("EXECSTARTPRE").unwrap();
        assert_eq!(exec_pre.len(), 1);
        assert!(exec_pre[0].1.contains("/usr/bin/setup"));
    }
}
