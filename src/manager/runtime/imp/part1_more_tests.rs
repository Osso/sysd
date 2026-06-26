use super::*;
use crate::manager::state::ServiceState;
use crate::units::{NotifyAccess, Service, Unit};
use std::collections::HashMap;
use std::time::Duration;

fn service_unit(name: &str, configure: impl FnOnce(&mut Service)) -> Unit {
    let mut service = Service::new(name.to_string());
    configure(&mut service);
    Unit::Service(service)
}

fn manager_with_service(name: &str, configure: impl FnOnce(&mut Service)) -> Manager {
    let mut manager = Manager::new();
    manager
        .units
        .insert(name.to_string(), service_unit(name, configure));
    manager.states.insert(name.to_string(), ServiceState::new());
    manager
}

fn user_manager_with_service(name: &str, configure: impl FnOnce(&mut Service)) -> Manager {
    let mut manager = Manager::new_user();
    manager
        .units
        .insert(name.to_string(), service_unit(name, configure));
    manager.states.insert(name.to_string(), ServiceState::new());
    manager
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

#[tokio::test]
async fn notify_access_main_accepts_tracked_child_pid_and_rejects_mismatch() {
    let mut manager = manager_with_service("notify.service", |service| {
        service.service.notify_access = NotifyAccess::Main;
        service.service.watchdog_sec = Some(Duration::from_secs(5));
    });
    let child = tokio::process::Command::new("/bin/sleep")
        .arg("5")
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    manager.processes.insert("notify.service".to_string(), child);

    assert_eq!(
        manager.find_service_by_pid(pid).as_deref(),
        Some("notify.service")
    );
    assert!(manager.validate_notify_access(&notify(pid, &[])));
    manager
        .waiting_ready
        .insert(pid + 1, "notify.service".to_string());
    assert!(!manager.validate_notify_access(&notify(pid + 1, &[])));

    manager.dispatch_notify(&notify(pid, &[("WATCHDOG", "1")]));
    assert!(manager.watchdog_deadlines.contains_key("notify.service"));

    let mut child = manager.processes.remove("notify.service").unwrap();
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[test]
fn fdstore_closes_unknown_or_disallowed_descriptors_and_ignores_bad_remove() {
    let mut manager = manager_with_service("limited.service", |_| {});
    manager.waiting_ready.insert(44, "limited.service".to_string());

    let unknown = pipe_fds();
    manager.handle_fdstore(&notify_with_fds(
        999,
        &[("FDSTORE", "1"), ("FDNAME", "unknown")],
        vec![unknown[0]],
    ));
    unsafe {
        libc::close(unknown[1]);
    }

    let disallowed = pipe_fds();
    manager.handle_fdstore(&notify_with_fds(
        44,
        &[("FDSTORE", "1"), ("FDNAME", "disallowed")],
        vec![disallowed[0]],
    ));
    assert!(!manager.fd_store.contains_key("limited.service"));
    unsafe {
        libc::close(disallowed[1]);
    }

    manager.handle_fdstoreremove(&notify(999, &[("FDSTOREREMOVE", "1")]));
    manager.handle_fdstoreremove(&notify(44, &[("FDSTOREREMOVE", "1")]));
}

#[tokio::test]
async fn propagate_binds_to_stop_stops_active_bound_services() {
    let mut manager = user_manager_with_service("bound.service", |service| {
        service.unit.binds_to = vec!["base.service".to_string()];
    });
    manager
        .states
        .get_mut("bound.service")
        .unwrap()
        .set_running(0);

    manager.propagate_binds_to_stop("base.service").await;

    let state = manager.states.get("bound.service").unwrap();
    assert_eq!(state.active, ActiveState::Inactive);
    assert_eq!(state.sub, SubState::Exited);
}

#[tokio::test]
async fn handle_reaped_service_applies_policy_and_cleans_runtime_state() {
    let mut manager = user_manager_with_service("reaped.service", |service| {
        service.service.restart = RestartPolicy::No;
    });
    manager.processes.clear();
    manager.watchdog_deadlines.insert(
        "reaped.service".to_string(),
        std::time::Instant::now() + Duration::from_secs(30),
    );
    manager
        .fd_store
        .insert("reaped.service".to_string(), Vec::new());

    manager
        .handle_reaped_service("reaped.service".to_string(), 0)
        .await;

    let state = manager.states.get("reaped.service").unwrap();
    assert_eq!(state.active, ActiveState::Inactive);
    assert_eq!(state.sub, SubState::Exited);
    assert_eq!(state.exit_code, Some(0));
    assert!(!manager.watchdog_deadlines.contains_key("reaped.service"));
    assert!(!manager.fd_store.contains_key("reaped.service"));
}

#[tokio::test]
async fn process_restarts_starts_due_service_with_real_executor() {
    let Some(executor) = local_executor_path() else {
        return;
    };
    let mut manager = user_manager_with_service("restart.service", |service| {
        service.service.exec_start = vec!["/bin/true".to_string()];
    });
    manager.executor_path = executor;
    manager
        .states
        .get_mut("restart.service")
        .unwrap()
        .set_auto_restart(Duration::ZERO);

    manager.process_restarts().await;

    let state = manager.states.get("restart.service").unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.sub, SubState::Running);
    assert!(state.restart_at.is_none());
    let mut child = manager.processes.remove("restart.service").unwrap();
    assert!(child.wait().await.unwrap().success());
}

fn local_executor_path() -> Option<String> {
    let path = std::env::current_dir()
        .ok()?
        .join("target/x86_64-unknown-linux-musl/debug/sysd-executor");
    path.exists().then(|| path.to_string_lossy().to_string())
}
