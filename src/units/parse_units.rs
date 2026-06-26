//! Unit parsing implementation extracted from mod.rs.

use super::*;
use std::path::{Path, PathBuf};

struct SectionView<'a> {
    section: Option<&'a parser::ParsedSection>,
}

impl<'a> SectionView<'a> {
    fn from(parsed: &'a ParsedFile, name: &str) -> Self {
        Self {
            section: parsed.get(name),
        }
    }

    fn has(&self, key: &str) -> bool {
        self.section
            .is_some_and(|section| section.contains_key(key))
    }

    fn values(&self, key: &str) -> Option<&'a [(u32, String)]> {
        self.section
            .and_then(|section| section.get(key).map(Vec::as_slice))
    }

    fn first(&self, key: &str) -> Option<&'a str> {
        self.values(key)
            .and_then(|values| values.first().map(|(_, value)| value.as_str()))
    }

    fn strings(&self, key: &str) -> Vec<String> {
        self.values(key)
            .map(|values| values.iter().map(|(_, value)| value.clone()).collect())
            .unwrap_or_default()
    }

    fn words(&self, key: &str) -> Vec<String> {
        self.values(key)
            .map(|values| {
                values
                    .iter()
                    .flat_map(|(_, value)| value.split_whitespace().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn first_string(&self, key: &str) -> Option<String> {
        self.first(key).map(String::from)
    }

    fn first_pathbuf(&self, key: &str) -> Option<PathBuf> {
        self.first(key).map(PathBuf::from)
    }

    fn first_bool(&self, key: &str) -> Option<bool> {
        self.first(key).map(parse_yes_no)
    }

    fn first_parsed<T, F>(&self, key: &str, parse: F) -> Option<T>
    where
        F: Fn(&str) -> Option<T>,
    {
        self.first(key).and_then(parse)
    }

    fn parsed_or_default<T, F>(&self, key: &str, parse: F) -> T
    where
        T: Default,
        F: Fn(&str) -> Option<T>,
    {
        self.first_parsed(key, parse).unwrap_or_default()
    }
}

fn parse_yes_no(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "yes" | "true" | "1" | "on"
    )
}

fn parse_limit(value: &str) -> Option<u64> {
    if value.eq_ignore_ascii_case("infinity") {
        Some(u64::MAX)
    } else {
        value.parse().ok()
    }
}

fn parse_octal(value: &str) -> Option<u32> {
    u32::from_str_radix(value.trim_start_matches('0'), 8).ok()
}

fn parse_restrict_namespaces(value: &str) -> Option<Vec<String>> {
    let lower = value.to_ascii_lowercase();
    if matches!(lower.as_str(), "yes" | "true" | "1" | "on") {
        return Some(Vec::new());
    }
    if matches!(lower.as_str(), "no" | "false" | "0" | "off") {
        return None;
    }
    Some(value.split_whitespace().map(String::from).collect())
}

fn apply_unit_core(unit: &mut UnitSection, view: &SectionView<'_>) {
    unit.description = view.first_string("DESCRIPTION");
    unit.after = view.strings("AFTER");
    unit.before = view.strings("BEFORE");
    unit.requires = view.strings("REQUIRES");
    unit.wants = view.strings("WANTS");
    unit.conflicts = view.strings("CONFLICTS");
    unit.default_dependencies = view
        .first_bool("DEFAULTDEPENDENCIES")
        .unwrap_or(unit.default_dependencies);
}

fn apply_unit_conditions(unit: &mut UnitSection, view: &SectionView<'_>) {
    unit.condition_path_exists = view.strings("CONDITIONPATHEXISTS");
    unit.condition_directory_not_empty = view.strings("CONDITIONDIRECTORYNOTEMPTY");
    unit.condition_virtualization = view.strings("CONDITIONVIRTUALIZATION");
    unit.condition_capability = view.strings("CONDITIONCAPABILITY");
    unit.condition_kernel_command_line = view.strings("CONDITIONKERNELCOMMANDLINE");
    unit.condition_security = view.strings("CONDITIONSECURITY");
    unit.condition_first_boot = view.first_bool("CONDITIONFIRSTBOOT");
    unit.condition_needs_update = view.strings("CONDITIONNEEDSUPDATE");
}

fn apply_unit_service_extras(unit: &mut UnitSection, view: &SectionView<'_>) {
    unit.binds_to = view.strings("BINDSTO");
    unit.ignore_on_isolate = view
        .first_bool("IGNOREONISOLATE")
        .unwrap_or(unit.ignore_on_isolate);
}

fn apply_install_core(install: &mut InstallSection, view: &SectionView<'_>) {
    install.wanted_by = view.strings("WANTEDBY");
    install.required_by = view.strings("REQUIREDBY");
}

fn apply_install_extended(install: &mut InstallSection, view: &SectionView<'_>) {
    apply_install_core(install, view);
    install.also = view.strings("ALSO");
    install.alias = view.strings("ALIAS");
    install.default_instance = view.first_string("DEFAULTINSTANCE");
}

fn apply_install_without_default_instance(install: &mut InstallSection, view: &SectionView<'_>) {
    apply_install_core(install, view);
    install.also = view.strings("ALSO");
    install.alias = view.strings("ALIAS");
}

fn apply_service_exec_and_restart(service: &mut ServiceSection, view: &SectionView<'_>) {
    service.service_type = view.parsed_or_default("TYPE", ServiceType::parse);
    service.exec_start = view.strings("EXECSTART");
    service.exec_start_pre = view.strings("EXECSTARTPRE");
    service.exec_start_post = view.strings("EXECSTARTPOST");
    service.exec_stop = view.strings("EXECSTOP");
    service.exec_reload = view.strings("EXECRELOAD");
    service.restart = view.parsed_or_default("RESTART", RestartPolicy::parse);
    service.restart_sec = view
        .first_parsed("RESTARTSEC", parse_duration)
        .unwrap_or(service.restart_sec);
    service.timeout_start_sec = view.first_parsed("TIMEOUTSTARTSEC", parse_duration);
    service.timeout_stop_sec = view.first_parsed("TIMEOUTSTOPSEC", parse_duration);
    service.remain_after_exit = view
        .first_bool("REMAINAFTEREXIT")
        .unwrap_or(service.remain_after_exit);
}

fn apply_service_identity(service: &mut ServiceSection, view: &SectionView<'_>) {
    service.watchdog_sec = view.first_parsed("WATCHDOGSEC", parse_duration);
    service.notify_access = view.parsed_or_default("NOTIFYACCESS", NotifyAccess::parse);
    service.pid_file = view.first_pathbuf("PIDFILE");
    service.bus_name = view.first_string("BUSNAME");
    service.kill_mode = view.parsed_or_default("KILLMODE", KillMode::parse);
    service.user = view.first_string("USER");
    service.group = view.first_string("GROUP");
    service.working_directory = view.first_pathbuf("WORKINGDIRECTORY");
}

fn apply_service_environment(service: &mut ServiceSection, view: &SectionView<'_>) {
    service.environment = view
        .values("ENVIRONMENT")
        .map(|values| {
            values
                .iter()
                .filter_map(|(_, value)| parser::parse_environment(value).ok())
                .flatten()
                .collect()
        })
        .unwrap_or_default();
    service.environment_file = view
        .strings("ENVIRONMENTFILE")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    service.unset_environment = view.words("UNSETENVIRONMENT");
}

fn apply_service_stdio(service: &mut ServiceSection, view: &SectionView<'_>) {
    service.standard_output = view.parsed_or_default("STANDARDOUTPUT", StdOutput::parse);
    service.standard_error = view.parsed_or_default("STANDARDERROR", StdOutput::parse);
    service.standard_input = view.parsed_or_default("STANDARDINPUT", StdInput::parse);
    service.tty_path = view.first_pathbuf("TTYPATH");
    service.tty_reset = view.first_bool("TTYRESET").unwrap_or(service.tty_reset);
}

fn apply_service_limits(service: &mut ServiceSection, view: &SectionView<'_>) {
    service.memory_max = view.first_parsed("MEMORYMAX", parse_memory);
    service.cpu_quota = view.first_parsed("CPUQUOTA", parse_cpu_quota);
    service.tasks_max = view.first_parsed("TASKSMAX", |raw| raw.parse().ok());
    service.limit_nofile = view.first_parsed("LIMITNOFILE", parse_limit);
    service.limit_nproc = view.first_parsed("LIMITNPROC", parse_limit);
    service.limit_core = view.first_parsed("LIMITCORE", parse_limit);
    service.state_directory = view.words("STATEDIRECTORY");
    service.runtime_directory = view.words("RUNTIMEDIRECTORY");
    service.configuration_directory = view.words("CONFIGURATIONDIRECTORY");
    service.logs_directory = view.words("LOGSDIRECTORY");
    service.cache_directory = view.words("CACHEDIRECTORY");
    service.runtime_directory_preserve =
        view.parsed_or_default("RUNTIMEDIRECTORYPRESERVE", RuntimeDirectoryPreserve::parse);
    service.dynamic_user = view
        .first_bool("DYNAMICUSER")
        .unwrap_or(service.dynamic_user);
}

fn apply_service_security_core(service: &mut ServiceSection, view: &SectionView<'_>) {
    service.oom_score_adjust = view.first_parsed("OOMSCOREADJUST", |raw| raw.parse().ok());
    service.no_new_privileges = view
        .first_bool("NONEWPRIVILEGES")
        .unwrap_or(service.no_new_privileges);
    service.protect_system = view.parsed_or_default("PROTECTSYSTEM", ProtectSystem::parse);
    service.protect_home = view.parsed_or_default("PROTECTHOME", ProtectHome::parse);
    service.private_tmp = view.first_bool("PRIVATETMP").unwrap_or(service.private_tmp);
    service.private_devices = view
        .first_bool("PRIVATEDEVICES")
        .unwrap_or(service.private_devices);
    service.private_network = view
        .first_bool("PRIVATENETWORK")
        .unwrap_or(service.private_network);
    service.protect_kernel_modules = view
        .first_bool("PROTECTKERNELMODULES")
        .unwrap_or(service.protect_kernel_modules);
    service.protect_proc = view.parsed_or_default("PROTECTPROC", ProtectProc::parse);
    service.capability_bounding_set = view.words("CAPABILITYBOUNDINGSET");
    service.ambient_capabilities = view.words("AMBIENTCAPABILITIES");
    service.restrict_namespaces = view
        .first("RESTRICTNAMESPACES")
        .and_then(parse_restrict_namespaces);
}

fn apply_service_security_paths(service: &mut ServiceSection, view: &SectionView<'_>) {
    service.read_write_paths = view
        .words("READWRITEPATHS")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    service.read_only_paths = view
        .words("READONLYPATHS")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    service.inaccessible_paths = view
        .words("INACCESSIBLEPATHS")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    service.system_call_filter = view.words("SYSTEMCALLFILTER");
    service.device_policy = view.parsed_or_default("DEVICEPOLICY", DevicePolicy::parse);
    service.device_allow = view.strings("DEVICEALLOW");
}

fn apply_service_security_extended(service: &mut ServiceSection, view: &SectionView<'_>) {
    service.restrict_realtime = view
        .first_bool("RESTRICTREALTIME")
        .unwrap_or(service.restrict_realtime);
    service.protect_control_groups = view
        .first_bool("PROTECTCONTROLGROUPS")
        .unwrap_or(service.protect_control_groups);
    service.memory_deny_write_execute = view
        .first_bool("MEMORYDENYWRITEEXECUTE")
        .unwrap_or(service.memory_deny_write_execute);
    service.lock_personality = view
        .first_bool("LOCKPERSONALITY")
        .unwrap_or(service.lock_personality);
    service.protect_kernel_tunables = view
        .first_bool("PROTECTKERNELTUNABLES")
        .unwrap_or(service.protect_kernel_tunables);
    service.protect_kernel_logs = view
        .first_bool("PROTECTKERNELLOGS")
        .unwrap_or(service.protect_kernel_logs);
    service.protect_clock = view
        .first_bool("PROTECTCLOCK")
        .unwrap_or(service.protect_clock);
    service.protect_hostname = view
        .first_bool("PROTECTHOSTNAME")
        .unwrap_or(service.protect_hostname);
    service.ignore_sigpipe = view
        .first_bool("IGNORESIGPIPE")
        .unwrap_or(service.ignore_sigpipe);
    service.restrict_suid_sgid = view
        .first_bool("RESTRICTSUIDSGID")
        .unwrap_or(service.restrict_suid_sgid);
}

fn apply_service_process_control(service: &mut ServiceSection, view: &SectionView<'_>) {
    if view.has("RESTRICTADDRESSFAMILIES") {
        service.restrict_address_families = Some(view.words("RESTRICTADDRESSFAMILIES"));
    }
    service.system_call_error_number =
        view.first_parsed("SYSTEMCALLERRORNUMBER", |raw| raw.parse().ok());
    service.system_call_architectures = view.words("SYSTEMCALLARCHITECTURES");
    service.start_limit_burst = view.first_parsed("STARTLIMITBURST", |raw| raw.parse().ok());
    service.start_limit_interval_sec = view.first_parsed("STARTLIMITINTERVALSEC", parse_duration);
    service.sockets = view.words("SOCKETS");
    service.send_sighup = view.first_bool("SENDSIGHUP").unwrap_or(service.send_sighup);
    service.slice = view.first_string("SLICE");
    service.delegate = view.first_bool("DELEGATE").unwrap_or(service.delegate);
    service.exec_stop_post = view.strings("EXECSTOPPOST");
    service.file_descriptor_store_max =
        view.first_parsed("FILEDESCRIPTORSTOREMAX", |raw| raw.parse().ok());
    service.restart_prevent_exit_status = view
        .words("RESTARTPREVENTEXITSTATUS")
        .into_iter()
        .filter_map(|raw| raw.parse::<i32>().ok())
        .collect();
}

fn apply_mount_section(mount: &mut MountSection, view: &SectionView<'_>) {
    mount.what = view.first_string("WHAT").unwrap_or_default();
    mount.r#where = view.first_string("WHERE").unwrap_or_default();
    mount.fs_type = view.first_string("TYPE");
    mount.options = view.first_string("OPTIONS");
    mount.sloppy_options = view
        .first_bool("SLOPPYOPTIONS")
        .unwrap_or(mount.sloppy_options);
    mount.lazy_unmount = view.first_bool("LAZYUNMOUNT").unwrap_or(mount.lazy_unmount);
    mount.force_unmount = view
        .first_bool("FORCEUNMOUNT")
        .unwrap_or(mount.force_unmount);
    mount.read_write_only = view
        .first_bool("READWRITEONLY")
        .unwrap_or(mount.read_write_only);
    mount.directory_mode = view.first_parsed("DIRECTORYMODE", parse_octal);
    mount.timeout_sec = view.first_parsed("TIMEOUTSEC", parse_duration);
}

fn apply_socket_listeners(socket: &mut SocketSection, view: &SectionView<'_>) {
    socket.listeners = view
        .strings("LISTENSTREAM")
        .into_iter()
        .map(|address| Listener {
            address,
            listen_type: ListenType::Stream,
        })
        .chain(
            view.strings("LISTENDATAGRAM")
                .into_iter()
                .map(|address| Listener {
                    address,
                    listen_type: ListenType::Datagram,
                }),
        )
        .chain(
            view.strings("LISTENFIFO")
                .into_iter()
                .map(|address| Listener {
                    address,
                    listen_type: ListenType::Fifo,
                }),
        )
        .chain(
            view.strings("LISTENNETLINK")
                .into_iter()
                .map(|address| Listener {
                    address,
                    listen_type: ListenType::Netlink,
                }),
        )
        .collect();
}

fn apply_socket_fields(socket: &mut SocketSection, view: &SectionView<'_>) {
    socket.accept = view.first_bool("ACCEPT").unwrap_or(socket.accept);
    socket.service = view.first_string("SERVICE");
    socket.socket_mode = view.first_parsed("SOCKETMODE", parse_octal);
    socket.socket_user = view.first_string("SOCKETUSER");
    socket.socket_group = view.first_string("SOCKETGROUP");
    socket.fd_name = view.first_string("FILEDESCRIPTORNAME");
    socket.remove_on_stop = view
        .first_bool("REMOVEONSTOP")
        .unwrap_or(socket.remove_on_stop);
    socket.max_connections_per_source =
        view.first_parsed("MAXCONNECTIONSPERSOURCE", |raw| raw.parse().ok());
    socket.receive_buffer = view.first_parsed("RECEIVEBUFFER", parse_memory);
    socket.send_buffer = view.first_parsed("SENDBUFFER", parse_memory);
    socket.pass_credentials = view
        .first_bool("PASSCREDENTIALS")
        .unwrap_or(socket.pass_credentials);
    socket.pass_security = view
        .first_bool("PASSSECURITY")
        .unwrap_or(socket.pass_security);
    socket.symlinks = view.words("SYMLINKS");
    socket.defer_trigger = view
        .first_bool("DEFERTRIGGER")
        .unwrap_or(socket.defer_trigger);
}

fn apply_timer_section(timer: &mut TimerSection, view: &SectionView<'_>) {
    timer.on_calendar = view
        .strings("ONCALENDAR")
        .into_iter()
        .map(|raw| CalendarSpec::parse(&raw))
        .collect();
    timer.on_boot_sec = view.first_parsed("ONBOOTSEC", parse_duration);
    timer.on_startup_sec = view.first_parsed("ONSTARTUPSEC", parse_duration);
    timer.on_active_sec = view.first_parsed("ONACTIVESEC", parse_duration);
    timer.on_unit_active_sec = view.first_parsed("ONUNITACTIVESEC", parse_duration);
    timer.on_unit_inactive_sec = view.first_parsed("ONUNITINACTIVESEC", parse_duration);
    timer.accuracy_sec = view
        .first_parsed("ACCURACYSEC", parse_duration)
        .unwrap_or(timer.accuracy_sec);
    timer.randomized_delay_sec = view.first_parsed("RANDOMIZEDDELAYSEC", parse_duration);
    timer.persistent = view.first_bool("PERSISTENT").unwrap_or(timer.persistent);
    timer.wake_system = view.first_bool("WAKESYSTEM").unwrap_or(timer.wake_system);
    timer.on_clock_change = view
        .first_bool("ONCLOCKCHANGE")
        .unwrap_or(timer.on_clock_change);
    timer.on_timezone_change = view
        .first_bool("ONTIMEZONECHANGE")
        .unwrap_or(timer.on_timezone_change);
    timer.unit = view.first_string("UNIT");
}

pub fn parse_service(name: &str, parsed: &ParsedFile) -> Result<Service, ParseError> {
    let mut service = Service::new(name.to_string());
    let unit_view = SectionView::from(parsed, "[Unit]");
    apply_unit_core(&mut service.unit, &unit_view);
    apply_unit_conditions(&mut service.unit, &unit_view);
    apply_unit_service_extras(&mut service.unit, &unit_view);

    let service_view = SectionView::from(parsed, "[Service]");
    apply_service_exec_and_restart(&mut service.service, &service_view);
    apply_service_identity(&mut service.service, &service_view);
    apply_service_environment(&mut service.service, &service_view);
    apply_service_stdio(&mut service.service, &service_view);
    apply_service_limits(&mut service.service, &service_view);
    apply_service_security_core(&mut service.service, &service_view);
    apply_service_security_paths(&mut service.service, &service_view);
    apply_service_security_extended(&mut service.service, &service_view);
    apply_service_process_control(&mut service.service, &service_view);

    let install_view = SectionView::from(parsed, "[Install]");
    apply_install_extended(&mut service.install, &install_view);
    Ok(service)
}

pub fn parse_target(name: &str, parsed: &ParsedFile) -> Result<Target, ParseError> {
    let mut target = Target::new(name.to_string());
    let unit_view = SectionView::from(parsed, "[Unit]");
    apply_unit_core(&mut target.unit, &unit_view);
    apply_unit_conditions(&mut target.unit, &unit_view);
    Ok(target)
}

pub fn parse_path_unit(name: &str, parsed: &ParsedFile) -> Result<path::Path, ParseError> {
    let mut path_unit = path::Path::new(name.to_string());
    let unit_view = SectionView::from(parsed, "[Unit]");
    apply_unit_core(&mut path_unit.unit, &unit_view);

    let path_view = SectionView::from(parsed, "[Path]");
    path_unit.path.path_exists = path_view.strings("PATHEXISTS");
    path_unit.path.path_exists_glob = path_view.strings("PATHEXISTSGLOB");
    path_unit.path.path_changed = path_view.strings("PATHCHANGED");
    path_unit.path.path_modified = path_view.strings("PATHMODIFIED");
    path_unit.path.directory_not_empty = path_view.strings("DIRECTORYNOTEMPTY");
    path_unit.path.unit = path_view.first_string("UNIT");
    path_unit.path.make_directory = path_view
        .first_bool("MAKEDIRECTORY")
        .unwrap_or(path_unit.path.make_directory);
    path_unit.path.directory_mode = path_view.first_parsed("DIRECTORYMODE", parse_octal);

    let install_view = SectionView::from(parsed, "[Install]");
    apply_install_without_default_instance(&mut path_unit.install, &install_view);
    Ok(path_unit)
}

pub fn parse_slice(name: &str, parsed: &ParsedFile) -> Result<Slice, ParseError> {
    let mut slice = Slice::new(name.to_string());
    let unit_view = SectionView::from(parsed, "[Unit]");
    apply_unit_core(&mut slice.unit, &unit_view);
    apply_unit_conditions(&mut slice.unit, &unit_view);
    Ok(slice)
}

pub fn parse_mount(name: &str, parsed: &ParsedFile) -> Result<Mount, ParseError> {
    let mut mount = Mount::new(name.to_string());
    let unit_view = SectionView::from(parsed, "[Unit]");
    apply_unit_core(&mut mount.unit, &unit_view);
    apply_unit_conditions(&mut mount.unit, &unit_view);

    let mount_view = SectionView::from(parsed, "[Mount]");
    apply_mount_section(&mut mount.mount, &mount_view);
    if mount.mount.r#where.is_empty() {
        mount.mount.r#where = Mount::mount_point_from_name(name);
    }

    let install_view = SectionView::from(parsed, "[Install]");
    apply_install_core(&mut mount.install, &install_view);
    Ok(mount)
}

pub fn parse_socket(name: &str, parsed: &ParsedFile) -> Result<Socket, ParseError> {
    let mut socket = Socket::new(name.to_string());
    let unit_view = SectionView::from(parsed, "[Unit]");
    apply_unit_core(&mut socket.unit, &unit_view);
    apply_unit_conditions(&mut socket.unit, &unit_view);

    let socket_view = SectionView::from(parsed, "[Socket]");
    apply_socket_listeners(&mut socket.socket, &socket_view);
    apply_socket_fields(&mut socket.socket, &socket_view);

    let install_view = SectionView::from(parsed, "[Install]");
    apply_install_extended(&mut socket.install, &install_view);
    Ok(socket)
}

pub fn parse_timer(name: &str, parsed: &ParsedFile) -> Result<Timer, ParseError> {
    let mut timer = Timer::new(name.to_string());
    let unit_view = SectionView::from(parsed, "[Unit]");
    apply_unit_core(&mut timer.unit, &unit_view);
    apply_unit_conditions(&mut timer.unit, &unit_view);

    let timer_view = SectionView::from(parsed, "[Timer]");
    apply_timer_section(&mut timer.timer, &timer_view);

    let install_view = SectionView::from(parsed, "[Install]");
    apply_install_extended(&mut timer.install, &install_view);
    Ok(timer)
}

fn fallback_unit_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn resolve_service_name(path: &Path) -> String {
    if !path.is_symlink() {
        return fallback_unit_name(path);
    }

    std::fs::read_link(path)
        .ok()
        .and_then(|target| target.file_name().map(|name| name.to_os_string()))
        .and_then(|name| name.into_string().ok())
        .unwrap_or_else(|| fallback_unit_name(path))
}

fn dropin_directories(unit_path: &Path) -> Vec<PathBuf> {
    let Some(unit_name) = unit_path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };

    let mut directories = vec![
        Path::new("/etc/systemd/system").join(format!("{}.d", unit_name)),
        Path::new("/usr/lib/systemd/system").join(format!("{}.d", unit_name)),
    ];

    if let Some(parent) = unit_path.parent() {
        directories.push(parent.join(format!("{}.d", unit_name)));
    }

    directories
}

fn collect_dropin_files(directories: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();

    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "conf") {
                files.push(path);
            }
        }
    }

    files.sort();
    files
}

async fn load_dropins(unit_path: &Path, parsed: &mut ParsedFile) {
    let directories = dropin_directories(unit_path);
    let files = collect_dropin_files(&directories);

    for conf_path in files {
        match parse_unit_file(&conf_path).await {
            Ok(dropin) => {
                log::debug!("Loaded drop-in: {}", conf_path.display());
                merge_parsed_files(parsed, &dropin);
            }
            Err(error) => {
                log::warn!("Failed to parse drop-in {}: {}", conf_path.display(), error);
            }
        }
    }
}

fn merge_parsed_files(base: &mut ParsedFile, dropin: &ParsedFile) {
    for (section_name, section_values) in dropin {
        let base_section = base.entry(section_name.clone()).or_default();

        for (key, values) in section_values {
            let has_reset = values.iter().any(|(_, value)| value.is_empty());
            if has_reset {
                base_section.remove(key);
                let non_empty: Vec<_> = values
                    .iter()
                    .filter(|(_, value)| !value.is_empty())
                    .cloned()
                    .collect();
                if !non_empty.is_empty() {
                    base_section.insert(key.clone(), non_empty);
                }
                continue;
            }

            base_section
                .entry(key.clone())
                .or_default()
                .extend(values.clone());
        }
    }
}

async fn load_parsed_with_dropins(path: &Path) -> Result<ParsedFile, ParseError> {
    let mut parsed = parse_unit_file(path).await?;
    load_dropins(path, &mut parsed).await;
    Ok(parsed)
}

async fn load_with_parser<T>(
    path: &Path,
    name_resolver: fn(&Path) -> String,
    parser: fn(&str, &ParsedFile) -> Result<T, ParseError>,
) -> Result<T, ParseError> {
    let name = name_resolver(path);
    let parsed = load_parsed_with_dropins(path).await?;
    parser(&name, &parsed)
}

pub async fn load_service(path: &Path) -> Result<Service, ParseError> {
    load_with_parser(path, resolve_service_name, parse_service).await
}

fn read_wants_dir(path: &Path) -> Vec<String> {
    const UNIT_EXTENSIONS: [&str; 7] = [
        ".service", ".target", ".path", ".socket", ".mount", ".slice", ".timer",
    ];

    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(String::from))
        .filter(|name| {
            UNIT_EXTENSIONS
                .iter()
                .any(|extension| name.ends_with(extension))
        })
        .collect()
}

fn collect_target_wants(path: &Path, name: &str) -> Vec<String> {
    let mut wants = Vec::new();

    let local_wants_dir = path.with_extension("target.wants");
    if local_wants_dir.is_dir() {
        wants.extend(read_wants_dir(&local_wants_dir));
    }

    let etc_wants_dir = Path::new("/etc/systemd/system").join(format!("{}.wants", name));
    if etc_wants_dir.is_dir() {
        wants.extend(read_wants_dir(&etc_wants_dir));
    }

    wants
}

pub async fn load_target(path: &Path) -> Result<Target, ParseError> {
    let name = fallback_unit_name(path);
    let parsed = load_parsed_with_dropins(path).await?;
    let mut target = parse_target(&name, &parsed)?;
    target.wants_dir = collect_target_wants(path, &name);
    Ok(target)
}

pub async fn load_path(path: &Path) -> Result<path::Path, ParseError> {
    load_with_parser(path, fallback_unit_name, parse_path_unit).await
}

pub async fn load_slice(path: &Path) -> Result<Slice, ParseError> {
    load_with_parser(path, fallback_unit_name, parse_slice).await
}

pub async fn load_mount(path: &Path) -> Result<Mount, ParseError> {
    load_with_parser(path, fallback_unit_name, parse_mount).await
}

pub async fn load_socket(path: &Path) -> Result<Socket, ParseError> {
    load_with_parser(path, fallback_unit_name, parse_socket).await
}

pub async fn load_timer(path: &Path) -> Result<Timer, ParseError> {
    load_with_parser(path, fallback_unit_name, parse_timer).await
}

pub async fn load_unit(path: &Path) -> Result<Unit, ParseError> {
    let extension = path.extension().and_then(|ext| ext.to_str());

    match extension {
        Some("service") => load_service(path).await.map(Unit::Service),
        Some("target") => load_target(path).await.map(Unit::Target),
        Some("mount") => load_mount(path).await.map(Unit::Mount),
        Some("slice") => load_slice(path).await.map(Unit::Slice),
        Some("socket") => load_socket(path).await.map(Unit::Socket),
        Some("timer") => load_timer(path).await.map(Unit::Timer),
        Some("path") => load_path(path).await.map(Unit::Path),
        _ => Err(ParseError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Unknown unit type: {:?}", path),
        ))),
    }
}

#[cfg(test)]
#[path = "parse_units_all_tests.rs"]
mod tests;
