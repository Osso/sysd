use super::*;
use crate::manager::{ActiveState, Manager, ManagerError, ServiceState, SubState};
use crate::units::{ListenType, Listener, Service, Socket, Unit};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempRoot(std::path::PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(label: &str) -> TempRoot {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sysd-socket-ops-{label}-{}-{counter}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempRoot(dir)
}

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

#[test]
fn listener_dispatch_creates_local_stream_datagram_and_abstract_sockets() {
    let manager = Manager::new();
    let root = temp_dir("listeners");
    let stream_path = root.0.join("stream.sock");
    let dgram_path = root.0.join("dgram.sock");
    let abstract_name = format!("@sysd-test-{}-stream", std::process::id());
    let mut socket = socket("local.socket", |_| {});
    socket.socket.socket_mode = Some(0o640);

    let tcp_fd = manager
        .create_listener(
            &Listener {
                address: "127.0.0.1:0".to_string(),
                listen_type: ListenType::Stream,
            },
            &socket,
        )
        .unwrap();
    let udp_fd = manager
        .create_listener(
            &Listener {
                address: "127.0.0.1:0".to_string(),
                listen_type: ListenType::Datagram,
            },
            &socket,
        )
        .unwrap();
    let unix_stream_fd = manager
        .create_listener(
            &Listener {
                address: stream_path.to_string_lossy().to_string(),
                listen_type: ListenType::Stream,
            },
            &socket,
        )
        .unwrap();
    let unix_dgram_fd = manager
        .create_listener(
            &Listener {
                address: dgram_path.to_string_lossy().to_string(),
                listen_type: ListenType::Datagram,
            },
            &socket,
        )
        .unwrap();
    let abstract_fd = manager
        .create_listener(
            &Listener {
                address: abstract_name,
                listen_type: ListenType::Stream,
            },
            &socket,
        )
        .unwrap();

    assert_eq!(
        std::fs::metadata(&stream_path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        std::fs::metadata(&dgram_path).unwrap().permissions().mode() & 0o777,
        0o640
    );

    for fd in [tcp_fd, udp_fd, unix_stream_fd, unix_dgram_fd, abstract_fd] {
        unsafe { libc::close(fd) };
    }
}

#[test]
fn listener_dispatch_accepts_port_only_tcp_udp_and_netlink_route() {
    let manager = Manager::new();
    let socket = socket("mixed.socket", |_| {});

    let tcp_fd = manager
        .create_listener(
            &Listener {
                address: "0".to_string(),
                listen_type: ListenType::Stream,
            },
            &socket,
        )
        .unwrap();
    let udp_fd = manager
        .create_listener(
            &Listener {
                address: "0".to_string(),
                listen_type: ListenType::Datagram,
            },
            &socket,
        )
        .unwrap();
    let netlink_fd = manager
        .create_listener(
            &Listener {
                address: "route 0".to_string(),
                listen_type: ListenType::Netlink,
            },
            &socket,
        )
        .unwrap();

    for fd in [tcp_fd, udp_fd, netlink_fd] {
        unsafe { libc::close(fd) };
    }
}

#[tokio::test]
async fn start_socket_requires_state_and_marks_failed_on_listener_error() {
    let mut manager = Manager::new();
    let bad_socket = socket("bad.socket", |socket| {
        socket.socket.listeners.push(Listener {
            address: "missing-protocol 1".to_string(),
            listen_type: ListenType::Netlink,
        });
    });

    assert!(matches!(
        manager.start_socket("bad.socket", &bad_socket).await,
        Err(ManagerError::NotFound(name)) if name == "bad.socket"
    ));

    manager
        .states
        .insert("bad.socket".to_string(), ServiceState::new());
    let err = manager.start_socket("bad.socket", &bad_socket).await.unwrap_err();

    assert!(matches!(err, ManagerError::Io(message) if message.contains("missing-protocol")));
    let state = manager.states.get("bad.socket").unwrap();
    assert_eq!(state.active, ActiveState::Failed);
    assert_eq!(state.sub, SubState::Failed);
    assert!(state.error.as_deref().unwrap().contains("listener creation failed"));
}

#[tokio::test]
async fn start_and_stop_socket_store_fds_mark_state_and_remove_socket_file() {
    let root = temp_dir("start-stop");
    let socket_path = root.0.join("api.sock");
    let mut manager = Manager::new();
    let socket = socket("api.socket", |socket| {
        socket.socket.remove_on_stop = true;
        socket.socket.listeners.push(Listener {
            address: socket_path.to_string_lossy().to_string(),
            listen_type: ListenType::Stream,
        });
    });
    manager
        .states
        .insert("api.socket".to_string(), ServiceState::new());

    manager.start_socket("api.socket", &socket).await.unwrap();

    assert!(socket_path.exists());
    assert_eq!(manager.socket_fds.get("api.socket").unwrap().len(), 1);
    assert!(manager.states.get("api.socket").unwrap().is_active());
    assert!(matches!(
        manager.start_socket("api.socket", &socket).await,
        Err(ManagerError::AlreadyActive(name)) if name == "api.socket"
    ));

    manager.stop_socket("api.socket", &socket).await.unwrap();

    assert!(!socket_path.exists());
    assert!(!manager.socket_fds.contains_key("api.socket"));
    assert_eq!(
        manager.states.get("api.socket").unwrap().active,
        ActiveState::Inactive
    );
}

#[tokio::test]
async fn handle_socket_activation_skips_active_services_and_reports_missing_services() {
    let mut manager = Manager::new();
    manager.units.insert(
        "ready.service".to_string(),
        Unit::Service(service("ready.service", &[])),
    );
    manager
        .states
        .insert("ready.service".to_string(), ServiceState::new());
    manager
        .states
        .get_mut("ready.service")
        .unwrap()
        .set_running(99);

    manager
        .handle_socket_activation(socket_watcher::SocketActivation {
            socket_name: "ready.socket".to_string(),
            service_name: "ready.service".to_string(),
        })
        .await
        .unwrap();

    let err = manager
        .handle_socket_activation(socket_watcher::SocketActivation {
            socket_name: "missing.socket".to_string(),
            service_name: "missing.service".to_string(),
        })
        .await
        .unwrap_err();

    assert!(matches!(err, ManagerError::NotFound(name) if name == "missing.service"));
}

#[test]
fn fd_names_fall_back_to_socket_names_for_reverse_mapping_without_fd_name() {
    let mut manager = Manager::new();
    manager.units.insert(
        "orphan.socket".to_string(),
        Unit::Socket(socket("orphan.socket", |_| {})),
    );
    manager
        .socket_fds
        .insert("orphan.socket".to_string(), vec![3, 4]);

    assert_eq!(manager.get_socket_fds("orphan.service"), [3, 4]);
    assert_eq!(
        manager.get_socket_fd_names("orphan.service"),
        ["orphan", "orphan"]
    );
}

#[test]
fn socket_mapping_skips_empty_configured_sockets_nonmatching_sockets_and_missing_fds() {
    let mut manager = Manager::new();
    manager.units.insert(
        "empty.service".to_string(),
        Unit::Service(service("empty.service", &[])),
    );
    manager.units.insert(
        "other.socket".to_string(),
        Unit::Socket(socket("other.socket", |socket| {
            socket.socket.service = Some("other.service".to_string());
        })),
    );
    manager.units.insert(
        "nofds.socket".to_string(),
        Unit::Socket(socket("nofds.socket", |socket| {
            socket.socket.service = Some("empty.service".to_string());
        })),
    );

    assert!(manager.get_socket_fds("empty.service").is_empty());
    assert!(manager.get_socket_fd_names("empty.service").is_empty());
}
