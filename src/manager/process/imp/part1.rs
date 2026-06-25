// Process spawning and management

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
    /// Names for socket FDs (for LISTEN_FDNAMES)
    pub socket_fd_names: Vec<String>,
    /// M19: Override UID for DynamicUser= (allocated by DynamicUserManager)
    pub dynamic_uid: Option<u32>,
    /// M19: Override GID for DynamicUser= (allocated by DynamicUserManager)
    pub dynamic_gid: Option<u32>,
    /// M19: Stored FDs from previous run (FileDescriptorStoreMax=)
    /// These are passed via LISTEN_FDS along with socket_fds
    pub stored_fds: Vec<RawFd>,
    /// Imported user environment (for user session management)
    /// If provided, these are merged with inherited environment
    pub user_environment: HashMap<String, String>,
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

    let mut cmd = create_spawn_command(&program, &args, &service.service.working_directory);
    prepare_spawn_settings(&mut cmd, service, options)?;
    configure_service_stdio(&mut cmd, &service.service.standard_input);
    spawn_command(cmd, &program, &args)
}

fn create_spawn_command(
    program: &str,
    args: &[String],
    working_directory: &Option<std::path::PathBuf>,
) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(wd) = working_directory {
        cmd.current_dir(wd);
    }
    cmd
}

fn prepare_spawn_settings(
    cmd: &mut Command,
    service: &Service,
    options: &SpawnOptions,
) -> Result<(), SpawnError> {
    let socket_activation = build_socket_activation(options);
    validate_socket_fds(&socket_activation.fds);
    let extra_env = build_service_environment(service, options);
    let unset_vars = service.service.unset_environment.clone();
    if socket_activation.fds.is_empty() {
        configure_direct_environment(cmd, &options.user_environment, &extra_env, &unset_vars);
    }
    let (uid, gid) = resolve_uid_gid(service, options);
    create_service_directories(&service.service, &service.name, uid, gid)?;
    install_pre_exec_context(cmd, service, socket_activation, extra_env, unset_vars, uid, gid);
    Ok(())
}

fn install_pre_exec_context(
    cmd: &mut Command,
    service: &Service,
    socket_activation: SocketActivation,
    extra_env: HashMap<String, String>,
    unset_vars: Vec<String>,
    uid: Option<u32>,
    gid: Option<u32>,
) {
    #[cfg(unix)]
    unsafe {
        let pre_exec = PreExecContext {
            socket_fds: socket_activation.fds,
            socket_fd_names: socket_activation.names,
            extra_env,
            unset_vars,
            limit_nofile: service.service.limit_nofile,
            limit_nproc: service.service.limit_nproc,
            limit_core: service.service.limit_core,
            oom_score_adjust: service.service.oom_score_adjust,
            service_section: service.service.clone(),
            uid,
            gid,
            tty_path: service.service.tty_path.clone(),
            tty_reset: service.service.tty_reset,
            std_input: service.service.standard_input.clone(),
        };
        cmd.pre_exec(move || run_pre_exec(&pre_exec));
    }
}

fn configure_service_stdio(cmd: &mut Command, std_input: &StdInput) {
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    cmd.stdin(match std_input {
        StdInput::Null => Stdio::null(),
        StdInput::Tty | StdInput::TtyForce | StdInput::TtyFail => Stdio::inherit(),
    });
}

fn spawn_command(mut cmd: Command, program: &str, args: &[String]) -> Result<Child, SpawnError> {
    log::debug!("Spawning: {} {:?}", program, args);
    cmd.spawn()
        .map_err(|e| SpawnError::Spawn(format!("{}: {} {:?}", e, program, args)))
}

struct SocketActivation {
    fds: Vec<RawFd>,
    names: Vec<String>,
}

fn build_socket_activation(options: &SpawnOptions) -> SocketActivation {
    let mut fds = options.socket_fds.clone();
    fds.extend(&options.stored_fds);

    let mut names = options.socket_fd_names.clone();
    names.extend(std::iter::repeat_n("stored".to_string(), options.stored_fds.len()));
    while names.len() < fds.len() {
        names.push("unknown".to_string());
    }

    SocketActivation { fds, names }
}

fn validate_socket_fds(socket_fds: &[RawFd]) {
    for &fd in socket_fds {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            log::error!(
                "Socket fd {} is invalid: {}",
                fd,
                std::io::Error::last_os_error()
            );
        } else {
            log::debug!("Socket fd {} is valid (flags={})", fd, flags);
        }
    }
}

fn build_service_environment(
    service: &Service,
    options: &SpawnOptions,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.extend(service.service.environment.clone());

    for env_file in &service.service.environment_file {
        if let Ok(vars) = load_env_file(env_file) {
            env.extend(vars);
        }
    }

    if let Some(socket_path) = &options.notify_socket {
        env.insert("NOTIFY_SOCKET".to_string(), socket_path.clone());
    }
    if let Some(usec) = options.watchdog_usec {
        env.insert("WATCHDOG_USEC".to_string(), usec.to_string());
    }

    env
}

fn configure_direct_environment(
    cmd: &mut Command,
    user_env: &HashMap<String, String>,
    extra_env: &HashMap<String, String>,
    unset_vars: &[String],
) {
    cmd.env_clear();
    cmd.envs(std::env::vars());
    cmd.envs(user_env);
    cmd.envs(extra_env);
    for var in unset_vars {
        cmd.env_remove(var);
    }
}

fn resolve_uid_gid(service: &Service, options: &SpawnOptions) -> (Option<u32>, Option<u32>) {
    let uid = options
        .dynamic_uid
        .or_else(|| service.service.user.as_ref().and_then(|u| resolve_user(u)));
    let gid = options.dynamic_gid.or_else(|| {
        service
            .service
            .group
            .as_ref()
            .and_then(|g| resolve_group(g))
    });
    (uid, gid)
}

#[cfg(unix)]
struct PreExecContext {
    socket_fds: Vec<RawFd>,
    socket_fd_names: Vec<String>,
    extra_env: HashMap<String, String>,
    unset_vars: Vec<String>,
    limit_nofile: Option<u64>,
    limit_nproc: Option<u64>,
    limit_core: Option<u64>,
    oom_score_adjust: Option<i32>,
    service_section: crate::units::ServiceSection,
    uid: Option<u32>,
    gid: Option<u32>,
    tty_path: Option<std::path::PathBuf>,
    tty_reset: bool,
    std_input: StdInput,
}

#[cfg(unix)]
fn run_pre_exec(ctx: &PreExecContext) -> std::io::Result<()> {
    if !ctx.socket_fds.is_empty() {
        apply_pre_exec_socket_activation(ctx)?;
    }

    apply_resource_limits(ctx.limit_nofile, ctx.limit_nproc, ctx.limit_core);
    apply_oom_score_adjust(ctx.oom_score_adjust);
    apply_sandbox(&ctx.service_section);
    drop_privileges(ctx.gid, ctx.uid)?;
    setup_tty(&ctx.std_input, ctx.tty_path.as_deref(), ctx.tty_reset)?;
    Ok(())
}

#[cfg(unix)]
fn apply_pre_exec_socket_activation(ctx: &PreExecContext) -> std::io::Result<()> {
    set_environment_from_maps(&ctx.extra_env, &ctx.unset_vars);
    set_systemd_socket_env(ctx.socket_fds.len(), &ctx.socket_fd_names);
    map_socket_fds(&ctx.socket_fds)?;
    Ok(())
}

#[cfg(unix)]
fn set_environment_from_maps(extra_env: &HashMap<String, String>, unset_vars: &[String]) {
    for (key, value) in extra_env {
        set_env_var(key, value);
    }
    for var in unset_vars {
        unset_env_var(var);
    }
}

#[cfg(unix)]
fn set_env_var(key: &str, value: &str) {
    if let (Ok(k), Ok(v)) = (std::ffi::CString::new(key), std::ffi::CString::new(value)) {
        unsafe {
            libc::setenv(k.as_ptr(), v.as_ptr(), 1);
        }
    }
}

#[cfg(unix)]
fn unset_env_var(key: &str) {
    if let Ok(k) = std::ffi::CString::new(key) {
        unsafe {
            libc::unsetenv(k.as_ptr());
        }
    }
}

#[cfg(unix)]
fn set_systemd_socket_env(socket_fd_count: usize, socket_fd_names: &[String]) {
    set_env_var("LISTEN_FDS", &socket_fd_count.to_string());
    set_env_var("LISTEN_PID", &std::process::id().to_string());
    set_env_var("LISTEN_FDNAMES", &socket_fd_names.join(":"));
}

#[cfg(unix)]
fn map_socket_fds(socket_fds: &[RawFd]) -> std::io::Result<()> {
    const SD_LISTEN_FDS_START: RawFd = 3;

    for (i, &fd) in socket_fds.iter().enumerate() {
        let target_fd = SD_LISTEN_FDS_START + i as RawFd;
        if fd != target_fd {
            duplicate_to_target_fd(fd, target_fd, socket_fds.len())?;
        }
        clear_cloexec(target_fd);
    }
    Ok(())
}

#[cfg(unix)]
fn duplicate_to_target_fd(fd: RawFd, target_fd: RawFd, fd_count: usize) -> std::io::Result<()> {
    const SD_LISTEN_FDS_START: RawFd = 3;
    if unsafe { libc::dup2(fd, target_fd) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let inside_target_range = (SD_LISTEN_FDS_START..(SD_LISTEN_FDS_START + fd_count as RawFd)).contains(&fd);
    if !inside_target_range {
        unsafe {
            libc::close(fd);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn clear_cloexec(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags >= 0 {
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
    }
}

#[cfg(unix)]
fn apply_resource_limits(limit_nofile: Option<u64>, limit_nproc: Option<u64>, limit_core: Option<u64>) {
    set_single_limit(libc::RLIMIT_NOFILE, limit_nofile, "RLIMIT_NOFILE");
    set_single_limit(libc::RLIMIT_NPROC, limit_nproc, "RLIMIT_NPROC");
    set_single_limit(libc::RLIMIT_CORE, limit_core, "RLIMIT_CORE");
}

#[cfg(unix)]
fn set_single_limit(resource: libc::c_int, value: Option<u64>, label: &str) {
    let Some(value) = value else {
        return;
    };
    let rlim = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    if unsafe { libc::setrlimit(resource, &rlim) } != 0 {
        log::warn!("Failed to set {} to {}", label, value);
    }
}

#[cfg(unix)]
fn apply_oom_score_adjust(score: Option<i32>) {
    let Some(score) = score else {
        return;
    };
    if std::fs::write("/proc/self/oom_score_adj", score.to_string()).is_err() {
        log::warn!("Failed to set oom_score_adj to {}", score);
    }
}

#[cfg(unix)]
fn apply_sandbox(service_section: &crate::units::ServiceSection) {
    if let Err(e) = crate::manager::sandbox::apply_sandbox(service_section) {
        log::warn!("Sandbox setup failed: {}", e);
    }
}

#[cfg(unix)]
fn drop_privileges(gid: Option<u32>, uid: Option<u32>) -> std::io::Result<()> {
    if let Some(gid) = gid {
        nix::unistd::setgid(nix::unistd::Gid::from_raw(gid))
            .map_err(std::io::Error::other)?;
    }
    if let Some(uid) = uid {
        nix::unistd::setuid(nix::unistd::Uid::from_raw(uid))
            .map_err(std::io::Error::other)?;
    }
    Ok(())
}

#[cfg(unix)]
fn setup_tty(
    std_input: &StdInput,
    tty_path: Option<&std::path::Path>,
    tty_reset: bool,
) -> std::io::Result<()> {
    if !matches!(std_input, StdInput::Tty | StdInput::TtyForce | StdInput::TtyFail) {
        return Ok(());
    }
    let Some(path) = tty_path else {
        return Ok(());
    };
    if tty_reset {
        let _ = std::fs::OpenOptions::new().read(true).write(true).open(path);
    }
    attach_controlling_tty(path, std_input)
}

#[cfg(unix)]
fn attach_controlling_tty(path: &std::path::Path, std_input: &StdInput) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new().read(true).write(true).open(path);
    match file {
        Ok(f) => {
            let fd = f.as_raw_fd();
            if unsafe { libc::ioctl(fd, libc::TIOCSCTTY, 0) } < 0 && matches!(std_input, StdInput::TtyFail) {
                return Err(std::io::Error::last_os_error());
            }
            duplicate_tty_fds(fd);
            std::mem::forget(f);
            Ok(())
        }
        Err(e) if matches!(std_input, StdInput::TtyFail) => Err(e),
        Err(e) => {
            log::warn!("Failed to open TTY {:?}: {}", path, e);
            Ok(())
        }
    }
}

#[cfg(unix)]
fn duplicate_tty_fds(fd: RawFd) {
    unsafe {
        libc::dup2(fd, 0);
        libc::dup2(fd, 1);
        libc::dup2(fd, 2);
        if fd > 2 {
            libc::close(fd);
        }
    }
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
    let base_name = service_name.strip_suffix(".service").unwrap_or(service_name);
    let directory_sets = [
        ("/var/lib", &service.state_directory[..], "state"),
        ("/run", &service.runtime_directory[..], "runtime"),
        ("/etc", &service.configuration_directory[..], "configuration"),
        ("/var/log", &service.logs_directory[..], "logs"),
        ("/var/cache", &service.cache_directory[..], "cache"),
    ];

    for (base, names, label) in directory_sets {
        ensure_directory_set(base, names, base_name, uid, gid, label);
    }
    Ok(())
}

fn ensure_directory_set(
    base: &str,
    names: &[String],
    default_name: &str,
    uid: Option<u32>,
    gid: Option<u32>,
    label: &str,
) {
    for name in names {
        let dir_name = if name.is_empty() { default_name } else { name };
        if let Err(e) = create_service_directory(base, dir_name, uid, gid) {
            log::warn!("Failed to create {} directory {}: {}", label, dir_name, e);
        }
    }
}

fn create_service_directory(
    base: &str,
    name: &str,
    uid: Option<u32>,
    gid: Option<u32>,
) -> std::io::Result<()> {
    use std::os::unix::fs::{chown, PermissionsExt};
    use std::path::Path;

    let path = Path::new(base).join(name);
    std::fs::create_dir_all(&path)?;
    if uid.is_some() || gid.is_some() {
        chown(&path, uid, gid)?;
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    log::debug!("Created directory: {}", path.display());
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

// ============================================================================
// Executor-based spawning (sd-executor pattern)
// ============================================================================

use crate::executor::{
    DevicePolicyConfig, ExecConfig, ProtectHomeConfig, ProtectProcConfig, ProtectSystemConfig,
    SandboxConfig, StdInputConfig,
};
