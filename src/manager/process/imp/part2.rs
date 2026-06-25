/// Build ExecConfig from Service and SpawnOptions
/// `command_index` specifies which ExecStart command to use (0 = first)
pub fn build_exec_config(
    service: &Service,
    options: &SpawnOptions,
    command_index: usize,
) -> Result<ExecConfig, SpawnError> {
    let exec_start = service
        .service
        .exec_start
        .get(command_index)
        .ok_or_else(|| SpawnError::NoExecStart(service.name.clone()))?;

    let exec_start = substitute_specifiers(exec_start, service);
    let (program, args) = parse_command(&exec_start)?;

    let (uid, gid) = resolve_uid_gid(service, options);
    let environment = build_exec_environment(service, options);
    let socket_activation = build_socket_activation(options);

    log::debug!(
        "{}: sandbox: protect_system={:?}, private_tmp={}, private_devices={}",
        service.name,
        service.service.protect_system,
        service.service.private_tmp,
        service.service.private_devices
    );

    let sandbox = build_sandbox_config(&service.service);
    let std_input = map_std_input(service.service.standard_input.clone());
    Ok(build_exec_config_output(
        service,
        program,
        args,
        environment,
        uid,
        gid,
        socket_activation,
        std_input,
        sandbox,
    ))
}

fn build_exec_config_output(
    service: &Service,
    program: String,
    args: Vec<String>,
    environment: HashMap<String, String>,
    uid: Option<u32>,
    gid: Option<u32>,
    socket_activation: SocketActivation,
    std_input: StdInputConfig,
    sandbox: SandboxConfig,
) -> ExecConfig {
    ExecConfig {
        program,
        args,
        working_directory: service.service.working_directory.clone(),
        environment,
        unset_environment: service.service.unset_environment.clone(),
        uid,
        gid,
        limit_nofile: service.service.limit_nofile,
        limit_nproc: service.service.limit_nproc,
        limit_core: service.service.limit_core,
        oom_score_adjust: service.service.oom_score_adjust,
        socket_fd_count: socket_activation.fds.len(),
        socket_fd_names: socket_activation.names,
        std_input,
        tty_path: service.service.tty_path.clone(),
        tty_reset: service.service.tty_reset,
        sandbox,
    }
}

fn build_exec_environment(service: &Service, options: &SpawnOptions) -> HashMap<String, String> {
    let mut environment: HashMap<String, String> = std::env::vars().collect();
    environment.extend(options.user_environment.clone());
    environment.extend(build_service_environment(service, options));
    environment
}

fn map_std_input(std_input: StdInput) -> StdInputConfig {
    match std_input {
        StdInput::Null => StdInputConfig::Null,
        StdInput::Tty => StdInputConfig::Tty,
        StdInput::TtyForce => StdInputConfig::TtyForce,
        StdInput::TtyFail => StdInputConfig::TtyFail,
    }
}

fn build_sandbox_config(service: &crate::units::ServiceSection) -> SandboxConfig {
    let mut sandbox = SandboxConfig::default();
    fill_sandbox_basic_fields(&mut sandbox, service);
    fill_sandbox_path_fields(&mut sandbox, service);
    fill_sandbox_security_fields(&mut sandbox, service);
    sandbox
}

fn fill_sandbox_basic_fields(sandbox: &mut SandboxConfig, service: &crate::units::ServiceSection) {
    sandbox.no_new_privileges = service.no_new_privileges;
    sandbox.protect_system = map_protect_system(&service.protect_system);
    sandbox.protect_home = map_protect_home(&service.protect_home);
    sandbox.private_tmp = service.private_tmp;
    sandbox.private_devices = service.private_devices;
    sandbox.private_network = service.private_network;
    sandbox.protect_kernel_modules = service.protect_kernel_modules;
    sandbox.protect_proc = map_protect_proc(&service.protect_proc);
    sandbox.capability_bounding_set = service.capability_bounding_set.clone();
    sandbox.ambient_capabilities = service.ambient_capabilities.clone();
    sandbox.restrict_namespaces = service.restrict_namespaces.clone();
    sandbox.device_policy = map_device_policy(&service.device_policy);
    sandbox.device_allow = service.device_allow.clone();
}

fn fill_sandbox_path_fields(sandbox: &mut SandboxConfig, service: &crate::units::ServiceSection) {
    sandbox.read_write_paths = service.read_write_paths.clone();
    sandbox.read_only_paths = service.read_only_paths.clone();
    sandbox.inaccessible_paths = service.inaccessible_paths.clone();
    sandbox.system_call_filter = service.system_call_filter.clone();
    sandbox.system_call_error_number = service.system_call_error_number;
    sandbox.system_call_architectures = service.system_call_architectures.clone();
}

fn fill_sandbox_security_fields(
    sandbox: &mut SandboxConfig,
    service: &crate::units::ServiceSection,
) {
    sandbox.restrict_realtime = service.restrict_realtime;
    sandbox.protect_control_groups = service.protect_control_groups;
    sandbox.memory_deny_write_execute = service.memory_deny_write_execute;
    sandbox.lock_personality = service.lock_personality;
    sandbox.protect_kernel_tunables = service.protect_kernel_tunables;
    sandbox.protect_kernel_logs = service.protect_kernel_logs;
    sandbox.protect_clock = service.protect_clock;
    sandbox.protect_hostname = service.protect_hostname;
    sandbox.ignore_sigpipe = service.ignore_sigpipe;
    sandbox.restrict_suid_sgid = service.restrict_suid_sgid;
    sandbox.restrict_address_families = service.restrict_address_families.clone();
}

fn map_protect_system(mode: &crate::units::ProtectSystem) -> ProtectSystemConfig {
    match mode {
        crate::units::ProtectSystem::No => ProtectSystemConfig::No,
        crate::units::ProtectSystem::Yes => ProtectSystemConfig::Yes,
        crate::units::ProtectSystem::Full => ProtectSystemConfig::Full,
        crate::units::ProtectSystem::Strict => ProtectSystemConfig::Strict,
    }
}

fn map_protect_home(mode: &crate::units::ProtectHome) -> ProtectHomeConfig {
    match mode {
        crate::units::ProtectHome::No => ProtectHomeConfig::No,
        crate::units::ProtectHome::Yes => ProtectHomeConfig::Yes,
        crate::units::ProtectHome::ReadOnly => ProtectHomeConfig::ReadOnly,
        crate::units::ProtectHome::Tmpfs => ProtectHomeConfig::Tmpfs,
    }
}

fn map_protect_proc(mode: &crate::units::ProtectProc) -> ProtectProcConfig {
    match mode {
        crate::units::ProtectProc::Default => ProtectProcConfig::Default,
        crate::units::ProtectProc::Invisible => ProtectProcConfig::Invisible,
        crate::units::ProtectProc::Ptraceable => ProtectProcConfig::Ptraceable,
        crate::units::ProtectProc::NoAccess => ProtectProcConfig::NoAccess,
    }
}

fn map_device_policy(policy: &crate::units::DevicePolicy) -> DevicePolicyConfig {
    match policy {
        crate::units::DevicePolicy::Auto => DevicePolicyConfig::Auto,
        crate::units::DevicePolicy::Closed => DevicePolicyConfig::Closed,
        crate::units::DevicePolicy::Strict => DevicePolicyConfig::Strict,
    }
}

/// Spawn a process using the executor pattern
///
/// This avoids CoW memory issues by:
/// 1. Serializing execution config to a memfd
/// 2. Spawning sysd-executor with the memfd FD
/// 3. Executor deserializes, applies sandbox, and execs target
/// `command_index` specifies which ExecStart command to use (0 = first)
pub fn spawn_service_via_executor(
    service: &Service,
    options: &SpawnOptions,
    executor_path: &str,
    command_index: usize,
) -> Result<Child, SpawnError> {
    if executor_path.is_empty() {
        return spawn_service_with_options(service, options);
    }

    let config = build_exec_config(service, options, command_index)?;
    create_service_directories(&service.service, &service.name, config.uid, config.gid)?;
    let memfd = crate::executor::serialize_to_memfd(&config)
        .map_err(|e| SpawnError::Spawn(format!("Failed to serialize config: {}", e)))?;
    log::debug!("{}: memfd created at fd {}", service.name, memfd);

    let all_fds = build_socket_activation(options).fds;
    let mut cmd = Command::new(executor_path);
    cmd.arg(format!("--deserialize={}", memfd));
    configure_executor_stdio(&mut cmd, &service.service.standard_input);
    configure_executor_pre_exec(&mut cmd, all_fds, memfd);

    log::debug!(
        "Spawning via executor: {} -> {} {}",
        executor_path,
        config.program,
        config.args.join(" ")
    );

    let result = cmd
        .spawn()
        .map_err(|e| SpawnError::Spawn(format!("Failed to spawn executor: {}", e)));

    // Close memfd in parent - child has its own copy after fork
    // This prevents FD leak on repeated spawns (especially during service restarts)
    unsafe {
        libc::close(memfd);
    }

    result
}

fn configure_executor_stdio(cmd: &mut Command, std_input: &StdInput) {
    cmd.stdin(match std_input {
        StdInput::Null => Stdio::null(),
        StdInput::Tty | StdInput::TtyForce | StdInput::TtyFail => Stdio::inherit(),
    });
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
}

fn configure_executor_pre_exec(cmd: &mut Command, all_fds: Vec<RawFd>, memfd: RawFd) {
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || prepare_executor_child_fds(&all_fds, memfd));
    }
}

#[cfg(unix)]
fn prepare_executor_child_fds(all_fds: &[RawFd], memfd: RawFd) -> std::io::Result<()> {
    map_socket_fds(all_fds)?;
    clear_cloexec(memfd);
    Ok(())
}
