use super::*;
use crate::units::{
    Mount, PathUnit, RuntimeDirectoryPreserve, Service, Slice, Socket, Target, Timer, Unit,
};
use std::path::PathBuf;
use std::time::Duration;

fn service(name: &str, configure: impl FnOnce(&mut Service)) -> Service {
    let mut service = Service::new(name.to_string());
    configure(&mut service);
    service
}

fn manager_with_state(name: &str) -> Manager {
    let mut manager = Manager::new();
    manager
        .states
        .insert(name.to_string(), ServiceState::new());
    manager
}

#[test]
fn mark_service_starting_updates_state_and_rejects_missing_or_active_units() {
    let mut manager = manager_with_state("demo.service");

    assert!(matches!(
        manager.mark_service_starting("missing.service"),
        Err(ManagerError::NotFound(name)) if name == "missing.service"
    ));

    manager.mark_service_starting("demo.service").unwrap();
    let state = manager.states.get("demo.service").unwrap();
    assert_eq!(state.active, ActiveState::Activating);
    assert_eq!(state.sub, SubState::Starting);
    assert_eq!(manager.active_jobs, 1);

    state_is_running(&mut manager, "demo.service");
    assert!(matches!(
        manager.mark_service_starting("demo.service"),
        Err(ManagerError::AlreadyActive(name)) if name == "demo.service"
    ));
}

fn state_is_running(manager: &mut Manager, name: &str) {
    manager.states.get_mut(name).unwrap().set_running(42);
}

#[test]
fn mark_service_type_start_helpers_record_waiting_state() {
    let mut manager = manager_with_state("demo.service");
    manager.active_jobs = 2;

    let notify = service("demo.service", |service| {
        service.service.service_type = ServiceType::Notify;
    });
    manager.configure_post_spawn_state("demo.service", 100, &notify);
    assert_eq!(
        manager.waiting_ready.get(&100).map(String::as_str),
        Some("demo.service")
    );
    assert_eq!(manager.active_jobs, 2);

    let dbus = service("demo.service", |service| {
        service.service.service_type = ServiceType::Dbus;
        service.service.bus_name = Some("com.example.Demo".to_string());
    });
    manager.configure_post_spawn_state("demo.service", 101, &dbus);
    assert_eq!(
        manager
            .waiting_bus_name
            .get("com.example.Demo")
            .map(String::as_str),
        Some("demo.service")
    );

    let forking = service("demo.service", |service| {
        service.service.service_type = ServiceType::Forking;
        service.service.pid_file = Some(PathBuf::from("/run/demo.pid"));
    });
    manager.configure_post_spawn_state("demo.service", 102, &forking);
    assert_eq!(
        manager.pid_files.get("demo.service"),
        Some(&PathBuf::from("/run/demo.pid"))
    );
}

#[test]
fn mark_running_start_sets_running_state_decrements_job_and_arms_watchdog() {
    let mut manager = manager_with_state("demo.service");
    manager.active_jobs = 1;
    let simple = service("demo.service", |service| {
        service.service.watchdog_sec = Some(Duration::from_secs(12));
    });

    manager.configure_post_spawn_state("demo.service", 321, &simple);

    let state = manager.states.get("demo.service").unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.sub, SubState::Running);
    assert_eq!(state.main_pid, Some(321));
    assert_eq!(manager.active_jobs, 0);
    assert!(manager.watchdog_deadlines.contains_key("demo.service"));
}

#[test]
fn dbus_without_bus_name_falls_back_to_running_state() {
    let mut manager = manager_with_state("dbus.service");
    manager.active_jobs = 1;
    let dbus = service("dbus.service", |service| {
        service.service.service_type = ServiceType::Dbus;
    });

    manager.configure_post_spawn_state("dbus.service", 55, &dbus);

    let state = manager.states.get("dbus.service").unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.sub, SubState::Running);
    assert_eq!(state.main_pid, Some(55));
    assert!(manager.waiting_bus_name.is_empty());
}

#[test]
fn log_oneshot_start_returns_exec_command_count() {
    let manager = Manager::new();
    let oneshot = service("oneshot.service", |service| {
        service.service.exec_start = vec!["/bin/true".to_string(), "/bin/echo done".to_string()];
    });

    assert_eq!(manager.log_oneshot_start("oneshot.service", &oneshot), 2);
}

#[test]
fn prepare_socket_fds_uses_explicit_socket_names() {
    let mut manager = Manager::new();
    manager
        .socket_fds
        .insert("api.socket".to_string(), vec![10, 11]);
    let service = service("api.service", |service| {
        service.service.sockets = vec!["api.socket".to_string()];
    });
    manager
        .units
        .insert("api.service".to_string(), Unit::Service(service.clone()));

    let (fds, names) = manager.prepare_socket_fds(&service, "api.service");

    assert_eq!(fds, [10, 11]);
    assert_eq!(names, ["api", "api"]);
}

#[test]
fn build_spawn_options_carries_runtime_inputs_and_stored_fds() {
    let mut manager = Manager::new();
    manager
        .fd_store
        .insert("demo.service".to_string(), vec![("cache".to_string(), 22)]);
    manager
        .user_environment
        .insert("SESSION".to_string(), "desktop".to_string());
    let notify = service("demo.service", |service| {
        service.service.service_type = ServiceType::Notify;
        service.service.watchdog_sec = Some(Duration::from_secs(3));
    });

    let options = manager.build_spawn_options(
        &notify,
        "demo.service",
        vec![10],
        vec!["api".to_string()],
        Some(5000),
        Some(5001),
    );

    assert_eq!(options.watchdog_usec, Some(3_000_000));
    assert_eq!(options.socket_fds, [10]);
    assert_eq!(options.socket_fd_names, ["api"]);
    assert_eq!(options.dynamic_uid, Some(5000));
    assert_eq!(options.dynamic_gid, Some(5001));
    assert_eq!(options.stored_fds, [22]);
    assert_eq!(
        options.user_environment.get("SESSION").map(String::as_str),
        Some("desktop")
    );
}

#[tokio::test]
async fn resolve_start_unit_name_returns_loaded_units_without_disk_lookup() {
    let mut manager = Manager::new_user();
    manager.units.insert(
        "loaded.service".to_string(),
        Unit::Service(service("loaded.service", |_| {})),
    );

    let actual_name = manager.resolve_start_unit_name("loaded.service").await.unwrap();

    assert_eq!(actual_name, "loaded.service");
}

#[tokio::test]
async fn resolve_start_unit_name_reports_missing_units_from_load() {
    let mut manager = Manager::new_user();
    manager.unit_paths.clear();

    let result = manager
        .resolve_start_unit_name("definitely-missing-sysd-test.service")
        .await;

    assert!(matches!(
        result,
        Err(ManagerError::NotFound(name)) if name == "definitely-missing-sysd-test.service"
    ));
}

#[tokio::test]
async fn start_non_service_unit_handles_target_slice_timer_and_service_fallback() {
    let mut manager = Manager::new_user();
    manager
        .states
        .insert("system.slice".to_string(), ServiceState::new());
    manager
        .states
        .insert("cleanup.timer".to_string(), ServiceState::new());

    assert!(matches!(
        manager
            .start_non_service_unit(
                "multi-user.target",
                &Unit::Target(Target::new("multi-user.target".to_string())),
            )
            .await,
        Err(ManagerError::IsTarget(name)) if name == "multi-user.target"
    ));

    assert!(matches!(
        manager
            .start_non_service_unit(
                "system.slice",
                &Unit::Slice(Slice::new("system.slice".to_string())),
            )
            .await,
        Ok(true)
    ));
    assert!(manager.states.get("system.slice").unwrap().is_active());

    assert!(matches!(
        manager
            .start_non_service_unit(
                "cleanup.timer",
                &Unit::Timer(Timer::new("cleanup.timer".to_string())),
            )
            .await,
        Ok(true)
    ));
    assert!(manager.states.get("cleanup.timer").unwrap().is_active());

    assert!(matches!(
        manager
            .start_non_service_unit(
                "plain.service",
                &Unit::Service(service("plain.service", |_| {})),
            )
            .await,
        Ok(false)
    ));
}

#[tokio::test]
async fn start_non_service_unit_handles_path_socket_and_mount_branches_safely() {
    let mut manager = Manager::new_user();
    manager
        .states
        .insert("watch.path".to_string(), ServiceState::new());
    manager
        .states
        .insert("-.mount".to_string(), ServiceState::new());

    assert!(matches!(
        manager
            .start_non_service_unit(
                "watch.path",
                &Unit::Path(PathUnit::new("watch.path".to_string())),
            )
            .await,
        Ok(true)
    ));
    assert!(manager.states.get("watch.path").unwrap().is_active());

    assert!(matches!(
        manager
            .start_non_service_unit(
                "missing-state.socket",
                &Unit::Socket(Socket::new("missing-state.socket".to_string())),
            )
            .await,
        Err(ManagerError::NotFound(name)) if name == "missing-state.socket"
    ));

    let mut root_mount = Mount::new("-.mount".to_string());
    root_mount.mount.r#where = "/".to_string();
    root_mount.mount.what = "rootfs".to_string();
    root_mount.mount.directory_mode = None;

    assert!(matches!(
        manager
            .start_non_service_unit("-.mount", &Unit::Mount(root_mount))
            .await,
        Ok(true)
    ));
    assert!(manager.states.get("-.mount").unwrap().is_active());
}

#[tokio::test]
async fn start_non_service_unit_reports_failed_conditions_before_mounting() {
    let mut manager = Manager::new_user();
    let mut mount = Mount::new("tmp-sysd-missing.mount".to_string());
    mount
        .unit
        .condition_path_exists
        .push("/definitely/missing/sysd-condition".to_string());

    let result = manager
        .start_non_service_unit("tmp-sysd-missing.mount", &Unit::Mount(mount))
        .await;

    assert!(matches!(
        result,
        Err(ManagerError::ConditionFailed(name, reason))
            if name == "tmp-sysd-missing.mount"
                && reason.contains("ConditionPathExists=")
    ));
}

#[test]
fn allocate_dynamic_user_records_allocated_uid_and_skips_static_services() {
    let mut manager = Manager::new_user();
    let dynamic = service("dynamic.service", |service| {
        service.service.dynamic_user = true;
    });
    let static_service = service("static.service", |_| {});

    let (uid, gid) = manager
        .allocate_dynamic_user("dynamic.service", &dynamic)
        .unwrap();
    assert_eq!(uid, gid);
    let uid = uid.unwrap();
    assert!(crate::manager::dynamic_user::DynamicUserManager::is_dynamic_uid(uid));
    assert_eq!(manager.dynamic_uids.get("dynamic.service"), Some(&uid));

    assert_eq!(
        manager
            .allocate_dynamic_user("static.service", &static_service)
            .unwrap(),
        (None, None)
    );
}

#[tokio::test]
async fn idle_queue_returns_immediately_when_no_other_jobs_are_active() {
    let mut manager = Manager::new_user();
    manager.active_jobs = 1;

    let start = std::time::Instant::now();
    manager.wait_for_idle_queue("idle.service").await;

    assert!(start.elapsed() < Duration::from_millis(50));
}

#[test]
fn cleanup_runtime_dirs_respects_preserve_yes_and_ignores_non_services() {
    let mut manager = Manager::new_user();
    manager.units.insert(
        "preserve.service".to_string(),
        Unit::Service(service("preserve.service", |service| {
            service.service.runtime_directory = vec!["sysd-test-preserve".to_string()];
            service.service.runtime_directory_preserve = RuntimeDirectoryPreserve::Yes;
        })),
    );
    manager.units.insert(
        "plain.target".to_string(),
        Unit::Target(Target::new("plain.target".to_string())),
    );

    manager.cleanup_runtime_dirs("preserve.service");
    manager.cleanup_runtime_dirs("plain.target");
    manager.cleanup_runtime_dirs("missing.service");
}

#[test]
fn cleanup_runtime_dirs_allows_no_and_restart_when_directories_are_absent() {
    let mut manager = Manager::new_user();
    manager.units.insert(
        "remove.service".to_string(),
        Unit::Service(service("remove.service", |service| {
            service.service.runtime_directory =
                vec!["sysd-test-absent-remove-dir".to_string(), String::new()];
            service.service.runtime_directory_preserve = RuntimeDirectoryPreserve::No;
        })),
    );
    manager.units.insert(
        "restart.service".to_string(),
        Unit::Service(service("restart.service", |service| {
            service.service.runtime_directory = vec!["sysd-test-absent-restart-dir".to_string()];
            service.service.runtime_directory_preserve = RuntimeDirectoryPreserve::Restart;
        })),
    );

    manager.cleanup_runtime_dirs("remove.service");
    manager.cleanup_runtime_dirs("restart.service");
}

#[test]
fn prepare_socket_fds_logs_missing_socket_fds_and_returns_empty_values() {
    let manager = Manager::new_user();
    let svc = service("socketed.service", |service| {
        service.service.sockets = vec!["missing.socket".to_string()];
    });

    let (fds, names) = manager.prepare_socket_fds(&svc, "socketed.service");

    assert!(fds.is_empty());
    assert!(names.is_empty());
}

#[test]
fn build_spawn_options_omits_notify_socket_for_plain_services_without_watchdog() {
    let manager = Manager::new_user();
    let svc = service("plain.service", |_| {});

    let options =
        manager.build_spawn_options(&svc, "plain.service", Vec::new(), Vec::new(), None, None);

    assert!(options.notify_socket.is_none());
    assert_eq!(options.watchdog_usec, None);
    assert!(options.socket_fds.is_empty());
    assert!(options.socket_fd_names.is_empty());
    assert!(options.stored_fds.is_empty());
}

#[tokio::test]
async fn build_spawn_options_includes_notify_watchdog_fds_dynamic_ids_and_environment() {
    let mut manager = Manager::new_user();
    let notify_path = std::env::temp_dir().join(format!(
        "sysd-build-options-notify-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (listener, rx) = AsyncNotifyListener::new(&notify_path).unwrap();
    manager.notify_listener = Some(listener);
    manager.notify_rx = Some(rx);
    manager
        .fd_store
        .insert("api.service".to_string(), vec![("stored".to_string(), 12)]);
    manager
        .user_environment
        .insert("LANG".to_string(), "C.UTF-8".to_string());
    let svc = service("api.service", |service| {
        service.service.service_type = ServiceType::Notify;
        service.service.watchdog_sec = Some(Duration::from_secs(5));
    });

    let options = manager.build_spawn_options(
        &svc,
        "api.service",
        vec![3, 4],
        vec!["api".to_string(), "metrics".to_string()],
        Some(100_001),
        Some(100_001),
    );

    assert!(options.notify_socket.is_some());
    assert_eq!(options.watchdog_usec, Some(5_000_000));
    assert_eq!(options.socket_fds, [3, 4]);
    assert_eq!(options.socket_fd_names, ["api", "metrics"]);
    assert_eq!(options.dynamic_uid, Some(100_001));
    assert_eq!(options.dynamic_gid, Some(100_001));
    assert_eq!(options.stored_fds, [12]);
    assert_eq!(
        options.user_environment.get("LANG").map(String::as_str),
        Some("C.UTF-8")
    );
    drop(manager);
    let _ = std::fs::remove_file(notify_path);
}

#[tokio::test]
async fn wait_for_child_exit_records_success_status() {
    let mut manager = manager_with_state("wait.service");
    manager.units.insert(
        "wait.service".to_string(),
        Unit::Service(service("wait.service", |service| {
            service.service.timeout_stop_sec = Some(Duration::from_secs(1));
        })),
    );
    let child = tokio::process::Command::new("/bin/true").spawn().unwrap();

    manager.wait_for_child_exit("wait.service", child).await;

    let state = manager.states.get("wait.service").unwrap();
    assert_eq!(state.active, ActiveState::Inactive);
    assert_eq!(state.sub, SubState::Exited);
    assert_eq!(state.exit_code, Some(0));
}

#[tokio::test]
async fn wait_for_child_exit_kills_process_after_timeout() {
    let mut manager = manager_with_state("slow.service");
    manager.units.insert(
        "slow.service".to_string(),
        Unit::Service(service("slow.service", |service| {
            service.service.timeout_stop_sec = Some(Duration::from_millis(10));
        })),
    );
    let child = tokio::process::Command::new("/bin/sleep")
        .arg("5")
        .spawn()
        .unwrap();

    manager.wait_for_child_exit("slow.service", child).await;

    let state = manager.states.get("slow.service").unwrap();
    assert_eq!(state.active, ActiveState::Inactive);
    assert_eq!(state.exit_code, Some(-9));
}

#[tokio::test]
async fn run_stop_post_commands_runs_successes_and_ignores_failures_or_missing_units() {
    let marker = std::env::temp_dir().join(format!(
        "sysd-stop-post-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut manager = Manager::new_user();
    manager.units.insert(
        "cleanup.service".to_string(),
        Unit::Service(service("cleanup.service", |service| {
            service.service.exec_stop_post = vec![
                format!("/usr/bin/touch {}", marker.display()),
                "/bin/false".to_string(),
            ];
        })),
    );

    manager.run_stop_post_commands("cleanup.service").await;
    manager.run_stop_post_commands("missing.service").await;

    assert!(marker.exists());
    let _ = std::fs::remove_file(marker);
}
