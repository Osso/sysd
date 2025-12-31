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
