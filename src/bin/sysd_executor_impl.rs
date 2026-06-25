// sysd-executor - Process spawning helper for sysd
//
// This binary implements the executor pattern to avoid copy-on-write
// memory issues when forking from PID 1.
//
// Usage: sysd-executor --deserialize=FD
//
// The FD contains a serialized ExecConfig that specifies:
// - Program and arguments to execute
// - Environment variables
// - User/group credentials
// - Resource limits
// - Security sandbox settings
// - Socket activation FDs

use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::io::RawFd;

// Import executor module from sysd lib
use sysd::executor::{ExecConfig, StdInputConfig};

fn main() {
    // Parse arguments
    let args: Vec<String> = std::env::args().collect();

    let fd = parse_deserialize_fd(&args).unwrap_or_else(|| {
        eprintln!("Usage: sysd-executor --deserialize=FD");
        eprintln!("  FD: file descriptor containing serialized ExecConfig");
        std::process::exit(1);
    });

    // Deserialize config
    let config = match sysd::executor::deserialize_from_fd(fd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sysd-executor: failed to deserialize config: {}", e);
            std::process::exit(1);
        }
    };

    // Apply config and exec
    if let Err(e) = apply_and_exec(config) {
        eprintln!("sysd-executor: {}", e);
        std::process::exit(1);
    }
}

fn parse_deserialize_fd(args: &[String]) -> Option<RawFd> {
    for arg in args.iter().skip(1) {
        if let Some(fd_str) = arg.strip_prefix("--deserialize=") {
            return fd_str.parse().ok();
        }
    }
    None
}

fn apply_and_exec(config: ExecConfig) -> Result<(), String> {
    // 1. Set up socket activation FDs (must be done early, before other setup)
    setup_socket_fds(config.socket_fd_count, &config.socket_fd_names)?;

    // 2. Set environment variables
    setup_environment(&config.environment, &config.unset_environment)?;

    // 3. Set resource limits
    setup_rlimits(&config)?;

    // 4. Set OOM score adjust
    if let Some(score) = config.oom_score_adjust {
        set_oom_score_adjust(score)?;
    }

    // 5. Apply security sandbox PHASE 1: mount namespace, protections (before privileges)
    // This does NOT include: NoNewPrivileges, ambient caps, seccomp (those come later)
    apply_sandbox_phase1(&config.sandbox)?;

    // 6. Set credentials (uid/gid)
    // Use SECBIT_KEEP_CAPS to preserve capabilities across setuid()
    let needs_caps = !config.sandbox.ambient_capabilities.is_empty();
    set_credentials(config.gid, config.uid, needs_caps)?;

    // 7. Apply security sandbox PHASE 2: capabilities, NoNewPrivileges, seccomp
    // Must be AFTER setuid() so ambient caps work correctly
    apply_sandbox_phase2(&config.sandbox)?;

    // 8. Set working directory
    if let Some(ref wd) = config.working_directory {
        std::env::set_current_dir(wd)
            .map_err(|e| format!("Failed to set working directory: {}", e))?;
    }

    // 9. Set up TTY if needed
    setup_tty(&config)?;

    // 10. Exec the target program
    exec_program(&config.program, &config.args)
}

fn setup_socket_fds(count: usize, names: &[String]) -> Result<(), String> {
    if count == 0 {
        return Ok(());
    }

    // Debug: log socket activation setup
    eprintln!(
        "sysd-executor: socket activation: count={}, names={:?}",
        count, names
    );

    set_socket_activation_environment(count, names);
    clear_socket_fd_cloexec(count);
    Ok(())
}

fn set_socket_activation_environment(count: usize, names: &[String]) {
    let pid = std::process::id();
    set_env_cstr("LISTEN_FDS", &count.to_string());
    set_env_cstr("LISTEN_PID", &pid.to_string());
    set_env_cstr("LISTEN_FDNAMES", &socket_fd_names(count, names));
}

fn set_env_cstr(key: &str, value: &str) {
    let key = CString::new(key).unwrap();
    let value = CString::new(value).unwrap();
    unsafe {
        libc::setenv(key.as_ptr(), value.as_ptr(), 1);
    }
}

fn socket_fd_names(count: usize, names: &[String]) -> String {
    (0..count)
        .map(|i| {
            names
                .get(i)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string())
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn clear_socket_fd_cloexec(count: usize) {
    const SD_LISTEN_FDS_START: RawFd = 3;
    for i in 0..count {
        let fd = SD_LISTEN_FDS_START + i as RawFd;
        clear_fd_cloexec(fd);
    }
}

fn clear_fd_cloexec(fd: RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            eprintln!(
                "sysd-executor: socket fd {} is INVALID: {}",
                fd,
                std::io::Error::last_os_error()
            );
            return;
        }
        libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        eprintln!("sysd-executor: socket fd {} is valid (flags={})", fd, flags);
    }
}

fn setup_environment(env: &HashMap<String, String>, unset: &[String]) -> Result<(), String> {
    // Debug: log NOTIFY_SOCKET if present
    if let Some(notify_socket) = env.get("NOTIFY_SOCKET") {
        eprintln!("sysd-executor: NOTIFY_SOCKET={}", notify_socket);
        // Check if socket exists
        let path = std::path::Path::new(notify_socket);
        if path.exists() {
            eprintln!("sysd-executor: NOTIFY_SOCKET exists");
        } else {
            eprintln!("sysd-executor: NOTIFY_SOCKET does NOT exist");
        }
    }

    // Set environment variables
    for (key, value) in env {
        std::env::set_var(key, value);
    }

    // Unset environment variables
    for var in unset {
        std::env::remove_var(var);
    }

    Ok(())
}

fn setup_rlimits(config: &ExecConfig) -> Result<(), String> {
    if let Some(nofile) = config.limit_nofile {
        set_rlimit(libc::RLIMIT_NOFILE, nofile)?;
    }
    if let Some(nproc) = config.limit_nproc {
        set_rlimit(libc::RLIMIT_NPROC, nproc)?;
    }
    if let Some(core) = config.limit_core {
        set_rlimit(libc::RLIMIT_CORE, core)?;
    }
    Ok(())
}

fn set_rlimit(resource: libc::c_int, limit: u64) -> Result<(), String> {
    let rlim = libc::rlimit {
        rlim_cur: limit,
        rlim_max: limit,
    };
    unsafe {
        if libc::setrlimit(resource, &rlim) != 0 {
            return Err(format!(
                "Failed to set rlimit {:?}: {}",
                resource,
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn set_oom_score_adjust(score: i32) -> Result<(), String> {
    std::fs::write("/proc/self/oom_score_adj", score.to_string())
        .map_err(|e| format!("Failed to set oom_score_adj: {}", e))
}

// Securebits constants for preserving capabilities across setuid
const SECBIT_KEEP_CAPS: libc::c_ulong = 1 << 4;
const SECBIT_NO_SETUID_FIXUP: libc::c_ulong = 1 << 2;

fn set_credentials(gid: Option<u32>, uid: Option<u32>, needs_caps: bool) -> Result<(), String> {
    // If we need to preserve capabilities across setuid(), set SECBIT_KEEP_CAPS
    // This prevents the kernel from clearing the permitted capability set on setuid()
    if needs_caps && uid.is_some() {
        unsafe {
            // Set KEEP_CAPS and NO_SETUID_FIXUP to preserve capabilities
            let securebits = SECBIT_KEEP_CAPS | SECBIT_NO_SETUID_FIXUP;
            if libc::prctl(libc::PR_SET_SECUREBITS, securebits, 0, 0, 0) != 0 {
                eprintln!(
                    "sysd-executor: warning: failed to set securebits: {}",
                    std::io::Error::last_os_error()
                );
                // Continue anyway - caps might not work but we shouldn't fail the service
            }
        }
    }

    // Group must be set before user
    if let Some(gid) = gid {
        unsafe {
            if libc::setgid(gid) != 0 {
                return Err(format!(
                    "Failed to setgid({}): {}",
                    gid,
                    std::io::Error::last_os_error()
                ));
            }
            // Also set supplementary groups to empty (like systemd does)
            if libc::setgroups(0, std::ptr::null()) != 0 {
                // Non-fatal - might not have CAP_SETGID
            }
        }
    }

    if let Some(uid) = uid {
        unsafe {
            if libc::setuid(uid) != 0 {
                return Err(format!(
                    "Failed to setuid({}): {}",
                    uid,
                    std::io::Error::last_os_error()
                ));
            }
        }
    }

    Ok(())
}

fn setup_tty(config: &ExecConfig) -> Result<(), String> {
    if !matches!(
        config.std_input,
        StdInputConfig::Tty | StdInputConfig::TtyForce | StdInputConfig::TtyFail
    ) {
        return Ok(());
    }

    let path = match &config.tty_path {
        Some(p) => p,
        None => return Ok(()),
    };

    if config.tty_reset {
        let _ = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path);
    }

    let tty_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path);
    let tty_file = match tty_file {
        Ok(file) => file,
        Err(error) if matches!(config.std_input, StdInputConfig::TtyFail) => {
            return Err(format!("Failed to open TTY {:?}: {}", path, error));
        }
        Err(_) => return Ok(()),
    };
    configure_tty_fds(
        tty_file,
        matches!(config.std_input, StdInputConfig::TtyFail),
    )
}

fn configure_tty_fds(tty_file: std::fs::File, fail_if_no_ctty: bool) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;

    let fd = tty_file.as_raw_fd();
    set_controlling_terminal(fd, fail_if_no_ctty)?;
    dup_to_standard_streams(fd);
    std::mem::forget(tty_file);
    Ok(())
}

fn set_controlling_terminal(fd: RawFd, fail_if_no_ctty: bool) -> Result<(), String> {
    unsafe {
        if libc::ioctl(fd, libc::TIOCSCTTY, 0) >= 0 {
            return Ok(());
        }
    }
    if !fail_if_no_ctty {
        return Ok(());
    }
    Err(format!(
        "Failed to set controlling terminal: {}",
        std::io::Error::last_os_error()
    ))
}

fn dup_to_standard_streams(fd: RawFd) {
    unsafe {
        libc::dup2(fd, 0);
        libc::dup2(fd, 1);
        libc::dup2(fd, 2);
        if fd > 2 {
            libc::close(fd);
        }
    }
}

fn exec_program(program: &str, args: &[String]) -> Result<(), String> {
    let program_c = CString::new(program).map_err(|_| "Invalid program path (contains null)")?;

    let mut argv: Vec<CString> = Vec::with_capacity(args.len() + 1);
    argv.push(program_c.clone());
    for arg in args {
        argv.push(CString::new(arg.as_str()).map_err(|_| "Invalid argument (contains null)")?);
    }

    let argv_ptrs: Vec<*const libc::c_char> = argv
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    unsafe {
        // Use execvp to search PATH for commands without full paths
        // (e.g., "journalctl" instead of "/usr/bin/journalctl")
        libc::execvp(program_c.as_ptr(), argv_ptrs.as_ptr());
    }

    // If we get here, exec failed
    Err(format!("execv failed: {}", std::io::Error::last_os_error()))
}

#[path = "sysd_executor/sandbox.rs"]
mod sysd_executor_sandbox;
use self::sysd_executor_sandbox::{apply_sandbox_phase1, apply_sandbox_phase2};
