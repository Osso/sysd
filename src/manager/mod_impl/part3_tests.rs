use super::*;
use crate::units::{InstallSection, Service, Unit};
use std::collections::HashSet;
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
