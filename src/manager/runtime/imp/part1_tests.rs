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
