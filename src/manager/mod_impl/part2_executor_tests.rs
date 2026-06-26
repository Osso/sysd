use super::*;
use crate::manager::state::ServiceState;
use crate::units::Service;

fn service(name: &str, configure: impl FnOnce(&mut Service)) -> Service {
    let mut service = Service::new(name.to_string());
    configure(&mut service);
    service
}

#[tokio::test]
async fn start_service_unit_tracks_real_spawned_executor_child() {
    let Some(executor) = local_executor_path() else {
        return;
    };
    let mut manager = Manager::new_user();
    manager.executor_path = executor;
    manager
        .states
        .insert("true.service".to_string(), ServiceState::new());
    let svc = service("true.service", |service| {
        service.service.exec_start = vec!["/bin/true".to_string()];
    });

    manager.start_service_unit("true.service", svc).await.unwrap();

    let pid = manager.states.get("true.service").unwrap().main_pid.unwrap();
    assert_eq!(
        manager.pid_to_service.get(&pid).map(String::as_str),
        Some("true.service")
    );
    let mut child = manager.processes.remove("true.service").unwrap();
    assert!(child.wait().await.unwrap().success());
}

#[tokio::test]
async fn start_oneshot_service_spawns_completion_task_with_real_executor() {
    let Some(executor) = local_executor_path() else {
        return;
    };
    let mut manager = Manager::new_user();
    manager.executor_path = executor;
    manager
        .states
        .insert("oneshot.service".to_string(), ServiceState::new());
    let svc = service("oneshot.service", |service| {
        service.service.service_type = ServiceType::Oneshot;
        service.service.exec_start = vec!["/bin/true".to_string(), "/bin/true".to_string()];
    });
    let options =
        manager.build_spawn_options(&svc, "oneshot.service", Vec::new(), Vec::new(), None, None);

    manager
        .start_oneshot_service("oneshot.service", &svc, options)
        .unwrap();

    let mut rx = manager.oneshot_completion_rx.take().unwrap();
    let completion = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(completion.service_name, "oneshot.service");
    assert_eq!(completion.cmd_idx, 0);
    assert_eq!(completion.total_cmds, 2);
    assert_eq!(completion.exit_code, Some(0));
    assert_eq!(completion.error, None);
}

fn local_executor_path() -> Option<String> {
    let path = std::env::current_dir()
        .ok()?
        .join("target/x86_64-unknown-linux-musl/debug/sysd-executor");
    path.exists().then(|| path.to_string_lossy().to_string())
}
