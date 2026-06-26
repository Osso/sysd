use super::*;
use crate::units::{InstallSection, Service, Socket, Target, Timer, Unit};
use std::collections::HashSet;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

fn service(name: &str, configure: impl FnOnce(&mut Service)) -> Service {
    let mut service = Service::new(name.to_string());
    configure(&mut service);
    service
}

fn insert_service(manager: &mut Manager, name: &str, service: Service) {
    manager.units.insert(name.to_string(), Unit::Service(service));
    manager
        .states
        .insert(name.to_string(), ServiceState::new());
}

fn insert_target(manager: &mut Manager, name: &str) {
    manager
        .units
        .insert(name.to_string(), Unit::Target(Target::new(name.to_string())));
    manager
        .states
        .insert(name.to_string(), ServiceState::new());
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let path = std::env::temp_dir().join(format!(
        "sysd-manager-part3-{label}-{}-{}",
        std::process::id(),
        next_temp_id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

fn next_temp_id() -> u32 {
    static TEMP_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn write_service(path: &std::path::Path, exec_start: &str) {
    std::fs::write(
        path,
        format!("[Unit]\nDescription=Demo\n[Service]\nExecStart={exec_start}\n"),
    )
    .unwrap();
}

#[test]
fn accessors_normalize_service_names_and_list_loaded_units() {
    let mut manager = Manager::new();
    insert_service(&mut manager, "demo.service", service("demo.service", |_| {}));

    assert!(manager.status("demo").is_some());
    assert_eq!(manager.get_service("demo").unwrap().name, "demo.service");
    assert_eq!(manager.get_unit("demo").unwrap().name(), "demo.service");
    assert_eq!(manager.list().count(), 1);

    let listed = manager.list_units();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "demo.service");
    assert!(listed[0].2.is_some());
}

#[test]
fn mark_unit_stopping_requires_active_state() {
    let mut manager = Manager::new();
    insert_service(&mut manager, "demo.service", service("demo.service", |_| {}));

    assert!(matches!(
        manager.mark_unit_stopping("missing.service"),
        Err(ManagerError::NotFound(name)) if name == "missing.service"
    ));
    assert!(matches!(
        manager.mark_unit_stopping("demo.service"),
        Err(ManagerError::NotActive(name)) if name == "demo.service"
    ));

    manager
        .states
        .get_mut("demo.service")
        .unwrap()
        .set_running(123);
    manager.mark_unit_stopping("demo.service").unwrap();

    let state = manager.states.get("demo.service").unwrap();
    assert_eq!(state.active, ActiveState::Deactivating);
    assert_eq!(state.sub, SubState::Stopping);
}

#[test]
fn stop_signal_config_reads_service_kill_settings_or_defaults() {
    let mut manager = Manager::new();
    insert_service(
        &mut manager,
        "demo.service",
        service("demo.service", |service| {
            service.service.kill_mode = KillMode::Mixed;
            service.service.send_sighup = true;
        }),
    );

    assert_eq!(
        manager.stop_signal_config("demo.service"),
        (KillMode::Mixed, true)
    );
    assert_eq!(
        manager.stop_signal_config("missing.service"),
        (KillMode::default(), false)
    );
}

#[test]
fn cleanup_stopped_service_clears_watchdog_cgroup_and_stored_fds() {
    let mut manager = Manager::new();
    let mut pipe_fds = [0; 2];
    let pipe_result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
    assert_eq!(pipe_result, 0);

    manager
        .watchdog_deadlines
        .insert("demo.service".to_string(), std::time::Instant::now());
    manager
        .cgroup_paths
        .insert("demo.service".to_string(), PathBuf::from("/sys/fs/cgroup/demo"));
    manager.fd_store.insert(
        "demo.service".to_string(),
        vec![("stored".to_string(), pipe_fds[0])],
    );

    manager.cleanup_stopped_service("demo.service");

    assert!(!manager.watchdog_deadlines.contains_key("demo.service"));
    assert!(!manager.cgroup_paths.contains_key("demo.service"));
    assert!(!manager.fd_store.contains_key("demo.service"));
    unsafe {
        libc::close(pipe_fds[1]);
    }
}

#[tokio::test]
async fn stop_active_service_without_child_marks_it_stopped() {
    let mut manager = Manager::new();
    insert_service(&mut manager, "demo.service", service("demo.service", |_| {}));
    manager
        .states
        .get_mut("demo.service")
        .unwrap()
        .set_running(0);

    manager.stop("demo").await.unwrap();

    let state = manager.states.get("demo.service").unwrap();
    assert_eq!(state.active, ActiveState::Inactive);
    assert_eq!(state.sub, SubState::Exited);
}

#[tokio::test]
async fn scope_accessors_register_and_unregister_scope_state() {
    let mut manager = Manager::new_user();

    assert!(manager.cgroup_manager().is_none());
    assert!(!manager.scope_manager().exists("session-88.scope"));
    assert!(!manager.scope_manager_mut().exists("session-88.scope"));

    manager
        .register_scope("session-88.scope", None, Some("Session 88"), &[])
        .await
        .unwrap();
    assert!(manager.states.contains_key("session-88.scope"));
    assert!(manager.scope_manager().exists("session-88.scope"));

    manager.unregister_scope("session-88.scope").await.unwrap();
    assert!(!manager.states.contains_key("session-88.scope"));
    assert!(!manager.scope_manager().exists("session-88.scope"));
}

#[test]
fn environment_import_unset_and_reset_failed_update_manager_state() {
    let mut manager = Manager::new();
    manager.import_environment(vec![
        ("DISPLAY".to_string(), ":1".to_string()),
        ("WAYLAND_DISPLAY".to_string(), "wayland-1".to_string()),
    ]);
    assert_eq!(
        manager.get_user_environment().get("DISPLAY").map(String::as_str),
        Some(":1")
    );

    manager.unset_environment(&["DISPLAY".to_string()]);
    assert!(!manager.get_user_environment().contains_key("DISPLAY"));
    assert!(manager.get_user_environment().contains_key("WAYLAND_DISPLAY"));

    manager
        .states
        .insert("bad.service".to_string(), ServiceState::new());
    manager
        .states
        .get_mut("bad.service")
        .unwrap()
        .set_failed("boom".to_string());
    manager.reset_failed();
    let state = manager.states.get("bad.service").unwrap();
    assert_eq!(state.active, ActiveState::Inactive);
    assert_eq!(state.sub, SubState::Dead);
}

#[test]
fn service_helpers_extract_limits_default_instance_and_hash_changes() {
    let mut demo = service("demo.service", |service| {
        service.service.memory_max = Some(1024);
        service.service.cpu_quota = Some(50);
        service.service.tasks_max = Some(25);
        service.service.exec_start = vec!["/bin/true".to_string()];
        service.install = InstallSection {
            default_instance: Some("blue".to_string()),
            ..InstallSection::default()
        };
    });

    let limits = service_cgroup_limits(&demo);
    assert_eq!(limits.memory_max, Some(1024));
    assert_eq!(limits.cpu_quota, Some(50));
    assert_eq!(limits.tasks_max, Some(25));
    assert_eq!(
        default_instance_for_unit(&Unit::Service(demo.clone())).as_deref(),
        Some("blue")
    );

    let before = service_config_hash(&demo);
    demo.service.exec_start = vec!["/bin/false".to_string()];
    assert_ne!(service_config_hash(&demo), before);
}

#[test]
fn default_and_error_helpers_cover_remaining_simple_branches() {
    let manager = Manager::default();
    assert!(manager.list_units().is_empty());

    let io_error: ManagerError = std::io::Error::new(std::io::ErrorKind::Other, "boom").into();
    assert!(matches!(io_error, ManagerError::Io(message) if message == "boom"));
}

#[test]
fn default_instance_helper_handles_socket_timer_and_non_installable_units() {
    let mut socket = Socket::new("demo.socket".to_string());
    socket.install.default_instance = Some("sock".to_string());
    assert_eq!(
        default_instance_for_unit(&Unit::Socket(socket)).as_deref(),
        Some("sock")
    );

    let mut timer = Timer::new("demo.timer".to_string());
    timer.install.default_instance = Some("timer".to_string());
    assert_eq!(
        default_instance_for_unit(&Unit::Timer(timer)).as_deref(),
        Some("timer")
    );

    assert_eq!(
        default_instance_for_unit(&Unit::Target(Target::new("multi-user.target".to_string()))),
        None
    );
}

#[test]
fn unique_path_and_dependency_helpers_deduplicate_inputs() {
    let mut paths = Vec::new();
    let mut seen_paths = HashSet::new();
    push_unique_path(&mut paths, &mut seen_paths, PathBuf::from("/a"));
    push_unique_path(&mut paths, &mut seen_paths, PathBuf::from("/a"));
    push_unique_path(&mut paths, &mut seen_paths, PathBuf::from("/b"));
    assert_eq!(paths, [PathBuf::from("/a"), PathBuf::from("/b")]);

    let mut deps = Vec::new();
    let mut queued = HashSet::new();
    queue_dependency(&mut deps, &mut queued, "network.target");
    queue_dependency(&mut deps, &mut queued, "network.target");
    queue_dependency(&mut deps, &mut queued, "dbus.service");
    assert_eq!(deps, ["network.target", "dbus.service"]);
}

#[test]
fn oneshot_completion_result_reports_success_failure_and_io_error() {
    let success = std::process::Command::new("/bin/true").output();
    assert_eq!(oneshot_completion_result(success), (Some(0), None));

    let failure = std::process::Command::new("/bin/false").output();
    assert_eq!(
        oneshot_completion_result(failure),
        (Some(1), Some("exit code 1".to_string()))
    );

    let io_error = std::process::Command::new("/definitely/missing/sysd-test").output();
    let (code, error) = oneshot_completion_result(io_error);
    assert_eq!(code, None);
    assert!(error.unwrap().contains("No such file"));
}

#[test]
fn default_target_resolves_symlink_file_and_missing_cases() {
    let root = temp_dir("default-target");
    let mut manager = Manager::new();
    manager.unit_paths = vec![root.0.clone()];

    assert!(matches!(
        manager.get_default_target(),
        Err(ManagerError::NotFound(name)) if name == "default.target"
    ));

    let default_path = root.0.join("default.target");
    std::fs::write(&default_path, "").unwrap();
    assert_eq!(manager.get_default_target().unwrap(), "default.target");
    std::fs::remove_file(&default_path).unwrap();

    symlink("multi-user.target", &default_path).unwrap();
    assert_eq!(manager.get_default_target().unwrap(), "multi-user.target");
}

#[tokio::test]
async fn reload_units_skips_scopes_missing_files_and_reload_errors() {
    let root = temp_dir("reload");
    let mut manager = Manager::new();
    manager.unit_paths = vec![root.0.clone()];
    insert_service(&mut manager, "demo.service", service("demo.service", |_| {}));
    insert_service(
        &mut manager,
        "broken.service",
        service("broken.service", |_| {}),
    );
    insert_service(
        &mut manager,
        "session.scope",
        service("session.scope", |_| {}),
    );
    manager
        .states
        .insert("session.scope".to_string(), ServiceState::running_scope());

    write_service(&root.0.join("demo.service"), "/bin/true");
    std::fs::create_dir(root.0.join("broken.service")).unwrap();

    assert_eq!(manager.reload_units().await.unwrap(), 1);
    assert_eq!(
        manager
            .get_service("demo")
            .unwrap()
            .service
            .exec_start
            .as_slice(),
        ["/bin/true".to_string()]
    );
    assert!(manager.states.contains_key("session.scope"));
}

#[tokio::test]
async fn sync_units_reports_no_restarts_for_unchanged_or_inactive_services() {
    let root = temp_dir("sync");
    let mut manager = Manager::new();
    manager.unit_paths = vec![root.0.clone()];
    insert_service(
        &mut manager,
        "unchanged.service",
        service("unchanged.service", |service| {
            service.service.exec_start = vec!["/bin/true".to_string()];
        }),
    );
    insert_service(
        &mut manager,
        "changed.service",
        service("changed.service", |service| {
            service.service.exec_start = vec!["/bin/true".to_string()];
        }),
    );

    write_service(&root.0.join("unchanged.service"), "/bin/true");
    write_service(&root.0.join("changed.service"), "/bin/false");

    assert!(!manager.service_config_changed(
        "unchanged.service",
        service_config_hash(manager.get_service("unchanged").unwrap())
    ));
    assert!(!manager
        .restart_changed_running_service("changed.service")
        .await);

    assert!(manager.sync_units().await.unwrap().is_empty());
    assert_eq!(
        manager
            .get_service("changed")
            .unwrap()
            .service
            .exec_start
            .as_slice(),
        ["/bin/false".to_string()]
    );
}

#[tokio::test]
async fn get_boot_plan_reports_missing_targets() {
    let mut manager = Manager::new();

    assert!(matches!(
        manager.get_boot_plan("definitely-missing").await,
        Err(ManagerError::NotFound(name)) if name == "definitely-missing.service"
    ));
}

#[tokio::test]
async fn restart_normalizes_names_and_returns_start_error_after_not_active_stop() {
    let mut manager = Manager::new();
    insert_service(&mut manager, "empty.service", service("empty.service", |_| {}));

    assert!(matches!(
        manager.restart("empty").await,
        Err(ManagerError::Spawn(SpawnError::NoExecStart(name))) if name == "empty.service"
    ));
    assert!(manager.status("empty").is_some());
}

#[tokio::test]
async fn switch_target_stops_active_units_and_marks_target_reached() {
    let mut manager = Manager::new();
    insert_target(&mut manager, "rescue.target");
    insert_service(&mut manager, "old.service", service("old.service", |_| {}));
    manager
        .states
        .get_mut("old.service")
        .unwrap()
        .set_running(0);

    let stopped = manager.switch_target("rescue").await.unwrap();

    assert_eq!(stopped, ["old.service".to_string()]);
    assert!(!manager.status("old").unwrap().is_active());
    assert!(manager.status("rescue").unwrap().is_active());
}

#[test]
fn cleanup_stopped_service_releases_dynamic_uid_and_stored_fds() {
    let mut manager = Manager::new();
    insert_service(
        &mut manager,
        "dynamic.service",
        service("dynamic.service", |_| {}),
    );
    let (uid, _) = manager
        .dynamic_user_manager
        .allocate("dynamic.service")
        .unwrap();
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);

    manager.dynamic_uids.insert("dynamic.service".to_string(), uid);
    manager.fd_store.insert(
        "dynamic.service".to_string(),
        vec![("pipe-read".to_string(), fds[0])],
    );

    manager.cleanup_stopped_service("dynamic.service");

    assert!(!manager.dynamic_uids.contains_key("dynamic.service"));
    assert!(!manager.fd_store.contains_key("dynamic.service"));
    assert_eq!(unsafe { libc::close(fds[1]) }, 0);
}

#[tokio::test]
async fn run_simple_command_handles_empty_success_failure_and_missing_commands() {
    run_simple_command("").await.unwrap();
    run_simple_command("-+/bin/true").await.unwrap();

    let failed = run_simple_command("/bin/false").await.unwrap_err();
    assert_eq!(failed.kind(), std::io::ErrorKind::Other);
    assert!(failed.to_string().contains("Command exited"));

    let missing = run_simple_command("/definitely/missing/sysd-test")
        .await
        .unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
}
