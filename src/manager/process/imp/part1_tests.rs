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
