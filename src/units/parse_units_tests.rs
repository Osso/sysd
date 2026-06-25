use super::*;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn parsed(content: &str) -> ParsedFile {
    parse_file(content).expect("unit file should parse")
}

fn temp_unit_dir(test_name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "sysd-parse-units-{}-{}-{}",
        test_name,
        std::process::id(),
        nonce
    ));
    fs::create_dir(&path).expect("temp unit directory should be created");
    path
}

const SERVICE_UNIT_FIXTURE: &str = r#"
[Unit]
Description=Demo worker
After=network-online.target remote-fs.target
Requires=network-online.target
Wants=metrics.target audit.target
BindsTo=dbus.socket
ConditionPathExists=/etc/demo.conf
ConditionDirectoryNotEmpty=/var/lib/demo
ConditionVirtualization=!container
ConditionCapability=CAP_NET_BIND_SERVICE
ConditionKernelCommandLine=demo.enabled=1
ConditionSecurity=apparmor
ConditionFirstBoot=no
ConditionNeedsUpdate=/etc
DefaultDependencies=no
IgnoreOnIsolate=yes

[Service]
Type=notify-reload
ExecStartPre=/usr/bin/install -d /run/demo
ExecStart=/usr/bin/demo --foreground
ExecStartPost=/usr/bin/demo-ready
ExecReload=/bin/kill -HUP $MAINPID
ExecStop=/usr/bin/demo-stop
Restart=on-failure
RestartSec=5s
TimeoutStartSec=30s
TimeoutStopSec=45s
RemainAfterExit=yes
WatchdogSec=20s
NotifyAccess=all
PIDFile=/run/demo.pid
BusName=com.example.Demo
KillMode=mixed
User=demo
Group=demo
WorkingDirectory=/var/lib/demo
Environment=MODE=prod "GREETING=hello world"
EnvironmentFile=/etc/demo.env
UnsetEnvironment=DEBUG
StandardOutput=null
StandardError=inherit
StandardInput=tty-force
TTYPath=/dev/tty1
TTYReset=yes
MemoryMax=128M
CPUQuota=250%
TasksMax=64
LimitNOFILE=infinity
LimitNPROC=512
LimitCORE=0
StateDirectory=demo state2
RuntimeDirectory=demo
ConfigurationDirectory=demo
LogsDirectory=demo
CacheDirectory=demo
RuntimeDirectoryPreserve=restart
DynamicUser=yes
OOMScoreAdjust=-100
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=yes
PrivateDevices=yes
PrivateNetwork=yes
ProtectKernelModules=yes
ProtectProc=invisible
CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_CHOWN
AmbientCapabilities=CAP_NET_BIND_SERVICE
RestrictNamespaces=~user pid
ReadWritePaths=/var/lib/demo /run/demo
ReadOnlyPaths=/etc/demo
InaccessiblePaths=/home
SystemCallFilter=@system-service
DevicePolicy=closed
DeviceAllow=/dev/null rw
RestrictRealtime=yes
ProtectControlGroups=yes
MemoryDenyWriteExecute=yes
LockPersonality=yes
ProtectKernelTunables=yes
ProtectKernelLogs=yes
ProtectClock=yes
ProtectHostname=yes
IgnoreSIGPIPE=no
RestrictSUIDSGID=yes
RestrictAddressFamilies=AF_UNIX AF_INET
SystemCallErrorNumber=13
SystemCallArchitectures=native
StartLimitBurst=3
StartLimitIntervalSec=1min
Sockets=demo.socket
SendSIGHUP=yes
Slice=system-demo.slice
Delegate=yes
ExecStopPost=/usr/bin/demo-cleanup
FileDescriptorStoreMax=8
RestartPreventExitStatus=64 65

[Install]
WantedBy=multi-user.target
RequiredBy=graphical.target
Also=demo.socket
Alias=demo-alias.service
DefaultInstance=main
"#;

#[test]
fn parse_service_maps_unit_service_and_install_sections() {
    let unit = parsed(SERVICE_UNIT_FIXTURE);

    let service = parse_service("demo@main.service", &unit).expect("service should parse");

    assert_eq!(service.name, "demo@main.service");
    assert_eq!(service.instance.as_deref(), Some("main"));
    assert_eq!(service.unit.description.as_deref(), Some("Demo worker"));
    assert_eq!(
        service.unit.after,
        ["network-online.target", "remote-fs.target"]
    );
    assert_eq!(service.unit.requires, ["network-online.target"]);
    assert_eq!(service.unit.wants, ["metrics.target", "audit.target"]);
    assert_eq!(service.unit.binds_to, ["dbus.socket"]);
    assert_eq!(service.unit.condition_path_exists, ["/etc/demo.conf"]);
    assert_eq!(
        service.unit.condition_directory_not_empty,
        ["/var/lib/demo"]
    );
    assert_eq!(service.unit.condition_virtualization, ["!container"]);
    assert_eq!(service.unit.condition_capability, ["CAP_NET_BIND_SERVICE"]);
    assert_eq!(
        service.unit.condition_kernel_command_line,
        ["demo.enabled=1"]
    );
    assert_eq!(service.unit.condition_security, ["apparmor"]);
    assert_eq!(service.unit.condition_first_boot, Some(false));
    assert_eq!(service.unit.condition_needs_update, ["/etc"]);
    assert!(!service.unit.default_dependencies);
    assert!(service.unit.ignore_on_isolate);

    assert_eq!(service.service.service_type, ServiceType::Notify);
    assert_eq!(
        service.service.exec_start_pre,
        ["/usr/bin/install -d /run/demo"]
    );
    assert_eq!(service.service.exec_start, ["/usr/bin/demo --foreground"]);
    assert_eq!(service.service.exec_start_post, ["/usr/bin/demo-ready"]);
    assert_eq!(service.service.exec_reload, ["/bin/kill -HUP $MAINPID"]);
    assert_eq!(service.service.exec_stop, ["/usr/bin/demo-stop"]);
    assert_eq!(service.service.restart, RestartPolicy::OnFailure);
    assert_eq!(service.service.restart_sec, Duration::from_secs(5));
    assert_eq!(
        service.service.timeout_start_sec,
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        service.service.timeout_stop_sec,
        Some(Duration::from_secs(45))
    );
    assert!(service.service.remain_after_exit);
    assert_eq!(service.service.watchdog_sec, Some(Duration::from_secs(20)));
    assert_eq!(service.service.notify_access, NotifyAccess::All);
    assert_eq!(
        service.service.pid_file.as_deref(),
        Some(Path::new("/run/demo.pid"))
    );
    assert_eq!(
        service.service.bus_name.as_deref(),
        Some("com.example.Demo")
    );
    assert_eq!(service.service.kill_mode, KillMode::Mixed);
    assert_eq!(service.service.user.as_deref(), Some("demo"));
    assert_eq!(service.service.group.as_deref(), Some("demo"));
    assert_eq!(
        service.service.working_directory.as_deref(),
        Some(Path::new("/var/lib/demo"))
    );
    assert_eq!(
        service.service.environment,
        [
            ("MODE".to_string(), "prod".to_string()),
            ("GREETING".to_string(), "hello world".to_string())
        ]
    );
    assert_eq!(
        service.service.environment_file,
        [PathBuf::from("/etc/demo.env")]
    );
    assert_eq!(service.service.unset_environment, ["DEBUG"]);
    assert_eq!(service.service.standard_output, StdOutput::Null);
    assert_eq!(service.service.standard_error, StdOutput::Inherit);
    assert_eq!(service.service.standard_input, StdInput::TtyForce);
    assert_eq!(
        service.service.tty_path.as_deref(),
        Some(Path::new("/dev/tty1"))
    );
    assert!(service.service.tty_reset);
    assert_eq!(service.service.memory_max, Some(128 * 1024 * 1024));
    assert_eq!(service.service.cpu_quota, Some(250));
    assert_eq!(service.service.tasks_max, Some(64));
    assert_eq!(service.service.limit_nofile, Some(u64::MAX));
    assert_eq!(service.service.limit_nproc, Some(512));
    assert_eq!(service.service.limit_core, Some(0));
    assert_eq!(service.service.state_directory, ["demo", "state2"]);
    assert_eq!(service.service.runtime_directory, ["demo"]);
    assert_eq!(service.service.configuration_directory, ["demo"]);
    assert_eq!(service.service.logs_directory, ["demo"]);
    assert_eq!(service.service.cache_directory, ["demo"]);
    assert_eq!(
        service.service.runtime_directory_preserve,
        RuntimeDirectoryPreserve::Restart
    );
    assert!(service.service.dynamic_user);
    assert_eq!(service.service.oom_score_adjust, Some(-100));
    assert!(service.service.no_new_privileges);
    assert_eq!(service.service.protect_system, ProtectSystem::Strict);
    assert_eq!(service.service.protect_home, ProtectHome::ReadOnly);
    assert!(service.service.private_tmp);
    assert!(service.service.private_devices);
    assert!(service.service.private_network);
    assert!(service.service.protect_kernel_modules);
    assert_eq!(service.service.protect_proc, ProtectProc::Invisible);
    assert_eq!(
        service.service.capability_bounding_set,
        ["CAP_NET_BIND_SERVICE", "CAP_CHOWN"]
    );
    assert_eq!(
        service.service.ambient_capabilities,
        ["CAP_NET_BIND_SERVICE"]
    );
    assert_eq!(
        service.service.restrict_namespaces,
        Some(vec!["~user".to_string(), "pid".to_string()])
    );
    assert_eq!(
        service.service.read_write_paths,
        [PathBuf::from("/var/lib/demo"), PathBuf::from("/run/demo")]
    );
    assert_eq!(
        service.service.read_only_paths,
        [PathBuf::from("/etc/demo")]
    );
    assert_eq!(service.service.inaccessible_paths, [PathBuf::from("/home")]);
    assert_eq!(service.service.system_call_filter, ["@system-service"]);
    assert_eq!(service.service.device_policy, DevicePolicy::Closed);
    assert_eq!(service.service.device_allow, ["/dev/null rw"]);
    assert!(service.service.restrict_realtime);
    assert!(service.service.protect_control_groups);
    assert!(service.service.memory_deny_write_execute);
    assert!(service.service.lock_personality);
    assert!(service.service.protect_kernel_tunables);
    assert!(service.service.protect_kernel_logs);
    assert!(service.service.protect_clock);
    assert!(service.service.protect_hostname);
    assert!(!service.service.ignore_sigpipe);
    assert!(service.service.restrict_suid_sgid);
    assert_eq!(
        service.service.restrict_address_families,
        Some(vec!["AF_UNIX".to_string(), "AF_INET".to_string()])
    );
    assert_eq!(service.service.system_call_error_number, Some(13));
    assert_eq!(service.service.system_call_architectures, ["native"]);
    assert_eq!(service.service.start_limit_burst, Some(3));
    assert_eq!(
        service.service.start_limit_interval_sec,
        Some(Duration::from_secs(60))
    );
    assert_eq!(service.service.sockets, ["demo.socket"]);
    assert!(service.service.send_sighup);
    assert_eq!(service.service.slice.as_deref(), Some("system-demo.slice"));
    assert!(service.service.delegate);
    assert_eq!(service.service.exec_stop_post, ["/usr/bin/demo-cleanup"]);
    assert_eq!(service.service.file_descriptor_store_max, Some(8));
    assert_eq!(service.service.restart_prevent_exit_status, [64, 65]);
    assert_eq!(service.install.wanted_by, ["multi-user.target"]);
    assert_eq!(service.install.required_by, ["graphical.target"]);
    assert_eq!(service.install.also, ["demo.socket"]);
    assert_eq!(service.install.alias, ["demo-alias.service"]);
    assert_eq!(service.install.default_instance.as_deref(), Some("main"));
}

#[test]
fn parse_target_and_slice_map_unit_metadata() {
    let unit = parsed(
        r#"
[Unit]
Description=Application stack
After=network.target
Before=shutdown.target
Conflicts=rescue.target
DefaultDependencies=no
ConditionFirstBoot=yes
"#,
    );

    let target = parse_target("app.target", &unit).expect("target should parse");
    assert_eq!(target.name, "app.target");
    assert_eq!(
        target.unit.description.as_deref(),
        Some("Application stack")
    );
    assert_eq!(target.unit.after, ["network.target"]);
    assert_eq!(target.unit.before, ["shutdown.target"]);
    assert_eq!(target.unit.conflicts, ["rescue.target"]);
    assert_eq!(target.unit.condition_first_boot, Some(true));
    assert!(!target.unit.default_dependencies);
    assert!(target.wants_dir.is_empty());

    let slice = parse_slice("system-app.slice", &unit).expect("slice should parse");
    assert_eq!(slice.name, "system-app.slice");
    assert_eq!(slice.unit.description.as_deref(), Some("Application stack"));
    assert_eq!(slice.unit.after, ["network.target"]);
    assert_eq!(slice.unit.before, ["shutdown.target"]);
    assert_eq!(slice.unit.conflicts, ["rescue.target"]);
    assert_eq!(slice.unit.condition_first_boot, Some(true));
    assert!(!slice.unit.default_dependencies);
}

#[test]
fn parse_path_unit_maps_watchers_and_install_data() {
    let unit = parsed(
        r#"
[Unit]
Description=Watch incoming files

[Path]
PathExists=/srv/inbox
PathExistsGlob=/srv/inbox/*.ready
PathChanged=/srv/inbox/current
PathModified=/srv/inbox/state
DirectoryNotEmpty=/srv/inbox/pending
Unit=ingest.service
MakeDirectory=yes
DirectoryMode=0750

[Install]
WantedBy=multi-user.target
RequiredBy=paths.target
Also=ingest.service
Alias=inbox-watch.path
DefaultInstance=ignored
"#,
    );

    let path_unit = parse_path_unit("ingest.path", &unit).expect("path unit should parse");
    assert_eq!(path_unit.name, "ingest.path");
    assert_eq!(
        path_unit.unit.description.as_deref(),
        Some("Watch incoming files")
    );
    assert_eq!(path_unit.path.path_exists, ["/srv/inbox"]);
    assert_eq!(path_unit.path.path_exists_glob, ["/srv/inbox/*.ready"]);
    assert_eq!(path_unit.path.path_changed, ["/srv/inbox/current"]);
    assert_eq!(path_unit.path.path_modified, ["/srv/inbox/state"]);
    assert_eq!(path_unit.path.directory_not_empty, ["/srv/inbox/pending"]);
    assert_eq!(path_unit.path.unit.as_deref(), Some("ingest.service"));
    assert!(path_unit.path.make_directory);
    assert_eq!(path_unit.path.directory_mode, Some(0o750));
    assert_eq!(path_unit.install.wanted_by, ["multi-user.target"]);
    assert_eq!(path_unit.install.required_by, ["paths.target"]);
    assert_eq!(path_unit.install.also, ["ingest.service"]);
    assert_eq!(path_unit.install.alias, ["inbox-watch.path"]);
    assert_eq!(path_unit.install.default_instance, None);
    assert_eq!(path_unit.activated_unit(), "ingest.service");
    assert!(path_unit.has_watches());
}

#[test]
fn parse_mount_maps_options_and_falls_back_to_name_mount_point() {
    let full = parsed(
        r#"
[Unit]
Description=Cache mount
Requires=local-fs-pre.target

[Mount]
What=tmpfs
Where=/var/cache/demo
Type=tmpfs
Options=mode=0755,size=64M
SloppyOptions=yes
LazyUnmount=yes
ForceUnmount=yes
ReadWriteOnly=yes
DirectoryMode=0700
TimeoutSec=15s

[Install]
WantedBy=local-fs.target
RequiredBy=multi-user.target
"#,
    );

    let mount = parse_mount("var-cache-demo.mount", &full).expect("mount should parse");
    assert_eq!(mount.name, "var-cache-demo.mount");
    assert_eq!(mount.unit.description.as_deref(), Some("Cache mount"));
    assert_eq!(mount.unit.requires, ["local-fs-pre.target"]);
    assert_eq!(mount.mount.what, "tmpfs");
    assert_eq!(mount.mount.r#where, "/var/cache/demo");
    assert_eq!(mount.mount.fs_type.as_deref(), Some("tmpfs"));
    assert_eq!(mount.mount.options.as_deref(), Some("mode=0755,size=64M"));
    assert!(mount.mount.sloppy_options);
    assert!(mount.mount.lazy_unmount);
    assert!(mount.mount.force_unmount);
    assert!(mount.mount.read_write_only);
    assert_eq!(mount.mount.directory_mode, Some(0o700));
    assert_eq!(mount.mount.timeout_sec, Some(Duration::from_secs(15)));
    assert_eq!(mount.install.wanted_by, ["local-fs.target"]);
    assert_eq!(mount.install.required_by, ["multi-user.target"]);

    let fallback = parsed(
        r#"
[Mount]
What=/dev/sdb1
"#,
    );
    let mount = parse_mount("mnt-data.mount", &fallback).expect("mount should parse");
    assert_eq!(mount.mount.r#where, "/mnt/data");
}

#[test]
fn parse_socket_maps_listeners_fields_and_install_data() {
    let unit = parsed(
        r#"
[Unit]
Description=Demo socket
After=network.target

[Socket]
ListenStream=127.0.0.1:8080
ListenDatagram=/run/demo.dgram
ListenFIFO=/run/demo.fifo
ListenNetlink=audit 1
Accept=yes
Service=demo@.service
SocketMode=0660
SocketUser=demo
SocketGroup=demo
FileDescriptorName=api
RemoveOnStop=yes
MaxConnectionsPerSource=12
ReceiveBuffer=64K
SendBuffer=128K
PassCredentials=yes
PassSecurity=yes
Symlinks=/run/demo.sock /run/demo-api.sock
DeferTrigger=yes

[Install]
WantedBy=sockets.target
RequiredBy=multi-user.target
Also=demo.service
Alias=demo-api.socket
DefaultInstance=main
"#,
    );

    let socket = parse_socket("demo.socket", &unit).expect("socket should parse");
    assert_eq!(socket.name, "demo.socket");
    assert_eq!(socket.unit.description.as_deref(), Some("Demo socket"));
    assert_eq!(socket.unit.after, ["network.target"]);
    assert_eq!(socket.socket.listeners.len(), 4);
    assert_eq!(socket.socket.listeners[0].address, "127.0.0.1:8080");
    assert_eq!(socket.socket.listeners[0].listen_type, ListenType::Stream);
    assert_eq!(socket.socket.listeners[1].address, "/run/demo.dgram");
    assert_eq!(socket.socket.listeners[1].listen_type, ListenType::Datagram);
    assert_eq!(socket.socket.listeners[2].address, "/run/demo.fifo");
    assert_eq!(socket.socket.listeners[2].listen_type, ListenType::Fifo);
    assert_eq!(socket.socket.listeners[3].address, "audit 1");
    assert_eq!(socket.socket.listeners[3].listen_type, ListenType::Netlink);
    assert!(socket.socket.accept);
    assert_eq!(socket.socket.service.as_deref(), Some("demo@.service"));
    assert_eq!(socket.socket.socket_mode, Some(0o660));
    assert_eq!(socket.socket.socket_user.as_deref(), Some("demo"));
    assert_eq!(socket.socket.socket_group.as_deref(), Some("demo"));
    assert_eq!(socket.socket.fd_name.as_deref(), Some("api"));
    assert!(socket.socket.remove_on_stop);
    assert_eq!(socket.socket.max_connections_per_source, Some(12));
    assert_eq!(socket.socket.receive_buffer, Some(64 * 1024));
    assert_eq!(socket.socket.send_buffer, Some(128 * 1024));
    assert!(socket.socket.pass_credentials);
    assert!(socket.socket.pass_security);
    assert_eq!(
        socket.socket.symlinks,
        ["/run/demo.sock", "/run/demo-api.sock"]
    );
    assert!(socket.socket.defer_trigger);
    assert_eq!(socket.service_name(), "demo@.service");
    assert!(socket.is_accept_socket());
    assert_eq!(socket.install.wanted_by, ["sockets.target"]);
    assert_eq!(socket.install.required_by, ["multi-user.target"]);
    assert_eq!(socket.install.also, ["demo.service"]);
    assert_eq!(socket.install.alias, ["demo-api.socket"]);
    assert_eq!(socket.install.default_instance.as_deref(), Some("main"));
}

#[test]
fn parse_timer_maps_calendar_monotonic_and_install_data() {
    let unit = parsed(
        r#"
[Unit]
Description=Demo timer
After=time-sync.target

[Timer]
OnCalendar=daily
OnCalendar=Mon
OnCalendar=12:34:56
OnCalendar=*-*-01 00:00:00
OnBootSec=5min
OnStartupSec=10s
OnActiveSec=2h
OnUnitActiveSec=1d
OnUnitInactiveSec=30s
AccuracySec=1s
RandomizedDelaySec=2min
Persistent=yes
WakeSystem=yes
OnClockChange=yes
OnTimezoneChange=yes
Unit=demo-refresh.service

[Install]
WantedBy=timers.target
RequiredBy=multi-user.target
Also=demo-refresh.service
Alias=demo-refresh.timer
DefaultInstance=nightly
"#,
    );

    let timer = parse_timer("demo-refresh.timer", &unit).expect("timer should parse");
    assert_eq!(timer.name, "demo-refresh.timer");
    assert_eq!(timer.unit.description.as_deref(), Some("Demo timer"));
    assert_eq!(timer.unit.after, ["time-sync.target"]);
    assert_eq!(
        timer.timer.on_calendar,
        [
            CalendarSpec::Named("daily".to_string()),
            CalendarSpec::DayOfWeek("Mon".to_string()),
            CalendarSpec::Time {
                hour: 12,
                minute: 34,
                second: 56
            },
            CalendarSpec::Full("*-*-01 00:00:00".to_string())
        ]
    );
    assert_eq!(timer.timer.on_boot_sec, Some(Duration::from_secs(5 * 60)));
    assert_eq!(timer.timer.on_startup_sec, Some(Duration::from_secs(10)));
    assert_eq!(
        timer.timer.on_active_sec,
        Some(Duration::from_secs(2 * 60 * 60))
    );
    assert_eq!(
        timer.timer.on_unit_active_sec,
        Some(Duration::from_secs(24 * 60 * 60))
    );
    assert_eq!(
        timer.timer.on_unit_inactive_sec,
        Some(Duration::from_secs(30))
    );
    assert_eq!(timer.timer.accuracy_sec, Duration::from_secs(1));
    assert_eq!(
        timer.timer.randomized_delay_sec,
        Some(Duration::from_secs(2 * 60))
    );
    assert!(timer.timer.persistent);
    assert!(timer.timer.wake_system);
    assert!(timer.timer.on_clock_change);
    assert!(timer.timer.on_timezone_change);
    assert_eq!(timer.timer.unit.as_deref(), Some("demo-refresh.service"));
    assert_eq!(timer.service_name(), "demo-refresh.service");
    assert!(timer.is_monotonic());
    assert!(timer.is_realtime());
    assert_eq!(timer.install.wanted_by, ["timers.target"]);
    assert_eq!(timer.install.required_by, ["multi-user.target"]);
    assert_eq!(timer.install.also, ["demo-refresh.service"]);
    assert_eq!(timer.install.alias, ["demo-refresh.timer"]);
    assert_eq!(timer.install.default_instance.as_deref(), Some("nightly"));
}

#[tokio::test]
async fn load_unit_loads_service_with_local_dropin_overrides() {
    let dir = temp_unit_dir("dropin");
    let unit_path = dir.join("demo.service");
    let dropin_dir = dir.join("demo.service.d");
    let dropin_path = dropin_dir.join("10-override.conf");

    fs::write(
        &unit_path,
        r#"
[Unit]
Description=Base description
After=network.target

[Service]
ExecStart=/usr/bin/demo --base
Environment=MODE=base

[Install]
WantedBy=multi-user.target
"#,
    )
    .expect("base unit should be written");
    fs::create_dir(&dropin_dir).expect("drop-in directory should be created");
    fs::write(
        &dropin_path,
        r#"
[Unit]
After=
After=dbus.service

[Service]
ExecStart=
ExecStart=/usr/bin/demo --override
Environment=
Environment=MODE=override
Environment=EXTRA=1

[Install]
WantedBy=
WantedBy=default.target
"#,
    )
    .expect("drop-in should be written");

    let unit = load_unit(&unit_path).await.expect("unit should load");
    let Unit::Service(service) = unit else {
        panic!("expected loaded service");
    };

    assert_eq!(service.name, "demo.service");
    assert_eq!(
        service.unit.description.as_deref(),
        Some("Base description")
    );
    assert_eq!(service.unit.after, ["network.target", "dbus.service"]);
    assert_eq!(service.service.exec_start, ["/usr/bin/demo --override"]);
    assert_eq!(
        service.service.environment,
        [
            ("MODE".to_string(), "override".to_string()),
            ("EXTRA".to_string(), "1".to_string())
        ]
    );
    assert_eq!(
        service.install.wanted_by,
        ["multi-user.target", "default.target"]
    );

    fs::remove_dir_all(&dir).expect("temp unit directory should be removed");
}

#[test]
fn merge_parsed_files_resets_keys_when_dropin_contains_empty_value() {
    let mut base = parsed(
        r#"
[Service]
ExecStart=/usr/bin/base
Environment=MODE=base
"#,
    );
    let dropin = parsed(
        r#"
[Service]
ExecStart=
ExecStart=/usr/bin/override
Environment=
Environment=MODE=override
"#,
    );

    merge_parsed_files(&mut base, &dropin);
    let service = parse_service("demo.service", &base).expect("service should parse");

    assert_eq!(service.service.exec_start, ["/usr/bin/override"]);
    assert_eq!(
        service.service.environment,
        [("MODE".to_string(), "override".to_string())]
    );
}

#[tokio::test]
async fn load_target_collects_local_wants_directory_units() {
    let dir = temp_unit_dir("target-wants");
    let unit_path = dir.join("demo.target");
    let wants_dir = dir.join("demo.target.wants");

    fs::write(
        &unit_path,
        r#"
[Unit]
Description=Demo target
"#,
    )
    .expect("target should be written");
    fs::create_dir(&wants_dir).expect("wants directory should be created");
    fs::write(wants_dir.join("alpha.service"), "").expect("wanted service should exist");
    fs::write(wants_dir.join("beta.timer"), "").expect("wanted timer should exist");
    fs::write(wants_dir.join("ignored.txt"), "").expect("ignored file should exist");

    let target = load_target(&unit_path).await.expect("target should load");

    assert_eq!(target.name, "demo.target");
    assert_eq!(target.unit.description.as_deref(), Some("Demo target"));
    assert_eq!(target.wants_dir, ["alpha.service", "beta.timer"]);

    fs::remove_dir_all(&dir).expect("temp target directory should be removed");
}
