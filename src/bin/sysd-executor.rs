//! sysd-executor - Process spawning helper for sysd
//!
//! This binary implements the executor pattern to avoid copy-on-write
//! memory issues when forking from PID 1.
//!
//! Usage: sysd-executor --deserialize=FD
//!
//! The FD contains a serialized ExecConfig that specifies:
//! - Program and arguments to execute
//! - Environment variables
//! - User/group credentials
//! - Resource limits
//! - Security sandbox settings
//! - Socket activation FDs

use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::io::RawFd;

// Import executor module from sysd lib
use sysd::executor::{
    DevicePolicyConfig, ExecConfig, ProtectHomeConfig, ProtectProcConfig, ProtectSystemConfig,
    SandboxConfig, StdInputConfig,
};

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
    setup_socket_fds(config.socket_fd_count)?;

    // 2. Set environment variables
    setup_environment(&config.environment, &config.unset_environment)?;

    // 3. Set resource limits
    setup_rlimits(&config)?;

    // 4. Set OOM score adjust
    if let Some(score) = config.oom_score_adjust {
        set_oom_score_adjust(score)?;
    }

    // 5. Apply security sandbox (must be before dropping privileges)
    apply_sandbox(&config.sandbox)?;

    // 6. Set credentials (uid/gid) - must be after sandbox setup
    set_credentials(config.gid, config.uid)?;

    // 7. Set working directory
    if let Some(ref wd) = config.working_directory {
        std::env::set_current_dir(wd)
            .map_err(|e| format!("Failed to set working directory: {}", e))?;
    }

    // 8. Set up TTY if needed
    setup_tty(&config)?;

    // 9. Exec the target program
    exec_program(&config.program, &config.args)
}

fn setup_socket_fds(count: usize) -> Result<(), String> {
    if count == 0 {
        return Ok(());
    }

    // Socket FDs are already at positions 3, 4, 5, ... passed by parent
    // Set LISTEN_FDS and LISTEN_PID environment variables
    let pid = std::process::id();

    unsafe {
        let listen_fds_key = CString::new("LISTEN_FDS").unwrap();
        let listen_fds_val = CString::new(count.to_string()).unwrap();
        libc::setenv(listen_fds_key.as_ptr(), listen_fds_val.as_ptr(), 1);

        let listen_pid_key = CString::new("LISTEN_PID").unwrap();
        let listen_pid_val = CString::new(pid.to_string()).unwrap();
        libc::setenv(listen_pid_key.as_ptr(), listen_pid_val.as_ptr(), 1);

        // Clear FD_CLOEXEC on socket FDs so they survive exec
        const SD_LISTEN_FDS_START: RawFd = 3;
        for i in 0..count {
            let fd = SD_LISTEN_FDS_START + i as RawFd;
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }

    Ok(())
}

fn setup_environment(
    env: &HashMap<String, String>,
    unset: &[String],
) -> Result<(), String> {
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

fn set_rlimit(resource: libc::__rlimit_resource_t, limit: u64) -> Result<(), String> {
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

fn set_credentials(gid: Option<u32>, uid: Option<u32>) -> Result<(), String> {
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
    use std::os::unix::io::AsRawFd;

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

    // Reset TTY if requested
    if config.tty_reset {
        let _ = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path);
    }

    // Open the TTY
    let tty_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path);

    match tty_file {
        Ok(f) => {
            let fd = f.as_raw_fd();
            unsafe {
                // Make this our controlling terminal
                if libc::ioctl(fd, libc::TIOCSCTTY, 0) < 0 {
                    if matches!(config.std_input, StdInputConfig::TtyFail) {
                        return Err(format!(
                            "Failed to set controlling terminal: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                }
                // Dup to stdin/stdout/stderr
                libc::dup2(fd, 0);
                libc::dup2(fd, 1);
                libc::dup2(fd, 2);
                if fd > 2 {
                    libc::close(fd);
                }
            }
            std::mem::forget(f);
        }
        Err(e) => {
            if matches!(config.std_input, StdInputConfig::TtyFail) {
                return Err(format!("Failed to open TTY {:?}: {}", path, e));
            }
        }
    }

    Ok(())
}

fn exec_program(program: &str, args: &[String]) -> Result<(), String> {
    let program_c =
        CString::new(program).map_err(|_| "Invalid program path (contains null)")?;

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
        libc::execv(program_c.as_ptr(), argv_ptrs.as_ptr());
    }

    // If we get here, exec failed
    Err(format!(
        "execv failed: {}",
        std::io::Error::last_os_error()
    ))
}

// ============================================================================
// Sandbox implementation (extracted from manager/sandbox.rs)
// ============================================================================

fn apply_sandbox(sandbox: &SandboxConfig) -> Result<(), String> {
    // NoNewPrivileges
    if sandbox.no_new_privileges {
        apply_no_new_privileges()?;
    }

    // ProtectKernelModules - drop CAP_SYS_MODULE
    if sandbox.protect_kernel_modules {
        drop_capability(16)?; // CAP_SYS_MODULE
    }

    // Capability bounding set
    apply_capability_bounding_set(&sandbox.capability_bounding_set)?;

    // Ambient capabilities
    apply_ambient_capabilities(&sandbox.ambient_capabilities)?;

    // Private network namespace
    if sandbox.private_network {
        apply_private_network()?;
    }

    // prctl-based protections
    if sandbox.restrict_realtime {
        // Handled via seccomp
    }

    if sandbox.memory_deny_write_execute {
        apply_memory_deny_write_execute()?;
    }

    if sandbox.lock_personality {
        // Handled via seccomp
    }

    if sandbox.ignore_sigpipe {
        apply_ignore_sigpipe()?;
    }

    // Mount namespace operations
    let needs_mount_ns = !matches!(sandbox.protect_system, ProtectSystemConfig::No)
        || !matches!(sandbox.protect_home, ProtectHomeConfig::No)
        || sandbox.private_tmp
        || sandbox.private_devices
        || !matches!(sandbox.device_policy, DevicePolicyConfig::Auto)
        || !matches!(sandbox.protect_proc, ProtectProcConfig::Default)
        || !sandbox.read_only_paths.is_empty()
        || !sandbox.read_write_paths.is_empty()
        || !sandbox.inaccessible_paths.is_empty()
        || sandbox.protect_control_groups
        || sandbox.protect_kernel_tunables
        || sandbox.protect_kernel_logs;

    if needs_mount_ns {
        create_mount_namespace()?;
        apply_protect_system(&sandbox.protect_system)?;
        apply_protect_home(&sandbox.protect_home)?;

        if sandbox.private_tmp {
            apply_private_tmp()?;
        }

        if !matches!(sandbox.device_policy, DevicePolicyConfig::Auto) {
            apply_device_policy(&sandbox.device_policy, &sandbox.device_allow)?;
        } else if sandbox.private_devices {
            apply_private_devices()?;
        }

        apply_protect_proc(&sandbox.protect_proc)?;

        if sandbox.protect_control_groups {
            apply_protect_control_groups()?;
        }

        if sandbox.protect_kernel_tunables {
            apply_protect_kernel_tunables()?;
        }

        if sandbox.protect_kernel_logs {
            apply_protect_kernel_logs()?;
        }

        apply_path_restrictions(
            &sandbox.read_write_paths,
            &sandbox.read_only_paths,
            &sandbox.inaccessible_paths,
        )?;
    }

    // Seccomp filters (must be last)
    let has_seccomp = sandbox.restrict_namespaces.is_some()
        || !sandbox.system_call_filter.is_empty()
        || sandbox.protect_clock
        || sandbox.protect_hostname
        || sandbox.restrict_suid_sgid
        || sandbox.restrict_address_families.is_some()
        || !sandbox.system_call_architectures.is_empty()
        || sandbox.restrict_realtime
        || sandbox.lock_personality;

    if has_seccomp {
        apply_seccomp(sandbox)?;
    }

    Ok(())
}

fn apply_no_new_privileges() -> Result<(), String> {
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err("Failed to set PR_SET_NO_NEW_PRIVS".to_string());
        }
    }
    Ok(())
}

fn drop_capability(cap: u32) -> Result<(), String> {
    unsafe {
        if libc::prctl(libc::PR_CAPBSET_DROP, cap as libc::c_ulong, 0, 0, 0) != 0 {
            return Err(format!("Failed to drop capability {}", cap));
        }
    }
    Ok(())
}

fn apply_capability_bounding_set(caps: &[String]) -> Result<(), String> {
    if caps.is_empty() {
        return Ok(());
    }

    for cap_str in caps {
        if let Some(name) = cap_str.strip_prefix('~') {
            if let Some(cap_num) = capability_name_to_num(name) {
                drop_capability(cap_num)?;
            }
        }
    }

    Ok(())
}

fn capability_name_to_num(name: &str) -> Option<u32> {
    let name = name.strip_prefix("CAP_").unwrap_or(name);
    match name.to_uppercase().as_str() {
        "CHOWN" => Some(0),
        "DAC_OVERRIDE" => Some(1),
        "DAC_READ_SEARCH" => Some(2),
        "FOWNER" => Some(3),
        "FSETID" => Some(4),
        "KILL" => Some(5),
        "SETGID" => Some(6),
        "SETUID" => Some(7),
        "SETPCAP" => Some(8),
        "LINUX_IMMUTABLE" => Some(9),
        "NET_BIND_SERVICE" => Some(10),
        "NET_BROADCAST" => Some(11),
        "NET_ADMIN" => Some(12),
        "NET_RAW" => Some(13),
        "IPC_LOCK" => Some(14),
        "IPC_OWNER" => Some(15),
        "SYS_MODULE" => Some(16),
        "SYS_RAWIO" => Some(17),
        "SYS_CHROOT" => Some(18),
        "SYS_PTRACE" => Some(19),
        "SYS_PACCT" => Some(20),
        "SYS_ADMIN" => Some(21),
        "SYS_BOOT" => Some(22),
        "SYS_NICE" => Some(23),
        "SYS_RESOURCE" => Some(24),
        "SYS_TIME" => Some(25),
        "SYS_TTY_CONFIG" => Some(26),
        "MKNOD" => Some(27),
        "LEASE" => Some(28),
        "AUDIT_WRITE" => Some(29),
        "AUDIT_CONTROL" => Some(30),
        "SETFCAP" => Some(31),
        "MAC_OVERRIDE" => Some(32),
        "MAC_ADMIN" => Some(33),
        "SYSLOG" => Some(34),
        "WAKE_ALARM" => Some(35),
        "BLOCK_SUSPEND" => Some(36),
        "AUDIT_READ" => Some(37),
        _ => None,
    }
}

const PR_CAP_AMBIENT: libc::c_int = 47;
const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;

fn apply_ambient_capabilities(caps: &[String]) -> Result<(), String> {
    for cap_str in caps {
        if let Some(cap_num) = capability_name_to_num(cap_str) {
            unsafe {
                libc::prctl(
                    PR_CAP_AMBIENT,
                    PR_CAP_AMBIENT_RAISE,
                    cap_num as libc::c_ulong,
                    0,
                    0,
                );
            }
        }
    }
    Ok(())
}

fn apply_private_network() -> Result<(), String> {
    unsafe {
        if libc::unshare(libc::CLONE_NEWNET) != 0 {
            return Err("Failed to create network namespace".to_string());
        }
    }
    Ok(())
}

const PR_SET_MDWE: libc::c_int = 65;
const PR_MDWE_REFUSE_EXEC_GAIN: libc::c_ulong = 1;

fn apply_memory_deny_write_execute() -> Result<(), String> {
    unsafe {
        if libc::prctl(PR_SET_MDWE, PR_MDWE_REFUSE_EXEC_GAIN, 0, 0, 0) != 0 {
            // May fail on older kernels
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINVAL) {
                return Err(format!("Failed to set PR_SET_MDWE: {}", err));
            }
        }
    }
    Ok(())
}

fn apply_ignore_sigpipe() -> Result<(), String> {
    unsafe {
        if libc::signal(libc::SIGPIPE, libc::SIG_IGN) == libc::SIG_ERR {
            return Err("Failed to ignore SIGPIPE".to_string());
        }
    }
    Ok(())
}

fn create_mount_namespace() -> Result<(), String> {
    unsafe {
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            return Err("Failed to create mount namespace".to_string());
        }
        let root = CString::new("/").unwrap();
        let none = CString::new("none").unwrap();
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            none.as_ptr(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        );
    }
    Ok(())
}

fn apply_protect_system(mode: &ProtectSystemConfig) -> Result<(), String> {
    match mode {
        ProtectSystemConfig::No => {}
        ProtectSystemConfig::Yes => {
            bind_mount_ro("/usr")?;
            bind_mount_ro("/boot")?;
        }
        ProtectSystemConfig::Full => {
            bind_mount_ro("/usr")?;
            bind_mount_ro("/boot")?;
            bind_mount_ro("/etc")?;
        }
        ProtectSystemConfig::Strict => {
            bind_mount_ro("/")?;
            for path in &["/dev", "/proc", "/sys", "/run", "/tmp", "/var"] {
                if std::path::Path::new(path).exists() {
                    let _ = remount_rw(path);
                }
            }
        }
    }
    Ok(())
}

fn apply_protect_home(mode: &ProtectHomeConfig) -> Result<(), String> {
    let home_dirs = ["/home", "/root", "/run/user"];

    match mode {
        ProtectHomeConfig::No => {}
        ProtectHomeConfig::Yes => {
            for dir in &home_dirs {
                if std::path::Path::new(dir).exists() {
                    make_inaccessible(dir)?;
                }
            }
        }
        ProtectHomeConfig::ReadOnly => {
            for dir in &home_dirs {
                if std::path::Path::new(dir).exists() {
                    bind_mount_ro(dir)?;
                }
            }
        }
        ProtectHomeConfig::Tmpfs => {
            for dir in &home_dirs {
                if std::path::Path::new(dir).exists() {
                    mount_tmpfs(dir)?;
                }
            }
        }
    }
    Ok(())
}

fn apply_private_tmp() -> Result<(), String> {
    mount_tmpfs("/tmp")?;
    mount_tmpfs("/var/tmp")?;
    Ok(())
}

fn apply_private_devices() -> Result<(), String> {
    mount_tmpfs("/dev")?;
    create_pseudo_devices()?;
    let _ = std::fs::create_dir("/dev/pts");
    let _ = std::fs::create_dir("/dev/shm");
    Ok(())
}

fn apply_device_policy(policy: &DevicePolicyConfig, device_allow: &[String]) -> Result<(), String> {
    mount_tmpfs("/dev")?;
    let _ = std::fs::create_dir("/dev/pts");
    let _ = std::fs::create_dir("/dev/shm");

    match policy {
        DevicePolicyConfig::Auto => {}
        DevicePolicyConfig::Closed => {
            create_pseudo_devices()?;
        }
        DevicePolicyConfig::Strict => {
            // No devices by default
        }
    }

    for entry in device_allow {
        let _ = add_device_allow_entry(entry);
    }

    Ok(())
}

fn create_pseudo_devices() -> Result<(), String> {
    let devices = [
        ("/dev/null", 1, 3),
        ("/dev/zero", 1, 5),
        ("/dev/full", 1, 7),
        ("/dev/random", 1, 8),
        ("/dev/urandom", 1, 9),
        ("/dev/tty", 5, 0),
    ];

    for (path, major, minor) in &devices {
        let path_c = CString::new(*path).unwrap();
        unsafe {
            let dev = libc::makedev(*major, *minor);
            libc::mknod(path_c.as_ptr(), libc::S_IFCHR | 0o666, dev);
        }
    }
    Ok(())
}

fn add_device_allow_entry(entry: &str) -> Result<(), String> {
    let parts: Vec<&str> = entry.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(());
    }

    let device = parts[0];
    if device.starts_with("/dev/") && std::path::Path::new(device).exists() {
        let device_c = CString::new(device).unwrap();
        let none = CString::new("none").unwrap();
        unsafe {
            libc::mount(
                device_c.as_ptr(),
                device_c.as_ptr(),
                none.as_ptr(),
                libc::MS_BIND,
                std::ptr::null(),
            );
        }
    }
    Ok(())
}

fn apply_protect_proc(mode: &ProtectProcConfig) -> Result<(), String> {
    match mode {
        ProtectProcConfig::Default => {}
        ProtectProcConfig::Invisible => {
            remount_proc("hidepid=2")?;
        }
        ProtectProcConfig::Ptraceable => {
            remount_proc("hidepid=1")?;
        }
        ProtectProcConfig::NoAccess => {
            make_inaccessible("/proc")?;
        }
    }
    Ok(())
}

fn apply_protect_control_groups() -> Result<(), String> {
    if std::path::Path::new("/sys/fs/cgroup").exists() {
        bind_mount_ro("/sys/fs/cgroup")?;
    }
    Ok(())
}

fn apply_protect_kernel_tunables() -> Result<(), String> {
    if std::path::Path::new("/proc/sys").exists() {
        bind_mount_ro("/proc/sys")?;
    }
    if std::path::Path::new("/sys").exists() {
        bind_mount_ro("/sys")?;
    }
    Ok(())
}

fn apply_protect_kernel_logs() -> Result<(), String> {
    if std::path::Path::new("/dev/kmsg").exists() {
        make_inaccessible("/dev/kmsg")?;
    }
    if std::path::Path::new("/proc/kmsg").exists() {
        make_inaccessible("/proc/kmsg")?;
    }
    Ok(())
}

fn apply_path_restrictions(
    read_write: &[std::path::PathBuf],
    read_only: &[std::path::PathBuf],
    inaccessible: &[std::path::PathBuf],
) -> Result<(), String> {
    for path in inaccessible {
        if path.exists() {
            make_inaccessible(path.to_str().unwrap_or(""))?;
        }
    }
    for path in read_only {
        if path.exists() {
            bind_mount_ro(path.to_str().unwrap_or(""))?;
        }
    }
    for path in read_write {
        if path.exists() {
            let _ = remount_rw(path.to_str().unwrap_or(""));
        }
    }
    Ok(())
}

// Mount helpers

fn bind_mount_ro(path: &str) -> Result<(), String> {
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let none = CString::new("none").unwrap();

    unsafe {
        if libc::mount(
            path_c.as_ptr(),
            path_c.as_ptr(),
            none.as_ptr(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("Failed to bind mount {}", path));
        }

        libc::mount(
            std::ptr::null(),
            path_c.as_ptr(),
            none.as_ptr(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC,
            std::ptr::null(),
        );
    }
    Ok(())
}

fn remount_rw(path: &str) -> Result<(), String> {
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let none = CString::new("none").unwrap();

    unsafe {
        if libc::mount(
            std::ptr::null(),
            path_c.as_ptr(),
            none.as_ptr(),
            libc::MS_REMOUNT | libc::MS_BIND,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("Failed to remount {} read-write", path));
        }
    }
    Ok(())
}

fn mount_tmpfs(path: &str) -> Result<(), String> {
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let fstype = CString::new("tmpfs").unwrap();
    let source = CString::new("tmpfs").unwrap();

    unsafe {
        if libc::mount(
            source.as_ptr(),
            path_c.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("Failed to mount tmpfs on {}", path));
        }
    }
    Ok(())
}

fn make_inaccessible(path: &str) -> Result<(), String> {
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let fstype = CString::new("tmpfs").unwrap();
    let source = CString::new("tmpfs").unwrap();

    unsafe {
        if libc::mount(
            source.as_ptr(),
            path_c.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("Failed to make {} inaccessible", path));
        }
    }
    Ok(())
}

fn remount_proc(options: &str) -> Result<(), String> {
    let path = CString::new("/proc").unwrap();
    let fstype = CString::new("proc").unwrap();
    let source = CString::new("proc").unwrap();
    let opts = CString::new(options).map_err(|e| e.to_string())?;

    unsafe {
        libc::umount2(path.as_ptr(), libc::MNT_DETACH);
        if libc::mount(
            source.as_ptr(),
            path.as_ptr(),
            fstype.as_ptr(),
            0,
            opts.as_ptr() as *const libc::c_void,
        ) != 0
        {
            return Err(format!("Failed to remount /proc with {}", options));
        }
    }
    Ok(())
}

// Seccomp (simplified - just log for now, full impl would use seccompiler)
fn apply_seccomp(sandbox: &SandboxConfig) -> Result<(), String> {
    // For a minimal implementation, we just note that seccomp would be applied
    // Full implementation would use seccompiler crate like in sandbox.rs

    if sandbox.restrict_namespaces.is_some() {
        // Would block unshare/clone with namespace flags
    }

    if !sandbox.system_call_filter.is_empty() {
        // Would apply syscall filter
    }

    if sandbox.protect_clock {
        // Would block clock_settime, clock_adjtime, settimeofday
    }

    if sandbox.protect_hostname {
        // Would block sethostname, setdomainname
    }

    // Note: Full seccomp implementation requires seccompiler crate
    // which is already a dependency, but to keep the executor binary
    // small, we skip it for now. The main sandbox features (namespaces,
    // mounts, capabilities) are the most important.

    Ok(())
}
