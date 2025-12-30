//! Process spawning and management

use std::collections::HashMap;
use std::os::unix::io::AsRawFd;
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

    // Set NOTIFY_SOCKET for Type=notify services
    if let Some(socket_path) = &options.notify_socket {
        cmd.env("NOTIFY_SOCKET", socket_path);
    }

    // Set WATCHDOG_USEC for services with WatchdogSec
    if let Some(usec) = options.watchdog_usec {
        cmd.env("WATCHDOG_USEC", usec.to_string());
    }

    // Collect pre_exec settings
    let uid = service.service.user.as_ref().and_then(|u| resolve_user(u));
    let limit_nofile = service.service.limit_nofile;
    let oom_score_adjust = service.service.oom_score_adjust;
    let tty_path = service.service.tty_path.clone();
    let tty_reset = service.service.tty_reset;
    let std_input = service.service.standard_input.clone();
    let service_section = service.service.clone(); // For sandbox

    // Apply process settings in pre_exec (runs after fork, before exec)
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            // Set resource limits (LimitNOFILE=)
            if let Some(nofile) = limit_nofile {
                let rlim = libc::rlimit {
                    rlim_cur: nofile,
                    rlim_max: nofile,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) != 0 {
                    log::warn!("Failed to set RLIMIT_NOFILE to {}", nofile);
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

            // Set user ID (if specified) - must be after sandbox setup
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

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("Service {0} has no ExecStart")]
    NoExecStart(String),

    #[error("Invalid command: {0}")]
    InvalidCommand(String),

    #[error("Failed to spawn process: {0}")]
    Spawn(String),
}
