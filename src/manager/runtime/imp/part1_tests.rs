use super::*;
use crate::manager::state::ServiceState;
use crate::units::{Service, Unit};
use std::collections::HashMap;
use std::time::Duration;

fn service_unit(name: &str, configure: impl FnOnce(&mut Service)) -> Unit {
    let mut service = Service::new(name.to_string());
    configure(&mut service);
    Unit::Service(service)
}

fn notify(pid: u32, fields: &[(&str, &str)]) -> NotifyMessage {
    NotifyMessage {
        pid,
        fields: fields
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>(),
        fds: Vec::new(),
    }
}

fn notify_with_fds(pid: u32, fields: &[(&str, &str)], fds: Vec<i32>) -> NotifyMessage {
    NotifyMessage {
        pid,
        fields: fields
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>(),
        fds,
    }
}

fn pipe_fds() -> [libc::c_int; 2] {
    let mut fds = [0; 2];
    let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(result, 0);
    fds
}

fn manager_with_service(name: &str, configure: impl FnOnce(&mut Service)) -> Manager {
    let mut manager = Manager::new();
    manager
        .units
        .insert(name.to_string(), service_unit(name, configure));
    manager.states.insert(name.to_string(), ServiceState::new());
    manager
}

#[test]
fn notify_access_defaults_and_reads_service_setting() {
    let mut manager = Manager::new();
    assert_eq!(
        manager.notify_access_for_service("missing.service"),
        NotifyAccess::Main
    );

    manager.units.insert(
        "notify.service".to_string(),
        service_unit("notify.service", |service| {
            service.service.notify_access = NotifyAccess::All;
        }),
    );

    assert_eq!(
        manager.notify_access_for_service("notify.service"),
        NotifyAccess::All
    );
}

#[test]
fn validate_notify_access_accepts_unknown_pid_and_rejects_notify_none() {
    let mut manager = manager_with_service("notify.service", |service| {
        service.service.notify_access = NotifyAccess::None;
    });
    manager.waiting_ready.insert(22, "notify.service".to_string());

    assert!(manager.validate_notify_access(&notify(99, &[])));
    assert!(!manager.validate_notify_access(&notify(22, &[])));
}

#[test]
fn resolve_ready_service_name_prefers_main_pid_then_sender_pid() {
    let mut manager = Manager::new();
    manager.waiting_ready.insert(10, "main.service".to_string());
    manager.waiting_ready.insert(20, "sender.service".to_string());

    let resolved = manager.resolve_ready_service_name(&notify(20, &[("MAINPID", "10")]));
    assert_eq!(resolved.as_deref(), Some("main.service"));
    assert!(!manager.waiting_ready.contains_key(&10));
    assert!(manager.waiting_ready.contains_key(&20));

    let resolved = manager.resolve_ready_service_name(&notify(20, &[]));
    assert_eq!(resolved.as_deref(), Some("sender.service"));
    assert!(!manager.waiting_ready.contains_key(&20));
}

#[test]
fn mark_service_ready_sets_running_state_and_arms_watchdog() {
    let mut manager = manager_with_service("notify.service", |service| {
        service.service.watchdog_sec = Some(Duration::from_secs(30));
    });
    manager.active_jobs = 2;

    manager.mark_service_ready("notify.service");

    let state = manager.states.get("notify.service").unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.sub, SubState::Running);
    assert_eq!(state.main_pid, Some(0));
    assert_eq!(manager.active_jobs, 1);
    assert!(manager.watchdog_deadlines.contains_key("notify.service"));
}

#[test]
fn dispatch_notify_stores_and_removes_named_file_descriptors() {
    let mut manager = manager_with_service("store.service", |service| {
        service.service.file_descriptor_store_max = Some(2);
    });
    manager.waiting_ready.insert(77, "store.service".to_string());
    let fds = pipe_fds();

    manager.dispatch_notify(&notify_with_fds(
        77,
        &[("FDSTORE", "1"), ("FDNAME", "cache")],
        vec![fds[0]],
    ));

    let stored = manager.fd_store.get("store.service").unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].0, "cache");

    manager.dispatch_notify(&notify(77, &[("FDSTOREREMOVE", "1"), ("FDNAME", "cache")]));

    assert!(manager.fd_store.get("store.service").unwrap().is_empty());
    unsafe {
        libc::close(fds[1]);
    }
}

#[test]
fn fdstore_enforces_file_descriptor_store_limit() {
    let mut manager = manager_with_service("limited.service", |service| {
        service.service.file_descriptor_store_max = Some(1);
    });
    manager.waiting_ready.insert(88, "limited.service".to_string());
    let first = pipe_fds();
    let second = pipe_fds();

    manager.handle_fdstore(&notify_with_fds(
        88,
        &[("FDSTORE", "1"), ("FDNAME", "first")],
        vec![first[0]],
    ));
    manager.handle_fdstore(&notify_with_fds(
        88,
        &[("FDSTORE", "1"), ("FDNAME", "second")],
        vec![second[0]],
    ));

    let stored = manager.fd_store.get("limited.service").unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].0, "first");
    unsafe {
        libc::close(first[1]);
        libc::close(second[1]);
    }
}

#[test]
fn forking_pid_file_paths_update_running_state_or_report_errors() {
    let mut manager = manager_with_service("forking.service", |service| {
        service.service.watchdog_sec = Some(Duration::from_secs(5));
    });
    manager.active_jobs = 1;
    let dir = std::env::temp_dir().join(format!("sysd-runtime-pid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("service.pid");

    assert!(manager.read_forking_pid_file(&pid_file).is_err());
    std::fs::write(&pid_file, "not-a-pid").unwrap();
    assert!(manager.read_forking_pid_file(&pid_file).is_err());
    std::fs::write(&pid_file, "4242\n").unwrap();
    assert_eq!(manager.read_forking_pid_file(&pid_file), Ok(4242));

    manager
        .pid_files
        .insert("forking.service".to_string(), pid_file.clone());
    assert!(manager.reap_forking_parent("forking.service", 0));

    let state = manager.states.get("forking.service").unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.sub, SubState::Running);
    assert_eq!(state.main_pid, Some(4242));
    assert_eq!(manager.active_jobs, 0);
    assert!(manager.watchdog_deadlines.contains_key("forking.service"));
    assert!(!manager.pid_files.contains_key("forking.service"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn forking_parent_exit_without_pid_file_marks_service_running() {
    let mut manager = manager_with_service("forking.service", |_| {});
    manager.active_jobs = 1;

    assert!(manager.reap_forking_parent("forking.service", 0));

    let state = manager.states.get("forking.service").unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.sub, SubState::Running);
    assert_eq!(state.main_pid, Some(0));
    assert_eq!(manager.active_jobs, 0);
    assert!(!manager.reap_forking_parent("forking.service", 1));
}

#[test]
fn apply_restart_decision_handles_clean_oneshot_remain_after_exit() {
    let mut manager = Manager::new();
    manager
        .states
        .insert("oneshot.service".to_string(), ServiceState::new());

    manager.apply_restart_decision(
        "oneshot.service",
        0,
        true,
        true,
        &RestartPolicy::No,
        Duration::from_secs(1),
        None,
        None,
        &[],
    );

    let state = manager.states.get("oneshot.service").unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.sub, SubState::Exited);
    assert_eq!(state.exit_code, Some(0));
}

#[test]
fn apply_restart_decision_schedules_restart_and_honors_prevent_status() {
    let mut manager = Manager::new();
    manager
        .states
        .insert("retry.service".to_string(), ServiceState::new());
    manager
        .states
        .insert("prevent.service".to_string(), ServiceState::new());

    manager.apply_restart_decision(
        "retry.service",
        1,
        false,
        false,
        &RestartPolicy::OnFailure,
        Duration::from_secs(5),
        None,
        None,
        &[],
    );
    manager.apply_restart_decision(
        "prevent.service",
        77,
        false,
        false,
        &RestartPolicy::Always,
        Duration::from_secs(5),
        None,
        None,
        &[77],
    );

    let retry = manager.states.get("retry.service").unwrap();
    assert_eq!(retry.active, ActiveState::Activating);
    assert_eq!(retry.sub, SubState::AutoRestart);
    assert!(retry.restart_at.is_some());

    let prevent = manager.states.get("prevent.service").unwrap();
    assert_eq!(prevent.active, ActiveState::Inactive);
    assert_eq!(prevent.sub, SubState::Dead);
    assert_eq!(prevent.exit_code, Some(77));
}

#[test]
fn apply_restart_decision_marks_rate_limited_restart_as_failed() {
    let mut manager = Manager::new();
    let mut state = ServiceState::new();
    state.restart_count = 1;
    state.restart_interval_start = Some(std::time::Instant::now());
    manager.states.insert("limited.service".to_string(), state);

    manager.apply_restart_decision(
        "limited.service",
        1,
        false,
        false,
        &RestartPolicy::Always,
        Duration::from_secs(5),
        Some(1),
        Some(Duration::from_secs(60)),
        &[],
    );

    let state = manager.states.get("limited.service").unwrap();
    assert_eq!(state.active, ActiveState::Failed);
    assert_eq!(state.sub, SubState::Failed);
    assert!(state.error.as_deref().unwrap().contains("Start limit hit"));
}

#[tokio::test]
async fn cleanup_after_exit_removes_runtime_state_when_not_restarting() {
    let mut manager = manager_with_service("done.service", |_| {});
    let fds = pipe_fds();
    manager
        .watchdog_deadlines
        .insert("done.service".to_string(), std::time::Instant::now());
    manager
        .cgroup_paths
        .insert("done.service".to_string(), "/sys/fs/cgroup/done".into());
    manager.dynamic_uids.insert("done.service".to_string(), 61000);
    manager.fd_store.insert(
        "done.service".to_string(),
        vec![("stored".to_string(), fds[0])],
    );

    manager.cleanup_after_exit("done.service").await;

    assert!(!manager.watchdog_deadlines.contains_key("done.service"));
    assert!(!manager.cgroup_paths.contains_key("done.service"));
    assert!(!manager.dynamic_uids.contains_key("done.service"));
    assert!(!manager.fd_store.contains_key("done.service"));
    unsafe {
        libc::close(fds[1]);
    }
}

#[tokio::test]
async fn cleanup_after_exit_keeps_restart_resources_for_auto_restart() {
    let mut manager = manager_with_service("restart.service", |_| {});
    let mut state = ServiceState::new();
    state.set_auto_restart(Duration::from_secs(1));
    manager.states.insert("restart.service".to_string(), state);
    let fds = pipe_fds();
    manager.dynamic_uids.insert("restart.service".to_string(), 61001);
    manager.fd_store.insert(
        "restart.service".to_string(),
        vec![("stored".to_string(), fds[0])],
    );

    manager.cleanup_after_exit("restart.service").await;

    assert!(manager.dynamic_uids.contains_key("restart.service"));
    assert!(manager.fd_store.contains_key("restart.service"));
    let stored = manager.fd_store.remove("restart.service").unwrap();
    unsafe {
        libc::close(stored[0].1);
        libc::close(fds[1]);
    }
}

#[test]
fn read_restart_policy_returns_service_values_or_defaults() {
    let manager = manager_with_service("custom.service", |service| {
        service.service.restart = RestartPolicy::Always;
        service.service.restart_sec = Duration::from_secs(9);
        service.service.remain_after_exit = true;
        service.service.service_type = ServiceType::Forking;
        service.service.start_limit_burst = Some(3);
        service.service.start_limit_interval_sec = Some(Duration::from_secs(30));
        service.service.restart_prevent_exit_status = vec![77, 78];
    });

    let policy = manager.read_restart_policy("custom.service");
    assert!(matches!(policy.restart_policy, RestartPolicy::Always));
    assert_eq!(policy.restart_sec, Duration::from_secs(9));
    assert!(policy.remain_after_exit);
    assert!(policy.is_forking);
    assert!(!policy.is_oneshot);
    assert_eq!(policy.start_limit_burst, Some(3));
    assert_eq!(policy.start_limit_interval_sec, Some(Duration::from_secs(30)));
    assert_eq!(policy.restart_prevent_exit_status, [77, 78]);

    let default_policy = manager.read_restart_policy("missing.service");
    assert!(matches!(default_policy.restart_policy, RestartPolicy::No));
}

#[test]
fn resolve_reaped_status_removes_known_pid_and_ignores_orphans() {
    let mut manager = Manager::new();
    let pid = nix::unistd::Pid::from_raw(1234);
    manager
        .pid_to_service
        .insert(1234, "worker.service".to_string());

    assert_eq!(
        manager.resolve_reaped_status(nix::sys::wait::WaitStatus::Exited(pid, 7)),
        Some(("worker.service".to_string(), 7))
    );
    assert!(!manager.pid_to_service.contains_key(&1234));
    assert_eq!(
        manager.resolve_reaped_status(nix::sys::wait::WaitStatus::Exited(pid, 7)),
        None
    );
}

#[test]
fn decode_wait_status_maps_exit_signal_and_non_terminal_states() {
    let pid = nix::unistd::Pid::from_raw(4321);

    assert_eq!(
        Manager::decode_wait_status(nix::sys::wait::WaitStatus::Exited(pid, 3)),
        Some((4321, 3))
    );
    assert_eq!(
        Manager::decode_wait_status(nix::sys::wait::WaitStatus::Signaled(
            pid,
            nix::sys::signal::Signal::SIGTERM,
            false,
        )),
        Some((4321, -(nix::sys::signal::Signal::SIGTERM as i32)))
    );
    assert_eq!(
        Manager::decode_wait_status(nix::sys::wait::WaitStatus::StillAlive),
        None
    );
}
