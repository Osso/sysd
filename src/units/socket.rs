//! Socket unit type for socket activation
//!
//! Parses .socket unit files and manages socket activation.

use super::{InstallSection, UnitSection};

/// Type of listener
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ListenType {
    /// TCP stream socket (ListenStream=)
    #[default]
    Stream,
    /// UDP datagram socket (ListenDatagram=)
    Datagram,
    /// Named pipe / FIFO (ListenFIFO=)
    Fifo,
    /// Netlink socket (ListenNetlink=)
    Netlink,
}

/// A single listener configuration
#[derive(Debug, Clone)]
pub struct Listener {
    /// The address/path to listen on
    pub address: String,
    /// Type of listener
    pub listen_type: ListenType,
}

/// Socket section configuration
#[derive(Debug, Clone, Default)]
pub struct SocketSection {
    /// Listeners (can have multiple)
    pub listeners: Vec<Listener>,

    /// Accept mode: spawn new instance per connection (Accept=yes)
    /// If false, pass all connections to single service instance
    pub accept: bool,

    /// Service unit to activate (default: same name with .service)
    pub service: Option<String>,

    /// Socket file permissions (SocketMode=)
    pub socket_mode: Option<u32>,

    /// Socket owner user (SocketUser=)
    pub socket_user: Option<String>,

    /// Socket owner group (SocketGroup=)
    pub socket_group: Option<String>,

    /// Name for the file descriptor (FileDescriptorName=)
    pub fd_name: Option<String>,

    /// Remove socket file on stop (RemoveOnStop=)
    pub remove_on_stop: bool,

    /// Max connections per source IP (MaxConnectionsPerSource=)
    pub max_connections_per_source: Option<u32>,

    /// Receive buffer size (ReceiveBuffer=)
    pub receive_buffer: Option<u64>,

    /// Send buffer size (SendBuffer=)
    pub send_buffer: Option<u64>,

    /// Pass credentials via SO_PASSCRED (PassCredentials=)
    pub pass_credentials: bool,

    /// Pass security context (PassSecurity=)
    pub pass_security: bool,

    /// Symlinks to create (Symlinks=)
    pub symlinks: Vec<String>,

    /// Defer service activation (DeferTrigger=)
    pub defer_trigger: bool,
}

/// Represents a parsed .socket unit file
#[derive(Debug, Clone)]
pub struct Socket {
    /// Unit name (e.g., "dbus.socket")
    pub name: String,
    /// [Unit] section
    pub unit: UnitSection,
    /// [Socket] section
    pub socket: SocketSection,
    /// [Install] section
    pub install: InstallSection,
}

impl Socket {
    pub fn new(name: String) -> Self {
        Self {
            name,
            unit: UnitSection::default(),
            socket: SocketSection::default(),
            install: InstallSection::default(),
        }
    }

    /// Update the socket name (used for template instantiation)
    pub fn set_name(&mut self, new_name: String) {
        self.name = new_name;
    }

    /// Get the service name this socket activates
    pub fn service_name(&self) -> String {
        if let Some(ref svc) = self.socket.service {
            svc.clone()
        } else {
            // Default: same name with .service extension
            self.name.replace(".socket", ".service")
        }
    }

    /// Check if this is an Accept= socket (spawns per-connection instances)
    pub fn is_accept_socket(&self) -> bool {
        self.socket.accept
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_default_socket_state() {
        let socket = Socket::new("dbus.socket".to_string());

        assert_eq!(socket.name, "dbus.socket");
        assert_eq!(socket.service_name(), "dbus.service");
        assert!(!socket.is_accept_socket());
        assert!(socket.socket.listeners.is_empty());
        assert!(!socket.socket.remove_on_stop);
        assert_eq!(socket.socket.socket_mode, None);
    }

    #[test]
    fn explicit_service_accept_and_listener_fields_are_reported() {
        let mut socket = Socket::new("api.socket".to_string());
        socket.socket.service = Some("api-worker.service".to_string());
        socket.socket.accept = true;
        socket.socket.listeners.push(Listener {
            address: "/run/api.sock".to_string(),
            listen_type: ListenType::Stream,
        });
        socket.socket.socket_mode = Some(0o660);
        socket.socket.socket_user = Some("api".to_string());
        socket.socket.socket_group = Some("api".to_string());
        socket.socket.fd_name = Some("api".to_string());
        socket.socket.remove_on_stop = true;
        socket.socket.max_connections_per_source = Some(10);
        socket.socket.receive_buffer = Some(4096);
        socket.socket.send_buffer = Some(8192);
        socket.socket.pass_credentials = true;
        socket.socket.pass_security = true;
        socket.socket.symlinks = vec!["/run/api-link.sock".to_string()];
        socket.socket.defer_trigger = true;

        assert_eq!(socket.service_name(), "api-worker.service");
        assert!(socket.is_accept_socket());
        assert_eq!(socket.socket.listeners[0].listen_type, ListenType::Stream);
        assert_eq!(socket.socket.socket_mode, Some(0o660));
        assert_eq!(socket.socket.max_connections_per_source, Some(10));
        assert!(socket.socket.pass_credentials);
        assert!(socket.socket.pass_security);
        assert_eq!(socket.socket.symlinks, vec!["/run/api-link.sock"]);
        assert!(socket.socket.defer_trigger);
    }

    #[test]
    fn set_name_updates_socket_name_and_default_service() {
        let mut socket = Socket::new("old.socket".to_string());
        socket.set_name("new.socket".to_string());

        assert_eq!(socket.name, "new.socket");
        assert_eq!(socket.service_name(), "new.service");
    }
}
