//! Unit file parsing and type definitions
//!
//! Parses systemd .service, .target, and .mount files into typed Rust structures.

mod mount;
mod parser;
mod service;
mod slice;
mod socket;
mod target;
mod timer;
mod unit;

pub use mount::Mount;
pub use parser::{parse_file, parse_unit_file, ParseError, ParsedFile};
pub use service::*;
pub use slice::Slice;
pub use socket::{ListenType, Listener, Socket, SocketSection};
pub use target::Target;
pub use timer::{CalendarSpec, Timer, TimerSection};
pub use unit::Unit;

use std::path::{Path, PathBuf};

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

        // Security sandboxing
        if let Some(vals) = service.get("NONEWPRIVILEGES") {
            if let Some((_, s)) = vals.first() {
                svc.service.no_new_privileges =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = service.get("PROTECTSYSTEM") {
            if let Some((_, s)) = vals.first() {
                svc.service.protect_system = ProtectSystem::parse(s).unwrap_or_default();
            }
        }
        if let Some(vals) = service.get("PROTECTHOME") {
            if let Some((_, s)) = vals.first() {
                svc.service.protect_home = ProtectHome::parse(s).unwrap_or_default();
            }
        }
        if let Some(vals) = service.get("PRIVATETMP") {
            if let Some((_, s)) = vals.first() {
                svc.service.private_tmp =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = service.get("PRIVATEDEVICES") {
            if let Some((_, s)) = vals.first() {
                svc.service.private_devices =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = service.get("PRIVATENETWORK") {
            if let Some((_, s)) = vals.first() {
                svc.service.private_network =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = service.get("PROTECTKERNELMODULES") {
            if let Some((_, s)) = vals.first() {
                svc.service.protect_kernel_modules =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = service.get("PROTECTPROC") {
            if let Some((_, s)) = vals.first() {
                svc.service.protect_proc = ProtectProc::parse(s).unwrap_or_default();
            }
        }

        // Capabilities (space-separated list, possibly with ~ prefix for drop)
        if let Some(vals) = service.get("CAPABILITYBOUNDINGSET") {
            for (_, v) in vals {
                svc.service.capability_bounding_set.extend(
                    v.split_whitespace().map(|s| s.to_string())
                );
            }
        }
        if let Some(vals) = service.get("AMBIENTCAPABILITIES") {
            for (_, v) in vals {
                svc.service.ambient_capabilities.extend(
                    v.split_whitespace().map(|s| s.to_string())
                );
            }
        }

        // RestrictNamespaces (true/false or space-separated list)
        if let Some(vals) = service.get("RESTRICTNAMESPACES") {
            if let Some((_, s)) = vals.first() {
                let lower = s.to_lowercase();
                if matches!(lower.as_str(), "yes" | "true" | "1" | "on") {
                    // Block all namespaces
                    svc.service.restrict_namespaces = Some(Vec::new());
                } else if matches!(lower.as_str(), "no" | "false" | "0" | "off") {
                    // Allow all namespaces (no restriction)
                    svc.service.restrict_namespaces = None;
                } else {
                    // Space-separated list of allowed/denied namespaces
                    svc.service.restrict_namespaces = Some(
                        s.split_whitespace().map(|s| s.to_string()).collect()
                    );
                }
            }
        }

        // Path restrictions
        if let Some(vals) = service.get("READWRITEPATHS") {
            for (_, v) in vals {
                svc.service.read_write_paths.extend(
                    v.split_whitespace().map(|s| PathBuf::from(s))
                );
            }
        }
        if let Some(vals) = service.get("READONLYPATHS") {
            for (_, v) in vals {
                svc.service.read_only_paths.extend(
                    v.split_whitespace().map(|s| PathBuf::from(s))
                );
            }
        }
        if let Some(vals) = service.get("INACCESSIBLEPATHS") {
            for (_, v) in vals {
                svc.service.inaccessible_paths.extend(
                    v.split_whitespace().map(|s| PathBuf::from(s))
                );
            }
        }

        // Seccomp system call filter
        if let Some(vals) = service.get("SYSTEMCALLFILTER") {
            for (_, v) in vals {
                svc.service.system_call_filter.extend(
                    v.split_whitespace().map(|s| s.to_string())
                );
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

/// Convert parsed INI data into a typed Slice
pub fn parse_slice(name: &str, parsed: &ParsedFile) -> Result<Slice, ParseError> {
    let mut slice = Slice::new(name.to_string());

    // [Unit] section - same as Target
    if let Some(unit) = parsed.get("[Unit]") {
        if let Some(vals) = unit.get("DESCRIPTION") {
            slice.unit.description = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = unit.get("AFTER") {
            slice.unit.after = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("BEFORE") {
            slice.unit.before = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("REQUIRES") {
            slice.unit.requires = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("WANTS") {
            slice.unit.wants = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONFLICTS") {
            slice.unit.conflicts = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONPATHEXISTS") {
            slice.unit.condition_path_exists = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONDIRECTORYNOTEMPTY") {
            slice.unit.condition_directory_not_empty =
                vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("DEFAULTDEPENDENCIES") {
            if let Some((_, s)) = vals.first() {
                slice.unit.default_dependencies =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
    }

    Ok(slice)
}

/// Parse a slice file from disk
pub async fn load_slice(path: &Path) -> Result<Slice, ParseError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut parsed = parse_unit_file(path).await?;

    // Load and merge drop-in files
    load_dropins(path, &mut parsed).await;

    parse_slice(name, &parsed)
}

/// Convert parsed INI data into a typed Mount
pub fn parse_mount(name: &str, parsed: &ParsedFile) -> Result<Mount, ParseError> {
    let mut mnt = Mount::new(name.to_string());

    // [Unit] section - same as Service/Target
    if let Some(unit) = parsed.get("[Unit]") {
        if let Some(vals) = unit.get("DESCRIPTION") {
            mnt.unit.description = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = unit.get("AFTER") {
            mnt.unit.after = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("BEFORE") {
            mnt.unit.before = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("REQUIRES") {
            mnt.unit.requires = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("WANTS") {
            mnt.unit.wants = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONFLICTS") {
            mnt.unit.conflicts = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONPATHEXISTS") {
            mnt.unit.condition_path_exists = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONDIRECTORYNOTEMPTY") {
            mnt.unit.condition_directory_not_empty =
                vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("DEFAULTDEPENDENCIES") {
            if let Some((_, s)) = vals.first() {
                mnt.unit.default_dependencies =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
    }

    // [Mount] section
    if let Some(mount) = parsed.get("[Mount]") {
        if let Some(vals) = mount.get("WHAT") {
            if let Some((_, v)) = vals.first() {
                mnt.mount.what = v.clone();
            }
        }
        if let Some(vals) = mount.get("WHERE") {
            if let Some((_, v)) = vals.first() {
                mnt.mount.r#where = v.clone();
            }
        }
        if let Some(vals) = mount.get("TYPE") {
            mnt.mount.fs_type = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = mount.get("OPTIONS") {
            mnt.mount.options = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = mount.get("SLOPPYOPTIONS") {
            if let Some((_, s)) = vals.first() {
                mnt.mount.sloppy_options =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = mount.get("LAZYUNMOUNT") {
            if let Some((_, s)) = vals.first() {
                mnt.mount.lazy_unmount =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = mount.get("FORCEUNMOUNT") {
            if let Some((_, s)) = vals.first() {
                mnt.mount.force_unmount =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = mount.get("READWRITEONLY") {
            if let Some((_, s)) = vals.first() {
                mnt.mount.read_write_only =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = mount.get("DIRECTORYMODE") {
            if let Some((_, s)) = vals.first() {
                // Parse octal mode (e.g., "0755" or "755")
                let s = s.trim_start_matches('0');
                mnt.mount.directory_mode = u32::from_str_radix(s, 8).ok();
            }
        }
        if let Some(vals) = mount.get("TIMEOUTSEC") {
            if let Some((_, s)) = vals.first() {
                mnt.mount.timeout_sec = parse_duration(s);
            }
        }
    }

    // If Where= is not specified, derive from unit name
    if mnt.mount.r#where.is_empty() {
        mnt.mount.r#where = Mount::mount_point_from_name(name);
    }

    // [Install] section
    if let Some(install) = parsed.get("[Install]") {
        if let Some(vals) = install.get("WANTEDBY") {
            mnt.install.wanted_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("REQUIREDBY") {
            mnt.install.required_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
    }

    Ok(mnt)
}

/// Parse a mount file from disk
pub async fn load_mount(path: &Path) -> Result<Mount, ParseError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut parsed = parse_unit_file(path).await?;

    // Load and merge drop-in files
    load_dropins(path, &mut parsed).await;

    parse_mount(name, &parsed)
}

/// Convert parsed INI data into a typed Socket
pub fn parse_socket(name: &str, parsed: &ParsedFile) -> Result<Socket, ParseError> {
    let mut sock = Socket::new(name.to_string());

    // [Unit] section
    if let Some(unit) = parsed.get("[Unit]") {
        if let Some(vals) = unit.get("DESCRIPTION") {
            sock.unit.description = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = unit.get("AFTER") {
            sock.unit.after = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("BEFORE") {
            sock.unit.before = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("REQUIRES") {
            sock.unit.requires = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("WANTS") {
            sock.unit.wants = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONFLICTS") {
            sock.unit.conflicts = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONPATHEXISTS") {
            sock.unit.condition_path_exists = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONDIRECTORYNOTEMPTY") {
            sock.unit.condition_directory_not_empty =
                vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("DEFAULTDEPENDENCIES") {
            if let Some((_, s)) = vals.first() {
                sock.unit.default_dependencies =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
    }

    // [Socket] section
    if let Some(socket) = parsed.get("[Socket]") {
        // Listeners - can have multiple
        if let Some(vals) = socket.get("LISTENSTREAM") {
            for (_, v) in vals {
                sock.socket.listeners.push(Listener {
                    address: v.clone(),
                    listen_type: ListenType::Stream,
                });
            }
        }
        if let Some(vals) = socket.get("LISTENDATAGRAM") {
            for (_, v) in vals {
                sock.socket.listeners.push(Listener {
                    address: v.clone(),
                    listen_type: ListenType::Datagram,
                });
            }
        }
        if let Some(vals) = socket.get("LISTENFIFO") {
            for (_, v) in vals {
                sock.socket.listeners.push(Listener {
                    address: v.clone(),
                    listen_type: ListenType::Fifo,
                });
            }
        }
        if let Some(vals) = socket.get("LISTENNETLINK") {
            for (_, v) in vals {
                sock.socket.listeners.push(Listener {
                    address: v.clone(),
                    listen_type: ListenType::Netlink,
                });
            }
        }

        // Accept mode
        if let Some(vals) = socket.get("ACCEPT") {
            if let Some((_, s)) = vals.first() {
                sock.socket.accept =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }

        // Service to activate
        if let Some(vals) = socket.get("SERVICE") {
            sock.socket.service = vals.first().map(|(_, v)| v.clone());
        }

        // Socket mode (permissions)
        if let Some(vals) = socket.get("SOCKETMODE") {
            if let Some((_, s)) = vals.first() {
                let s = s.trim_start_matches('0');
                sock.socket.socket_mode = u32::from_str_radix(s, 8).ok();
            }
        }

        // Socket ownership
        if let Some(vals) = socket.get("SOCKETUSER") {
            sock.socket.socket_user = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = socket.get("SOCKETGROUP") {
            sock.socket.socket_group = vals.first().map(|(_, v)| v.clone());
        }

        // File descriptor name
        if let Some(vals) = socket.get("FILEDESCRIPTORNAME") {
            sock.socket.fd_name = vals.first().map(|(_, v)| v.clone());
        }

        // Remove on stop
        if let Some(vals) = socket.get("REMOVEONSTOP") {
            if let Some((_, s)) = vals.first() {
                sock.socket.remove_on_stop =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }

        // Max connections per source
        if let Some(vals) = socket.get("MAXCONNECTIONSPERSOURCE") {
            if let Some((_, s)) = vals.first() {
                sock.socket.max_connections_per_source = s.parse().ok();
            }
        }

        // Buffer sizes
        if let Some(vals) = socket.get("RECEIVEBUFFER") {
            if let Some((_, s)) = vals.first() {
                sock.socket.receive_buffer = parse_memory(s);
            }
        }
        if let Some(vals) = socket.get("SENDBUFFER") {
            if let Some((_, s)) = vals.first() {
                sock.socket.send_buffer = parse_memory(s);
            }
        }

        // Pass credentials/security
        if let Some(vals) = socket.get("PASSCREDENTIALS") {
            if let Some((_, s)) = vals.first() {
                sock.socket.pass_credentials =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = socket.get("PASSSECURITY") {
            if let Some((_, s)) = vals.first() {
                sock.socket.pass_security =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }

        // Symlinks
        if let Some(vals) = socket.get("SYMLINKS") {
            for (_, v) in vals {
                sock.socket.symlinks.extend(
                    v.split_whitespace().map(|s| s.to_string())
                );
            }
        }

        // Defer trigger
        if let Some(vals) = socket.get("DEFERTRIGGER") {
            if let Some((_, s)) = vals.first() {
                sock.socket.defer_trigger =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
    }

    // [Install] section
    if let Some(install) = parsed.get("[Install]") {
        if let Some(vals) = install.get("WANTEDBY") {
            sock.install.wanted_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("REQUIREDBY") {
            sock.install.required_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("ALSO") {
            sock.install.also = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("ALIAS") {
            sock.install.alias = vals.iter().map(|(_, v)| v.clone()).collect();
        }
    }

    Ok(sock)
}

/// Parse a socket file from disk
pub async fn load_socket(path: &Path) -> Result<Socket, ParseError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut parsed = parse_unit_file(path).await?;

    // Load and merge drop-in files
    load_dropins(path, &mut parsed).await;

    parse_socket(name, &parsed)
}

/// Convert parsed INI data into a typed Timer
pub fn parse_timer(name: &str, parsed: &ParsedFile) -> Result<Timer, ParseError> {
    let mut tmr = Timer::new(name.to_string());

    // [Unit] section
    if let Some(unit) = parsed.get("[Unit]") {
        if let Some(vals) = unit.get("DESCRIPTION") {
            tmr.unit.description = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = unit.get("AFTER") {
            tmr.unit.after = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("BEFORE") {
            tmr.unit.before = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("REQUIRES") {
            tmr.unit.requires = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("WANTS") {
            tmr.unit.wants = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONFLICTS") {
            tmr.unit.conflicts = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONPATHEXISTS") {
            tmr.unit.condition_path_exists = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONDIRECTORYNOTEMPTY") {
            tmr.unit.condition_directory_not_empty =
                vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("DEFAULTDEPENDENCIES") {
            if let Some((_, s)) = vals.first() {
                tmr.unit.default_dependencies =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
    }

    // [Timer] section
    if let Some(timer) = parsed.get("[Timer]") {
        // Calendar-based triggers
        if let Some(vals) = timer.get("ONCALENDAR") {
            for (_, v) in vals {
                tmr.timer.on_calendar.push(CalendarSpec::parse(v));
            }
        }

        // Monotonic timers
        if let Some(vals) = timer.get("ONBOOTSEC") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.on_boot_sec = parse_duration(s);
            }
        }
        if let Some(vals) = timer.get("ONSTARTUPSEC") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.on_startup_sec = parse_duration(s);
            }
        }
        if let Some(vals) = timer.get("ONACTIVESEC") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.on_active_sec = parse_duration(s);
            }
        }
        if let Some(vals) = timer.get("ONUNITACTIVESEC") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.on_unit_active_sec = parse_duration(s);
            }
        }
        if let Some(vals) = timer.get("ONUNITINACTIVESEC") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.on_unit_inactive_sec = parse_duration(s);
            }
        }

        // Accuracy/delay settings
        if let Some(vals) = timer.get("ACCURACYSEC") {
            if let Some((_, s)) = vals.first() {
                if let Some(d) = parse_duration(s) {
                    tmr.timer.accuracy_sec = d;
                }
            }
        }
        if let Some(vals) = timer.get("RANDOMIZEDDELAYSEC") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.randomized_delay_sec = parse_duration(s);
            }
        }

        // Persistent
        if let Some(vals) = timer.get("PERSISTENT") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.persistent =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }

        // Wake system
        if let Some(vals) = timer.get("WAKESYSTEM") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.wake_system =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }

        // Clock/timezone change triggers
        if let Some(vals) = timer.get("ONCLOCKCHANGE") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.on_clock_change =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
        if let Some(vals) = timer.get("ONTIMEZONECHANGE") {
            if let Some((_, s)) = vals.first() {
                tmr.timer.on_timezone_change =
                    matches!(s.to_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }

        // Unit to activate
        if let Some(vals) = timer.get("UNIT") {
            tmr.timer.unit = vals.first().map(|(_, v)| v.clone());
        }
    }

    // [Install] section
    if let Some(install) = parsed.get("[Install]") {
        if let Some(vals) = install.get("WANTEDBY") {
            tmr.install.wanted_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("REQUIREDBY") {
            tmr.install.required_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("ALSO") {
            tmr.install.also = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("ALIAS") {
            tmr.install.alias = vals.iter().map(|(_, v)| v.clone()).collect();
        }
    }

    Ok(tmr)
}

/// Parse a timer file from disk
pub async fn load_timer(path: &Path) -> Result<Timer, ParseError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut parsed = parse_unit_file(path).await?;

    // Load and merge drop-in files
    load_dropins(path, &mut parsed).await;

    parse_timer(name, &parsed)
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
                    || name.ends_with(".slice")
                    || name.ends_with(".timer")
                {
                    units.push(name.to_string());
                }
            }
        }
    }

    units
}

/// Load a unit file (service, target, mount, slice, or socket) from disk
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
        Some("mount") => {
            let mount = load_mount(path).await?;
            Ok(Unit::Mount(mount))
        }
        Some("slice") => {
            let slice = load_slice(path).await?;
            Ok(Unit::Slice(slice))
        }
        Some("socket") => {
            let socket = load_socket(path).await?;
            Ok(Unit::Socket(socket))
        }
        Some("timer") => {
            let timer = load_timer(path).await?;
            Ok(Unit::Timer(timer))
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

    #[test]
    fn test_parse_resource_limits() {
        let content = r#"
[Unit]
Description=Service with resource limits

[Service]
ExecStart=/usr/bin/myservice
LimitNOFILE=65536
OOMScoreAdjust=-500
StandardInput=tty
TTYPath=/dev/tty1
TTYReset=yes
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("resource.service", &parsed).unwrap();

        assert_eq!(svc.service.limit_nofile, Some(65536));
        assert_eq!(svc.service.oom_score_adjust, Some(-500));
        assert_eq!(svc.service.standard_input, StdInput::Tty);
        assert_eq!(svc.service.tty_path, Some("/dev/tty1".into()));
        assert!(svc.service.tty_reset);
    }

    #[test]
    fn test_parse_limit_nofile_infinity() {
        let content = r#"
[Service]
ExecStart=/usr/bin/myservice
LimitNOFILE=infinity
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("unlimited.service", &parsed).unwrap();

        assert_eq!(svc.service.limit_nofile, Some(u64::MAX));
    }

    #[test]
    fn test_parse_getty_service() {
        // Based on real getty@.service
        let content = r#"
[Unit]
Description=Getty on %I
After=systemd-user-sessions.service

[Service]
Type=idle
ExecStart=/sbin/agetty -o '-p -- \\u' --noclear - $TERM
StandardInput=tty-force
TTYPath=/dev/%I
TTYReset=yes

[Install]
WantedBy=getty.target
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("getty@tty1.service", &parsed).unwrap();

        assert_eq!(svc.service.service_type, ServiceType::Idle);
        assert_eq!(svc.service.standard_input, StdInput::TtyForce);
        assert_eq!(svc.service.tty_path, Some("/dev/%I".into()));
        assert!(svc.service.tty_reset);
    }

    #[test]
    fn test_parse_security_sandboxing() {
        let content = r#"
[Unit]
Description=Hardened service

[Service]
ExecStart=/usr/bin/myservice
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=true
PrivateDevices=yes
PrivateNetwork=yes
ProtectKernelModules=yes
ProtectProc=invisible
CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_DAC_OVERRIDE
AmbientCapabilities=CAP_NET_BIND_SERVICE
RestrictNamespaces=yes
ReadWritePaths=/var/lib/myservice /run/myservice
ReadOnlyPaths=/etc/myservice
InaccessiblePaths=/home
SystemCallFilter=@system-service ~@privileged
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("hardened.service", &parsed).unwrap();

        assert!(svc.service.no_new_privileges);
        assert_eq!(svc.service.protect_system, ProtectSystem::Strict);
        assert_eq!(svc.service.protect_home, ProtectHome::ReadOnly);
        assert!(svc.service.private_tmp);
        assert!(svc.service.private_devices);
        assert!(svc.service.private_network);
        assert!(svc.service.protect_kernel_modules);
        assert_eq!(svc.service.protect_proc, ProtectProc::Invisible);

        assert_eq!(svc.service.capability_bounding_set, vec![
            "CAP_NET_BIND_SERVICE", "CAP_DAC_OVERRIDE"
        ]);
        assert_eq!(svc.service.ambient_capabilities, vec!["CAP_NET_BIND_SERVICE"]);

        // RestrictNamespaces=yes means block all (empty vec)
        assert_eq!(svc.service.restrict_namespaces, Some(Vec::new()));

        assert_eq!(svc.service.read_write_paths.len(), 2);
        assert_eq!(svc.service.read_only_paths, vec![PathBuf::from("/etc/myservice")]);
        assert_eq!(svc.service.inaccessible_paths, vec![PathBuf::from("/home")]);

        assert_eq!(svc.service.system_call_filter, vec!["@system-service", "~@privileged"]);
    }

    #[test]
    fn test_parse_mount_unit() {
        let content = r#"
[Unit]
Description=Temporary Directory /tmp
DefaultDependencies=no
Conflicts=umount.target
Before=local-fs.target umount.target
After=swap.target

[Mount]
What=tmpfs
Where=/tmp
Type=tmpfs
Options=mode=1777,strictatime,nosuid,nodev,size=50%
"#;
        let parsed = parse_file(content).unwrap();
        let mnt = parse_mount("tmp.mount", &parsed).unwrap();

        assert_eq!(mnt.unit.description, Some("Temporary Directory /tmp".into()));
        assert!(!mnt.unit.default_dependencies);
        assert!(mnt.unit.conflicts.contains(&"umount.target".into()));
        assert!(mnt.unit.before.contains(&"local-fs.target".into()));
        assert!(mnt.unit.after.contains(&"swap.target".into()));

        assert_eq!(mnt.mount.what, "tmpfs");
        assert_eq!(mnt.mount.r#where, "/tmp");
        assert_eq!(mnt.mount.fs_type, Some("tmpfs".into()));
        assert_eq!(mnt.mount.options, Some("mode=1777,strictatime,nosuid,nodev,size=50%".into()));
    }

    #[test]
    fn test_parse_mount_hugepages() {
        let content = r#"
[Unit]
Description=Huge Pages File System
DefaultDependencies=no
Before=sysinit.target
ConditionPathExists=/sys/kernel/mm/hugepages

[Mount]
What=hugetlbfs
Where=/dev/hugepages
Type=hugetlbfs
Options=nosuid,nodev
"#;
        let parsed = parse_file(content).unwrap();
        let mnt = parse_mount("dev-hugepages.mount", &parsed).unwrap();

        assert_eq!(mnt.mount.what, "hugetlbfs");
        assert_eq!(mnt.mount.r#where, "/dev/hugepages");
        assert_eq!(mnt.mount.fs_type, Some("hugetlbfs".into()));
        assert!(mnt.unit.condition_path_exists.contains(&"/sys/kernel/mm/hugepages".into()));
    }

    #[test]
    fn test_mount_where_from_name() {
        // When Where= is not specified, derive from unit name
        let content = r#"
[Mount]
What=tmpfs
Type=tmpfs
"#;
        let parsed = parse_file(content).unwrap();
        let mnt = parse_mount("var-lib-docker.mount", &parsed).unwrap();

        // Where should be derived from the unit name
        assert_eq!(mnt.mount.r#where, "/var/lib/docker");
    }

    #[test]
    fn test_mount_default_values() {
        let mnt = Mount::new("test.mount".to_string());
        assert_eq!(mnt.mount.directory_mode, Some(0o755));
        assert!(!mnt.mount.sloppy_options);
        assert!(!mnt.mount.lazy_unmount);
        assert!(!mnt.mount.force_unmount);
    }

    #[test]
    fn test_parse_slice_unit() {
        let content = r#"
[Unit]
Description=User and Session Slice
Documentation=man:systemd.special(7)
Before=slices.target
"#;
        let parsed = parse_file(content).unwrap();
        let slice = parse_slice("user.slice", &parsed).unwrap();

        assert_eq!(slice.name, "user.slice");
        assert_eq!(slice.unit.description, Some("User and Session Slice".into()));
        assert!(slice.unit.before.contains(&"slices.target".into()));
    }

    #[test]
    fn test_parse_slice_no_default_deps() {
        let content = r#"
[Unit]
Description=Encrypted Volume Units Service Slice
DefaultDependencies=no
"#;
        let parsed = parse_file(content).unwrap();
        let slice = parse_slice("system-systemd-cryptsetup.slice", &parsed).unwrap();

        assert!(!slice.unit.default_dependencies);
    }

    #[test]
    fn test_slice_cgroup_path() {
        // Top-level slices
        let slice = Slice::new("system.slice".to_string());
        assert_eq!(slice.cgroup_path(), "/sys/fs/cgroup/system.slice");

        let slice = Slice::new("user.slice".to_string());
        assert_eq!(slice.cgroup_path(), "/sys/fs/cgroup/user.slice");

        let slice = Slice::new("machine.slice".to_string());
        assert_eq!(slice.cgroup_path(), "/sys/fs/cgroup/machine.slice");

        // Nested slices
        let slice = Slice::new("user-1000.slice".to_string());
        assert_eq!(slice.cgroup_path(), "/sys/fs/cgroup/user.slice/user-1000.slice");

        let slice = Slice::new("system-systemd-cryptsetup.slice".to_string());
        assert_eq!(slice.cgroup_path(), "/sys/fs/cgroup/system.slice/system-systemd-cryptsetup.slice");
    }

    #[test]
    fn test_parse_socket_dbus() {
        let content = r#"
[Unit]
Description=D-Bus System Message Bus Socket

[Socket]
ListenStream=/run/dbus/system_bus_socket
"#;
        let parsed = parse_file(content).unwrap();
        let sock = parse_socket("dbus.socket", &parsed).unwrap();

        assert_eq!(sock.name, "dbus.socket");
        assert_eq!(sock.unit.description, Some("D-Bus System Message Bus Socket".into()));
        assert_eq!(sock.socket.listeners.len(), 1);
        assert_eq!(sock.socket.listeners[0].address, "/run/dbus/system_bus_socket");
        assert_eq!(sock.socket.listeners[0].listen_type, ListenType::Stream);
        assert!(!sock.socket.accept);
        assert_eq!(sock.service_name(), "dbus.service");
    }

    #[test]
    fn test_parse_socket_docker() {
        let content = r#"
[Unit]
Description=Docker Socket for the API

[Socket]
ListenStream=/run/docker.sock
SocketMode=0660
SocketUser=root
SocketGroup=docker

[Install]
WantedBy=sockets.target
"#;
        let parsed = parse_file(content).unwrap();
        let sock = parse_socket("docker.socket", &parsed).unwrap();

        assert_eq!(sock.socket.listeners[0].address, "/run/docker.sock");
        assert_eq!(sock.socket.socket_mode, Some(0o660));
        assert_eq!(sock.socket.socket_user, Some("root".into()));
        assert_eq!(sock.socket.socket_group, Some("docker".into()));
        assert!(sock.install.wanted_by.contains(&"sockets.target".into()));
    }

    #[test]
    fn test_parse_socket_accept_mode() {
        let content = r#"
[Unit]
Description=Git Daemon Socket

[Socket]
ListenStream=9418
Accept=true

[Install]
WantedBy=sockets.target
"#;
        let parsed = parse_file(content).unwrap();
        let sock = parse_socket("git-daemon.socket", &parsed).unwrap();

        assert_eq!(sock.socket.listeners[0].address, "9418");
        assert!(sock.socket.accept);
    }

    #[test]
    fn test_parse_socket_fifo() {
        let content = r#"
[Unit]
Description=Device-mapper event daemon FIFOs

[Socket]
ListenFIFO=/run/dmeventd-server
ListenFIFO=/run/dmeventd-client
SocketMode=0600
RemoveOnStop=true
"#;
        let parsed = parse_file(content).unwrap();
        let sock = parse_socket("dm-event.socket", &parsed).unwrap();

        assert_eq!(sock.socket.listeners.len(), 2);
        assert_eq!(sock.socket.listeners[0].listen_type, ListenType::Fifo);
        assert_eq!(sock.socket.listeners[0].address, "/run/dmeventd-server");
        assert_eq!(sock.socket.listeners[1].address, "/run/dmeventd-client");
        assert!(sock.socket.remove_on_stop);
    }

    #[test]
    fn test_parse_socket_with_service() {
        let content = r#"
[Socket]
ListenStream=/run/cups/cups.sock
Service=cupsd.service
"#;
        let parsed = parse_file(content).unwrap();
        let sock = parse_socket("cups.socket", &parsed).unwrap();

        assert_eq!(sock.socket.service, Some("cupsd.service".into()));
        assert_eq!(sock.service_name(), "cupsd.service");
    }

    #[test]
    fn test_parse_socket_datagram() {
        let content = r#"
[Socket]
ListenDatagram=/run/systemd/journal/syslog
"#;
        let parsed = parse_file(content).unwrap();
        let sock = parse_socket("syslog.socket", &parsed).unwrap();

        assert_eq!(sock.socket.listeners[0].listen_type, ListenType::Datagram);
        assert_eq!(sock.socket.listeners[0].address, "/run/systemd/journal/syslog");
    }

    #[test]
    fn test_parse_timer_tmpfiles_clean() {
        let content = r#"
[Unit]
Description=Daily Cleanup of Temporary Directories
ConditionPathExists=!/etc/initrd-release

[Timer]
OnBootSec=15min
OnUnitActiveSec=1d
"#;
        let parsed = parse_file(content).unwrap();
        let tmr = parse_timer("systemd-tmpfiles-clean.timer", &parsed).unwrap();

        assert_eq!(tmr.name, "systemd-tmpfiles-clean.timer");
        assert_eq!(tmr.unit.description, Some("Daily Cleanup of Temporary Directories".into()));
        assert_eq!(tmr.timer.on_boot_sec, Some(std::time::Duration::from_secs(15 * 60)));
        assert_eq!(tmr.timer.on_unit_active_sec, Some(std::time::Duration::from_secs(86400)));
        assert_eq!(tmr.service_name(), "systemd-tmpfiles-clean.service");
    }

    #[test]
    fn test_parse_timer_weekly() {
        let content = r#"
[Unit]
Description=Discard unused filesystem blocks once a week

[Timer]
OnCalendar=weekly
AccuracySec=1h
Persistent=true
RandomizedDelaySec=100min

[Install]
WantedBy=timers.target
"#;
        let parsed = parse_file(content).unwrap();
        let tmr = parse_timer("fstrim.timer", &parsed).unwrap();

        assert_eq!(tmr.timer.on_calendar.len(), 1);
        assert!(tmr.timer.on_calendar[0].is_weekly());
        assert_eq!(tmr.timer.accuracy_sec, std::time::Duration::from_secs(3600));
        assert!(tmr.timer.persistent);
        assert_eq!(tmr.timer.randomized_delay_sec, Some(std::time::Duration::from_secs(6000)));
        assert!(tmr.install.wanted_by.contains(&"timers.target".into()));
    }

    #[test]
    fn test_parse_timer_daily() {
        let content = r#"
[Unit]
Description=Daily verification of password and group files

[Timer]
OnCalendar=daily
AccuracySec=12h
Persistent=true
"#;
        let parsed = parse_file(content).unwrap();
        let tmr = parse_timer("shadow.timer", &parsed).unwrap();

        assert!(tmr.timer.on_calendar[0].is_daily());
        assert_eq!(tmr.timer.accuracy_sec, std::time::Duration::from_secs(12 * 3600));
        assert!(tmr.timer.persistent);
    }

    #[test]
    fn test_parse_timer_hourly() {
        let content = r#"
[Timer]
OnCalendar=*-*-* *:00:00
RandomizedDelaySec=1h
Persistent=true
"#;
        let parsed = parse_file(content).unwrap();
        let tmr = parse_timer("fwupd-refresh.timer", &parsed).unwrap();

        assert_eq!(tmr.timer.on_calendar.len(), 1);
        match &tmr.timer.on_calendar[0] {
            CalendarSpec::Full(s) => assert_eq!(s, "*-*-* *:00:00"),
            _ => panic!("Expected Full calendar spec"),
        }
        assert!(tmr.timer.persistent);
    }

    #[test]
    fn test_parse_timer_with_unit() {
        let content = r#"
[Timer]
OnCalendar=weekly
Unit=my-special.service
"#;
        let parsed = parse_file(content).unwrap();
        let tmr = parse_timer("my-timer.timer", &parsed).unwrap();

        assert_eq!(tmr.timer.unit, Some("my-special.service".into()));
        assert_eq!(tmr.service_name(), "my-special.service");
    }

    #[test]
    fn test_calendar_spec_parse() {
        // Named shortcuts
        assert!(matches!(CalendarSpec::parse("daily"), CalendarSpec::Named(s) if s == "daily"));
        assert!(matches!(CalendarSpec::parse("weekly"), CalendarSpec::Named(s) if s == "weekly"));
        assert!(matches!(CalendarSpec::parse("monthly"), CalendarSpec::Named(s) if s == "monthly"));

        // Day of week
        assert!(matches!(CalendarSpec::parse("Sat"), CalendarSpec::DayOfWeek(_)));
        assert!(matches!(CalendarSpec::parse("Sun"), CalendarSpec::DayOfWeek(_)));

        // Time only
        match CalendarSpec::parse("4:10") {
            CalendarSpec::Time { hour, minute, second } => {
                assert_eq!(hour, 4);
                assert_eq!(minute, 10);
                assert_eq!(second, 0);
            }
            _ => panic!("Expected Time spec"),
        }

        // Full expression
        assert!(matches!(CalendarSpec::parse("*-*-* *:00:00"), CalendarSpec::Full(_)));
    }

    #[test]
    fn test_timer_is_monotonic() {
        let content = r#"
[Timer]
OnBootSec=15min
"#;
        let parsed = parse_file(content).unwrap();
        let tmr = parse_timer("boot.timer", &parsed).unwrap();
        assert!(tmr.is_monotonic());
        assert!(!tmr.is_realtime());

        let content = r#"
[Timer]
OnCalendar=daily
"#;
        let parsed = parse_file(content).unwrap();
        let tmr = parse_timer("daily.timer", &parsed).unwrap();
        assert!(!tmr.is_monotonic());
        assert!(tmr.is_realtime());
    }
}
