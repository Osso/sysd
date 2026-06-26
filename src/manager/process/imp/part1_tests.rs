use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("sysd-process-{label}-{id}"));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

fn service(name: &str) -> Service {
    Service::new(name.to_string())
}

fn unique_name(prefix: &str) -> String {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    format!("SYSD_TEST_{prefix}_{id}")
}

#[cfg(unix)]
fn libc_env_var(key: &str) -> Option<String> {
    let key = std::ffi::CString::new(key).unwrap();
    let value = unsafe { libc::getenv(key.as_ptr()) };
    if value.is_null() {
        None
    } else {
        Some(
            unsafe { std::ffi::CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[test]
fn parse_command_trims_systemd_prefixes_and_preserves_quoted_args() {
    let (program, args) = parse_command("-+!/bin/echo 'hello world' plain").unwrap();

    assert_eq!(program, "/bin/echo");
    assert_eq!(args, ["hello world", "plain"]);
}

#[test]
fn parse_command_rejects_empty_and_unbalanced_commands() {
    assert!(matches!(
        parse_command("-!"),
        Err(SpawnError::InvalidCommand(_))
    ));
    assert!(matches!(
        parse_command("/bin/echo 'unterminated"),
        Err(SpawnError::InvalidCommand(_))
    ));
}

#[test]
fn substitute_specifiers_expands_template_and_literal_percent_values() {
    let templated = service("worker@blue.service");
    let expanded = substitute_specifiers("%n %N %p %P %i %I %%", &templated);

    assert_eq!(
        expanded,
        "worker@blue.service worker@blue worker worker blue blue %"
    );

    let plain = service("plain.service");
    assert_eq!(substitute_specifiers("%N/%p/%i/%I", &plain), "plain/plain//");

    let bare = service("oneshot");
    assert_eq!(substitute_specifiers("%N/%p", &bare), "oneshot/oneshot");
}

#[test]
fn socket_activation_appends_stored_fds_and_default_names() {
    let options = SpawnOptions {
        socket_fds: vec![10, 11],
        socket_fd_names: vec!["api".to_string()],
        stored_fds: vec![12, 13],
        ..Default::default()
    };

    let activation = build_socket_activation(&options);

    assert_eq!(activation.fds, [10, 11, 12, 13]);
    assert_eq!(activation.names, ["api", "stored", "stored", "unknown"]);
}

#[test]
fn service_environment_merges_direct_files_and_notify_settings() {
    let root = temp_dir("env");
    let env_file = root.0.join("service.env");
    std::fs::write(
        &env_file,
        "\n# comment\nFROM_FILE=file\nQUOTED=\"quoted value\"\nSINGLE='single value'\n",
    )
    .unwrap();

    let mut service = service("env.service");
    service.service.environment = vec![
        ("DIRECT".to_string(), "direct".to_string()),
        ("FROM_FILE".to_string(), "direct-before-file".to_string()),
    ];
    service.service.environment_file = vec![env_file];
    let options = SpawnOptions {
        notify_socket: Some("/run/sysd/notify.sock".to_string()),
        watchdog_usec: Some(5_000_000),
        ..Default::default()
    };

    let env = build_service_environment(&service, &options);

    assert_eq!(env.get("DIRECT").map(String::as_str), Some("direct"));
    assert_eq!(env.get("FROM_FILE").map(String::as_str), Some("file"));
    assert_eq!(env.get("QUOTED").map(String::as_str), Some("quoted value"));
    assert_eq!(env.get("SINGLE").map(String::as_str), Some("single value"));
    assert_eq!(
        env.get("NOTIFY_SOCKET").map(String::as_str),
        Some("/run/sysd/notify.sock")
    );
    assert_eq!(env.get("WATCHDOG_USEC").map(String::as_str), Some("5000000"));
}

#[test]
fn load_env_file_skips_comments_and_malformed_lines() {
    let root = temp_dir("load-env");
    let env_file = root.0.join("service.env");
    std::fs::write(
        &env_file,
        "\n# comment\nKEY=value\nNO_EQUALS\nEMPTY=\nSPACED = spaced value \n",
    )
    .unwrap();

    let env = load_env_file(&env_file).unwrap();

    assert_eq!(env.get("KEY").map(String::as_str), Some("value"));
    assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
    assert_eq!(env.get("SPACED ").map(String::as_str), Some(" spaced value"));
    assert!(!env.contains_key("NO_EQUALS"));
    assert!(load_env_file(&root.0.join("missing.env")).is_err());
}

#[test]
fn resolve_uid_gid_prefers_dynamic_ids_over_service_user_group() {
    let mut service = service("identity.service");
    service.service.user = Some("0".to_string());
    service.service.group = Some("0".to_string());
    let options = SpawnOptions {
        dynamic_uid: Some(1234),
        dynamic_gid: Some(5678),
        ..Default::default()
    };

    assert_eq!(resolve_uid_gid(&service, &options), (Some(1234), Some(5678)));
}

#[test]
fn resolve_uid_gid_reads_numeric_service_user_and_group() {
    let mut service = service("identity.service");
    service.service.user = Some("0".to_string());
    service.service.group = Some("0".to_string());

    assert_eq!(
        resolve_uid_gid(&service, &SpawnOptions::default()),
        (Some(0), Some(0))
    );
}

#[test]
fn spawn_service_reports_missing_exec_and_spawn_failures() {
    assert!(matches!(
        spawn_service_with_options(&service("missing.service"), &SpawnOptions::default()),
        Err(SpawnError::NoExecStart(name)) if name == "missing.service"
    ));

    let mut missing_binary = service("bad-spawn.service");
    missing_binary.service.exec_start =
        vec!["/definitely/not/a/sysd-test-binary".to_string()];

    assert!(matches!(
        spawn_service_with_options(&missing_binary, &SpawnOptions::default()),
        Err(SpawnError::Spawn(message)) if message.contains("No such file")
            || message.contains("os error 2")
    ));
}

#[tokio::test]
async fn spawn_service_applies_working_directory_environment_and_unset_rules() {
    let root = temp_dir("spawn-env");
    let output = root.0.join("env.out");
    let remove_key = unique_name("REMOVE");
    let user_key = unique_name("USER");
    let direct_key = unique_name("DIRECT");
    unsafe {
        std::env::set_var(&remove_key, "parent");
    }

    let mut svc = service("env-spawn.service");
    svc.service.working_directory = Some(root.0.clone());
    svc.service.exec_start = vec![format!(
        "/bin/sh -c 'printf \"%s|%s|%s|%s\" \"$PWD\" \"${{{direct_key}}}\" \"${{{user_key}}}\" \"${{{remove_key}-unset}}\" > env.out'"
    )];
    svc.service
        .environment
        .push((direct_key.clone(), "unit".to_string()));
    svc.service.unset_environment.push(remove_key.clone());
    let mut user_environment = std::collections::HashMap::new();
    user_environment.insert(user_key.clone(), "session".to_string());

    let mut child = spawn_service_with_options(
        &svc,
        &SpawnOptions {
            user_environment,
            ..Default::default()
        },
    )
    .unwrap();

    let status = child.wait().await.unwrap();
    unsafe {
        std::env::remove_var(&remove_key);
    }

    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        format!("{}|unit|session|unset", root.0.display())
    );
}

#[test]
fn environment_helpers_apply_valid_names_and_ignore_invalid_cstrings() {
    let keep_key = unique_name("KEEP");
    let drop_key = unique_name("DROP");
    unsafe {
        std::env::remove_var(&keep_key);
        std::env::remove_var(&drop_key);
    }

    set_env_var(&keep_key, "one");
    set_env_var("BAD\0KEY", "ignored");
    assert_eq!(libc_env_var(&keep_key).unwrap(), "one");

    let mut extra = std::collections::HashMap::new();
    extra.insert(keep_key.clone(), "two".to_string());
    extra.insert("BAD\0KEY".to_string(), "ignored".to_string());
    set_env_var(&drop_key, "remove-me");
    set_environment_from_maps(&extra, &[drop_key.clone(), "BAD\0KEY".to_string()]);

    assert_eq!(libc_env_var(&keep_key).unwrap(), "two");
    assert!(libc_env_var(&drop_key).is_none());
    unset_env_var(&keep_key);
    assert!(libc_env_var(&keep_key).is_none());
}

#[test]
fn socket_fd_helpers_report_invalid_fds_and_clear_cloexec_on_valid_fds() {
    use std::os::fd::AsRawFd;

    validate_socket_fds(&[-1]);
    assert!(map_socket_fds(&[-1]).is_err());

    let file = std::fs::File::open("/dev/null").unwrap();
    let fd = file.as_raw_fd();
    validate_socket_fds(&[fd]);
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    clear_cloexec(fd);

    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert_eq!(flags & libc::FD_CLOEXEC, 0);
}

#[test]
fn no_op_process_settings_and_missing_identities_are_safe() {
    apply_resource_limits(None, None, None);
    set_single_limit(-1, Some(1), "INVALID_RESOURCE");
    apply_oom_score_adjust(None);
    apply_sandbox(&crate::units::ServiceSection::default());
    assert!(drop_privileges(None, None).is_ok());

    assert_eq!(resolve_user("42"), Some(42));
    assert_eq!(resolve_group("43"), Some(43));
    assert_eq!(resolve_user("missing\0user"), None);
    assert_eq!(resolve_group("missing\0group"), None);
    assert_eq!(resolve_user("definitely-missing-sysd-user"), None);
    assert_eq!(resolve_group("definitely-missing-sysd-group"), None);
}

#[test]
fn systemd_socket_env_records_count_pid_and_names() {
    let original_fds = std::env::var("LISTEN_FDS").ok();
    let original_pid = std::env::var("LISTEN_PID").ok();
    let original_names = std::env::var("LISTEN_FDNAMES").ok();

    set_systemd_socket_env(3, &["api".to_string(), "stored".to_string()]);

    assert_eq!(libc_env_var("LISTEN_FDS").as_deref(), Some("3"));
    assert_eq!(
        libc_env_var("LISTEN_PID"),
        Some(std::process::id().to_string())
    );
    assert_eq!(libc_env_var("LISTEN_FDNAMES").as_deref(), Some("api:stored"));

    restore_env_var("LISTEN_FDS", original_fds);
    restore_env_var("LISTEN_PID", original_pid);
    restore_env_var("LISTEN_FDNAMES", original_names);
}

#[test]
fn directory_helpers_create_default_and_named_paths_with_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_dir("dirs");
    ensure_directory_set(
        root.0.to_str().unwrap(),
        &["".to_string(), "explicit".to_string()],
        "fallback",
        None,
        None,
        "state",
    );

    let fallback = root.0.join("fallback");
    let explicit = root.0.join("explicit");

    assert!(fallback.is_dir());
    assert!(explicit.is_dir());
    assert_eq!(
        std::fs::metadata(fallback).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::metadata(explicit).unwrap().permissions().mode() & 0o777,
        0o755
    );

    let blocking_file = root.0.join("not-a-dir");
    std::fs::write(&blocking_file, b"blocking").unwrap();
    ensure_directory_set(
        blocking_file.to_str().unwrap(),
        &["child".to_string()],
        "fallback",
        None,
        None,
        "state",
    );
}

#[test]
fn tty_setup_ignores_non_tty_and_reports_tty_fail_open_errors() {
    let missing = std::env::temp_dir().join(unique_name("missing-tty"));

    assert!(setup_tty(&StdInput::Null, Some(&missing), true).is_ok());
    assert!(setup_tty(&StdInput::Tty, Some(&missing), false).is_ok());
    assert!(setup_tty(&StdInput::TtyFail, Some(&missing), false).is_err());
    assert!(setup_tty(&StdInput::TtyForce, None, true).is_ok());
}

#[test]
fn executor_config_maps_stdio_and_sandbox_enum_variants() {
    let mut service = service("sandboxed.service");
    service.service.exec_start = vec!["/bin/true --flag".to_string()];
    service.service.standard_input = StdInput::TtyFail;
    service.service.protect_system = crate::units::ProtectSystem::Strict;
    service.service.protect_home = crate::units::ProtectHome::Tmpfs;
    service.service.protect_proc = crate::units::ProtectProc::NoAccess;
    service.service.device_policy = crate::units::DevicePolicy::Strict;

    let config = build_exec_config(&service, &SpawnOptions::default(), 0).unwrap();

    assert_eq!(config.std_input, StdInputConfig::TtyFail);
    assert_eq!(config.sandbox.protect_system, ProtectSystemConfig::Strict);
    assert_eq!(config.sandbox.protect_home, ProtectHomeConfig::Tmpfs);
    assert_eq!(config.sandbox.protect_proc, ProtectProcConfig::NoAccess);
    assert_eq!(config.sandbox.device_policy, DevicePolicyConfig::Strict);

    service.service.standard_input = StdInput::Tty;
    service.service.protect_system = crate::units::ProtectSystem::Yes;
    service.service.protect_home = crate::units::ProtectHome::Yes;
    service.service.protect_proc = crate::units::ProtectProc::Invisible;
    service.service.device_policy = crate::units::DevicePolicy::Closed;
    let config = build_exec_config(&service, &SpawnOptions::default(), 0).unwrap();
    assert_eq!(config.std_input, StdInputConfig::Tty);
    assert_eq!(config.sandbox.protect_system, ProtectSystemConfig::Yes);
    assert_eq!(config.sandbox.protect_home, ProtectHomeConfig::Yes);
    assert_eq!(config.sandbox.protect_proc, ProtectProcConfig::Invisible);
    assert_eq!(config.sandbox.device_policy, DevicePolicyConfig::Closed);

    service.service.standard_input = StdInput::TtyForce;
    service.service.protect_system = crate::units::ProtectSystem::Full;
    service.service.protect_home = crate::units::ProtectHome::ReadOnly;
    service.service.protect_proc = crate::units::ProtectProc::Ptraceable;
    let config = build_exec_config(&service, &SpawnOptions::default(), 0).unwrap();
    assert_eq!(config.std_input, StdInputConfig::TtyForce);
    assert_eq!(config.sandbox.protect_system, ProtectSystemConfig::Full);
    assert_eq!(config.sandbox.protect_home, ProtectHomeConfig::ReadOnly);
    assert_eq!(config.sandbox.protect_proc, ProtectProcConfig::Ptraceable);
}

fn restore_env_var(key: &str, value: Option<String>) {
    unsafe {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
