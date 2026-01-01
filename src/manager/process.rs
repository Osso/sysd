//! Process spawning and management

use std::collections::HashMap;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;
use std::process::Stdio;
use tokio::process::{Child, Command};

use crate::units::{Service, StdInput};

/// Options for spawning a service
#[derive(Default)]
pub struct SpawnOptions {
    /// Path to NOTIFY_SOCKET for Type=notify services
    pub notify_socket: Option<String>,
    /// Watchdog timeout in microseconds (for WatchdogSec services)
    pub watchdog_usec: Option<u64>,
    /// Socket file descriptors for socket activation (LISTEN_FDS)
    pub socket_fds: Vec<RawFd>,
    /// M19: Override UID for DynamicUser= (allocated by DynamicUserManager)
    pub dynamic_uid: Option<u32>,
    /// M19: Override GID for DynamicUser= (allocated by DynamicUserManager)
    pub dynamic_gid: Option<u32>,
    /// M19: Stored FDs from previous run (FileDescriptorStoreMax=)
    /// These are passed via LISTEN_FDS along with socket_fds
    pub stored_fds: Vec<RawFd>,
}

/// Spawn a process for a service (convenience wrapper)
#[allow(dead_code)]
pub fn spawn_service(service: &Service) -> Result<Child, SpawnError> {
    spawn_service_with_options(service, &SpawnOptions::default())
}

/// Spawn a process for a service with options
pub fn spawn_service_with_options(
    service: &Service,
    options: &SpawnOptions,
) -> Result<Child, SpawnError> {
    let exec_start = service
        .service
        .exec_start
        .first()
        .ok_or_else(|| SpawnError::NoExecStart(service.name.clone()))?;

    // Substitute specifiers (%i, %n, etc.) for template instances
    let exec_start = substitute_specifiers(exec_start, service);

    let (program, args) = parse_command(&exec_start)?;

    let mut cmd = Command::new(&program);
    cmd.args(&args);

    // Set working directory
    if let Some(wd) = &service.service.working_directory {
        cmd.current_dir(wd);
    }

    // Set environment
    cmd.env_clear();
    cmd.envs(std::env::vars()); // Inherit current env
    for (key, value) in &service.service.environment {
        cmd.env(key, value);
    }

    // Load environment files
    for env_file in &service.service.environment_file {
        if let Ok(vars) = load_env_file(env_file) {
            cmd.envs(vars);
        }
    }

    // Unset environment variables (UnsetEnvironment=)
    for var in &service.service.unset_environment {
        cmd.env_remove(var);
    }

    // Set NOTIFY_SOCKET for Type=notify services
    if let Some(socket_path) = &options.notify_socket {
        cmd.env("NOTIFY_SOCKET", socket_path);
    }

    // Set WATCHDOG_USEC for services with WatchdogSec
    if let Some(usec) = options.watchdog_usec {
        cmd.env("WATCHDOG_USEC", usec.to_string());
    }

    // Set LISTEN_FDS for socket activation and stored FDs
    // M19: Combine socket_fds and stored_fds for LISTEN_FDS
    let mut all_fds = options.socket_fds.clone();
    all_fds.extend(&options.stored_fds);
    let socket_fds = all_fds;

    if !socket_fds.is_empty() {
        cmd.env("LISTEN_FDS", socket_fds.len().to_string());
        // LISTEN_PID is set in pre_exec after fork
        // M19: Set LISTEN_FDNAMES for named FDs (not implemented yet - would need fd names)
    }

    // Collect pre_exec settings
    // M19: DynamicUser= overrides User=/Group= settings
    let uid = options
        .dynamic_uid
        .or_else(|| service.service.user.as_ref().and_then(|u| resolve_user(u)));
    let gid = options
        .dynamic_gid
        .or_else(|| service.service.group.as_ref().and_then(|g| resolve_group(g)));
    let limit_nofile = service.service.limit_nofile;
    let limit_nproc = service.service.limit_nproc;
    let limit_core = service.service.limit_core;
    let oom_score_adjust = service.service.oom_score_adjust;
    let tty_path = service.service.tty_path.clone();
    let tty_reset = service.service.tty_reset;
    let std_input = service.service.standard_input.clone();
    let service_section = service.service.clone(); // For sandbox

    // M17: Create runtime directories before fork (as root)
    create_service_directories(&service.service, &service.name, uid, gid)?;

    // Apply process settings in pre_exec (runs after fork, before exec)
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            // Socket activation: duplicate FDs to positions 3, 4, 5, ...
            // and set LISTEN_PID to our PID
            if !socket_fds.is_empty() {
                // Set LISTEN_PID (can only be done after fork)
                std::env::set_var("LISTEN_PID", std::process::id().to_string());

                // SD_LISTEN_FDS_START is 3 (after stdin/stdout/stderr)
                const SD_LISTEN_FDS_START: RawFd = 3;

                for (i, &fd) in socket_fds.iter().enumerate() {
                    let target_fd = SD_LISTEN_FDS_START + i as RawFd;

                    if fd != target_fd {
                        // Duplicate to correct position
                        if libc::dup2(fd, target_fd) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        // Close original if it's not in the target range
                        if fd >= SD_LISTEN_FDS_START + socket_fds.len() as RawFd || fd < SD_LISTEN_FDS_START {
                            libc::close(fd);
                        }
                    }

                    // Clear FD_CLOEXEC so the FD survives exec
                    let flags = libc::fcntl(target_fd, libc::F_GETFD);
                    if flags >= 0 {
                        libc::fcntl(target_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    }
                }
            }

            // Set resource limits (LimitNOFILE=, LimitNPROC=, LimitCORE=)
            if let Some(nofile) = limit_nofile {
                let rlim = libc::rlimit {
                    rlim_cur: nofile,
                    rlim_max: nofile,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) != 0 {
                    log::warn!("Failed to set RLIMIT_NOFILE to {}", nofile);
                }
            }
            if let Some(nproc) = limit_nproc {
                let rlim = libc::rlimit {
                    rlim_cur: nproc,
                    rlim_max: nproc,
                };
                if libc::setrlimit(libc::RLIMIT_NPROC, &rlim) != 0 {
                    log::warn!("Failed to set RLIMIT_NPROC to {}", nproc);
                }
            }
            if let Some(core) = limit_core {
                let rlim = libc::rlimit {
                    rlim_cur: core,
                    rlim_max: core,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &rlim) != 0 {
                    log::warn!("Failed to set RLIMIT_CORE to {}", core);
                }
            }

            // Set OOM score adjustment
            if let Some(score) = oom_score_adjust {
                let path = "/proc/self/oom_score_adj";
                if std::fs::write(path, score.to_string()).is_err() {
                    log::warn!("Failed to set oom_score_adj to {}", score);
                }
            }

            // Apply security sandbox (must be done before dropping privileges)
            if let Err(e) = super::sandbox::apply_sandbox(&service_section) {
                log::warn!("Sandbox setup failed: {}", e);
                // Continue anyway - sandbox failures shouldn't prevent service start
            }

            // Set group and user IDs (if specified) - must be after sandbox setup
            // Group must be set before user (can't change groups after dropping root)
            if let Some(gid) = gid {
                nix::unistd::setgid(nix::unistd::Gid::from_raw(gid))?;
            }
            if let Some(uid) = uid {
                nix::unistd::setuid(nix::unistd::Uid::from_raw(uid))?;
            }

            // TTY setup (for getty-like services)
            if matches!(
                std_input,
                StdInput::Tty | StdInput::TtyForce | StdInput::TtyFail
            ) {
                if let Some(ref path) = tty_path {
                    // Reset TTY if requested
                    if tty_reset {
                        // Best effort TTY reset - open and close with O_CLOEXEC
                        // Full reset would require termios manipulation
                        let _ = std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(path);
                    }

                    // Open the TTY and set up as controlling terminal
                    let tty_file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path);

                    match tty_file {
                        Ok(f) => {
                            let fd = f.as_raw_fd();
                            // Make this our controlling terminal
                            if libc::ioctl(fd, libc::TIOCSCTTY, 0) < 0 {
                                if matches!(std_input, StdInput::TtyFail) {
                                    return Err(std::io::Error::last_os_error());
                                }
                            }
                            // Dup to stdin
                            libc::dup2(fd, 0);
                            // Also to stdout/stderr for getty
                            libc::dup2(fd, 1);
                            libc::dup2(fd, 2);
                            // Close original if not 0,1,2
                            if fd > 2 {
                                libc::close(fd);
                            }
                            // Don't drop f - we've already taken the fd
                            std::mem::forget(f);
                        }
                        Err(e) => {
                            if matches!(std_input, StdInput::TtyFail) {
                                return Err(e);
                            }
                            // TtyForce would try to steal it, but we don't implement that
                            log::warn!("Failed to open TTY {:?}: {}", path, e);
                        }
                    }
                }
            }

            Ok(())
        });
    }

    // Redirect stdout/stderr based on config
    // For now, inherit (we'll add journal support later)
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    // Set stdin based on StandardInput=
    let stdin = match service.service.standard_input {
        StdInput::Null => Stdio::null(),
        // TTY is handled in pre_exec above
        StdInput::Tty | StdInput::TtyForce | StdInput::TtyFail => Stdio::inherit(),
    };
    cmd.stdin(stdin);

    // Spawn the process
    let child = cmd.spawn().map_err(|e| SpawnError::Spawn(e.to_string()))?;

    Ok(child)
}

/// Parse a command line into program and arguments
fn parse_command(cmd: &str) -> Result<(String, Vec<String>), SpawnError> {
    // Handle special prefixes (-, @, +, !, !!)
    let cmd = cmd.trim_start_matches(|c| c == '-' || c == '@' || c == '+' || c == '!');

    let parts = shlex::split(cmd).ok_or_else(|| SpawnError::InvalidCommand(cmd.to_string()))?;

    if parts.is_empty() {
        return Err(SpawnError::InvalidCommand(cmd.to_string()));
    }

    let program = parts[0].clone();
    let args = parts[1..].to_vec();

    Ok((program, args))
}

/// Substitute systemd specifiers in a string
/// See: https://www.freedesktop.org/software/systemd/man/systemd.unit.html#Specifiers
pub fn substitute_specifiers(s: &str, service: &Service) -> String {
    let mut result = s.to_string();

    // %n - Full unit name
    result = result.replace("%n", &service.name);

    // %N - Full unit name without suffix (e.g., "foo@bar" from "foo@bar.service")
    let name_without_suffix = service
        .name
        .strip_suffix(".service")
        .or_else(|| service.name.strip_suffix(".target"))
        .unwrap_or(&service.name);
    result = result.replace("%N", name_without_suffix);

    // %p - Prefix name (before @ or full name if no @)
    let prefix = service
        .name
        .find('@')
        .map(|pos| &service.name[..pos])
        .unwrap_or(name_without_suffix);
    result = result.replace("%p", prefix);

    // %P - Escaped prefix (same as %p for now - proper escaping is complex)
    result = result.replace("%P", prefix);

    // %i - Instance name (unescaped)
    if let Some(ref instance) = service.instance {
        result = result.replace("%i", instance);
        // %I - Instance name (escaped for shell - basic escaping)
        result = result.replace("%I", instance);
    } else {
        // No instance - remove specifiers
        result = result.replace("%i", "");
        result = result.replace("%I", "");
    }

    // %% - Literal %
    result = result.replace("%%", "%");

    result
}

/// Load environment variables from a file
fn load_env_file(path: &Path) -> Result<HashMap<String, String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let mut vars = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            // Remove quotes from value
            let value = value.trim_matches('"').trim_matches('\'');
            vars.insert(key.to_string(), value.to_string());
        }
    }

    Ok(vars)
}

/// Resolve username to UID
#[cfg(unix)]
fn resolve_user(user: &str) -> Option<u32> {
    // Try numeric UID first
    if let Ok(uid) = user.parse::<u32>() {
        return Some(uid);
    }

    // Look up by name
    use std::ffi::CString;
    let name = CString::new(user).ok()?;
    unsafe {
        let pwd = libc::getpwnam(name.as_ptr());
        if pwd.is_null() {
            None
        } else {
            Some((*pwd).pw_uid)
        }
    }
}

/// Resolve group name to GID
#[cfg(unix)]
fn resolve_group(group: &str) -> Option<u32> {
    // Try numeric GID first
    if let Ok(gid) = group.parse::<u32>() {
        return Some(gid);
    }

    // Look up by name
    use std::ffi::CString;
    let name = CString::new(group).ok()?;
    unsafe {
        let grp = libc::getgrnam(name.as_ptr());
        if grp.is_null() {
            None
        } else {
            Some((*grp).gr_gid)
        }
    }
}

/// M17: Create service directories (State, Runtime, Config, Logs, Cache)
fn create_service_directories(
    service: &crate::units::ServiceSection,
    service_name: &str,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<(), SpawnError> {
    use std::os::unix::fs::{chown, PermissionsExt};
    use std::path::Path;

    // Extract base name from service name (remove .service suffix)
    let base_name = service_name.strip_suffix(".service").unwrap_or(service_name);

    // Helper to create a directory with correct ownership
    let create_dir = |base: &str, name: &str| -> std::io::Result<()> {
        let path = Path::new(base).join(name);
        std::fs::create_dir_all(&path)?;
        // Set ownership if user/group specified
        if uid.is_some() || gid.is_some() {
            chown(&path, uid, gid)?;
        }
        // Set permissions: 0755 for directories
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        log::debug!("Created directory: {}", path.display());
        Ok(())
    };

    // StateDirectory= -> /var/lib/<name>
    for name in &service.state_directory {
        let dir_name = if name.is_empty() { base_name } else { name };
        if let Err(e) = create_dir("/var/lib", dir_name) {
            log::warn!("Failed to create state directory {}: {}", dir_name, e);
        }
    }

    // RuntimeDirectory= -> /run/<name>
    for name in &service.runtime_directory {
        let dir_name = if name.is_empty() { base_name } else { name };
        if let Err(e) = create_dir("/run", dir_name) {
            log::warn!("Failed to create runtime directory {}: {}", dir_name, e);
        }
    }

    // ConfigurationDirectory= -> /etc/<name>
    for name in &service.configuration_directory {
        let dir_name = if name.is_empty() { base_name } else { name };
        if let Err(e) = create_dir("/etc", dir_name) {
            log::warn!("Failed to create configuration directory {}: {}", dir_name, e);
        }
    }

    // LogsDirectory= -> /var/log/<name>
    for name in &service.logs_directory {
        let dir_name = if name.is_empty() { base_name } else { name };
        if let Err(e) = create_dir("/var/log", dir_name) {
            log::warn!("Failed to create logs directory {}: {}", dir_name, e);
        }
    }

    // CacheDirectory= -> /var/cache/<name>
    for name in &service.cache_directory {
        let dir_name = if name.is_empty() { base_name } else { name };
        if let Err(e) = create_dir("/var/cache", dir_name) {
            log::warn!("Failed to create cache directory {}: {}", dir_name, e);
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("Service {0} has no ExecStart")]
    NoExecStart(String),

    #[error("Invalid command: {0}")]
    InvalidCommand(String),

    #[error("Failed to spawn process: {0}")]
    Spawn(String),
}
