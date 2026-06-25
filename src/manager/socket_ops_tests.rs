use super::*;
use crate::manager::{ActiveState, Manager, ManagerError, ServiceState, SubState};
use crate::units::{ListenType, Listener, Service, Socket, Unit};

fn service(name: &str, sockets: &[&str]) -> Service {
    let mut service = Service::new(name.to_string());
    service.service.sockets = sockets.iter().map(|name| (*name).to_string()).collect();
    service
}

fn socket(name: &str, configure: impl FnOnce(&mut Socket)) -> Socket {
    let mut socket = Socket::new(name.to_string());
    configure(&mut socket);
    socket
}

fn pipe_fds() -> [libc::c_int; 2] {
    let mut fds = [0; 2];
    let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(result, 0);
    fds
}

#[test]
fn netlink_address_parses_protocol_aliases_groups_and_errors() {
    assert_eq!(
        parse_netlink_protocol("route").unwrap(),
        libc::NETLINK_ROUTE
    );
    assert_eq!(
        parse_netlink_protocol("firewall").unwrap(),
        libc::NETLINK_NETFILTER
    );
    assert!(parse_netlink_protocol("missing").is_err());

    let (name, protocol, groups) = parse_netlink_address("audit 7").unwrap();
    assert_eq!(name, "audit");
    assert_eq!(protocol, libc::NETLINK_AUDIT);
    assert_eq!(groups, 7);

    let (_, _, groups) = parse_netlink_address("route").unwrap();
    assert_eq!(groups, 0);
    assert!(parse_netlink_address("").is_err());
    assert!(parse_netlink_address("route nope").is_err());
}

#[test]
fn configured_service_socket_mapping_returns_fds_and_fd_names() {
    let mut manager = Manager::new();
    manager.units.insert(
        "api.service".to_string(),
        Unit::Service(service("api.service", &["api.socket", "metrics.socket"])),
    );
    manager.units.insert(
        "api.socket".to_string(),
        Unit::Socket(socket("api.socket", |socket| {
            socket.socket.fd_name = Some("api-listener".to_string());
        })),
    );
    manager
        .socket_fds
        .insert("api.socket".to_string(), vec![10, 11]);

    assert_eq!(manager.get_socket_fds("api.service"), [10, 11]);
    assert_eq!(
        manager.get_socket_fd_names("api.service"),
        ["api-listener", "api-listener"]
    );
}

#[test]
fn reverse_socket_mapping_uses_socket_service_name_when_service_has_no_socket_list() {
    let mut manager = Manager::new();
    manager.units.insert(
        "api.socket".to_string(),
        Unit::Socket(socket("api.socket", |socket| {
            socket.socket.service = Some("worker.service".to_string());
        })),
    );
    manager
        .socket_fds
        .insert("api.socket".to_string(), vec![42]);

    assert_eq!(manager.get_socket_fds("worker.service"), [42]);
    assert_eq!(manager.get_socket_fd_names("worker.service"), ["api"]);
}

#[test]
fn configured_socket_mapping_reports_missing_fds_without_falling_back() {
    let mut manager = Manager::new();
    manager.units.insert(
        "api.service".to_string(),
        Unit::Service(service("api.service", &["missing.socket"])),
    );
    manager
        .socket_fds
        .insert("api.socket".to_string(), vec![7]);

    assert!(manager.get_socket_fds("api.service").is_empty());
    assert!(manager.get_socket_fd_names("api.service").is_empty());
}

#[test]
fn take_socket_activation_receiver_returns_receiver_once() {
    let mut manager = Manager::new();

    assert!(manager.take_socket_activation_rx().is_some());
    assert!(manager.take_socket_activation_rx().is_none());
}

#[tokio::test]
async fn stop_socket_requires_active_state_then_closes_fds_and_marks_stopped() {
    let mut manager = Manager::new();
    let socket = socket("api.socket", |_| {});

    assert!(matches!(
        manager.stop_socket("api.socket", &socket).await,
        Err(ManagerError::NotFound(name)) if name == "api.socket"
    ));

    manager
        .states
        .insert("api.socket".to_string(), ServiceState::new());
    assert!(matches!(
        manager.stop_socket("api.socket", &socket).await,
        Err(ManagerError::NotActive(name)) if name == "api.socket"
    ));

    let fds = pipe_fds();
    manager
        .states
        .get_mut("api.socket")
        .unwrap()
        .set_running(0);
    manager
        .socket_fds
        .insert("api.socket".to_string(), vec![fds[0]]);

    manager.stop_socket("api.socket", &socket).await.unwrap();

    let state = manager.states.get("api.socket").unwrap();
    assert_eq!(state.active, ActiveState::Inactive);
    assert_eq!(state.sub, SubState::Exited);
    assert!(!manager.socket_fds.contains_key("api.socket"));
    unsafe {
        libc::close(fds[1]);
    }
}

#[test]
fn listener_dispatch_rejects_invalid_netlink_and_creates_fifo() {
    let manager = Manager::new();
    let root = std::env::temp_dir().join(format!(
        "sysd-socket-ops-{}",
        std::process::id()
    ));
    let fifo_path = root.join("demo.fifo");
    let mut socket = socket("fifo.socket", |_| {});
    socket.socket.socket_mode = Some(0o600);
    let fifo_listener = Listener {
        address: fifo_path.to_string_lossy().to_string(),
        listen_type: ListenType::Fifo,
    };
    let netlink_listener = Listener {
        address: "unknown 1".to_string(),
        listen_type: ListenType::Netlink,
    };

    let fd = manager.create_listener(&fifo_listener, &socket).unwrap();
    unsafe {
        libc::close(fd);
    }
    assert!(fifo_path.exists());
    assert!(manager.create_listener(&netlink_listener, &socket).is_err());
    let _ = std::fs::remove_dir_all(root);
}
