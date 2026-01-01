//! Socket unit operations
//!
//! Handles creation and management of listening sockets for socket activation.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, RawFd};

use tokio::sync::mpsc;

use crate::units::{ListenType, Socket};

use super::{socket_watcher, Manager, ManagerError};

impl Manager {
    /// Start a socket unit (create listening sockets)
    pub(super) async fn start_socket(
        &mut self,
        name: &str,
        socket: &Socket,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        log::info!("Starting socket {}", name);

        let mut fds = Vec::new();

        for listener in &socket.socket.listeners {
            match self.create_listener(listener, socket) {
                Ok(fd) => {
                    log::debug!(
                        "{}: created {:?} listener on {} (fd {})",
                        name,
                        listener.listen_type,
                        listener.address,
                        fd
                    );
                    fds.push(fd);
                }
                Err(e) => {
                    // Close already created sockets on failure
                    for fd in fds {
                        unsafe { libc::close(fd) };
                    }
                    if let Some(state) = self.states.get_mut(name) {
                        state.set_failed(format!("listener creation failed: {}", e));
                    }
                    return Err(ManagerError::Io(format!(
                        "Failed to create listener {}: {}",
                        listener.address, e
                    )));
                }
            }
        }

        // Store the FDs
        self.socket_fds.insert(name.to_string(), fds.clone());

        // Spawn socket watcher task for activation
        let service_name = socket.service_name();
        let socket_name = name.to_string();
        let tx = self.socket_activation_tx.clone();
        tokio::spawn(async move {
            socket_watcher::watch_socket(socket_name, service_name, fds, tx).await;
        });

        // Mark as active
        if let Some(state) = self.states.get_mut(name) {
            state.set_running(0);
        }

        log::info!("{} listening", name);
        Ok(())
    }

    /// Create a single listener socket
    fn create_listener(
        &self,
        listener: &crate::units::Listener,
        socket: &Socket,
    ) -> std::io::Result<RawFd> {
        use std::os::unix::net::UnixListener;

        match listener.listen_type {
            ListenType::Stream => {
                // Check if it's a Unix socket (path) or TCP (port number)
                if listener.address.starts_with('/') || listener.address.starts_with('@') {
                    // Unix socket
                    let path = if listener.address.starts_with('@') {
                        // Abstract socket - use null byte prefix
                        format!("\0{}", &listener.address[1..])
                    } else {
                        listener.address.clone()
                    };

                    // Remove existing socket file
                    if !listener.address.starts_with('@') {
                        let _ = std::fs::remove_file(&listener.address);

                        // Create parent directory if needed
                        if let Some(parent) = std::path::Path::new(&listener.address).parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                    }

                    if listener.address.starts_with('@') {
                        // Abstract socket - need to use libc directly
                        self.create_abstract_unix_socket(&path)
                    } else {
                        // Filesystem socket
                        let unix_listener = UnixListener::bind(&listener.address)?;

                        // Set socket mode if specified
                        if let Some(mode) = socket.socket.socket_mode {
                            let perms = std::fs::Permissions::from_mode(mode);
                            std::fs::set_permissions(&listener.address, perms)?;
                        }

                        let fd = unix_listener.as_raw_fd();
                        // Prevent FD from being closed when UnixListener drops
                        std::mem::forget(unix_listener);
                        Ok(fd)
                    }
                } else {
                    // TCP socket (port number or host:port)
                    self.create_tcp_socket(&listener.address)
                }
            }
            ListenType::Datagram => {
                // UDP socket
                if listener.address.starts_with('/') {
                    // Unix datagram socket
                    self.create_unix_dgram_socket(&listener.address, socket)
                } else {
                    // UDP network socket
                    self.create_udp_socket(&listener.address)
                }
            }
            ListenType::Fifo => self.create_fifo(&listener.address, socket),
            ListenType::Netlink => {
                // Netlink sockets are complex - stub for now
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "Netlink sockets not yet implemented",
                ))
            }
        }
    }

    fn create_abstract_unix_socket(&self, addr: &str) -> std::io::Result<RawFd> {
        use std::mem::size_of;

        unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }

            // Set SO_REUSEADDR
            let optval: libc::c_int = 1;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &optval as *const _ as *const libc::c_void,
                size_of::<libc::c_int>() as libc::socklen_t,
            );

            let mut sockaddr: libc::sockaddr_un = std::mem::zeroed();
            sockaddr.sun_family = libc::AF_UNIX as u16;

            // Copy address including the null byte for abstract sockets
            let bytes = addr.as_bytes();
            let len = std::cmp::min(bytes.len(), sockaddr.sun_path.len());
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                sockaddr.sun_path.as_mut_ptr() as *mut u8,
                len,
            );

            let addr_len = (size_of::<libc::sa_family_t>() + len) as libc::socklen_t;

            if libc::bind(fd, &sockaddr as *const _ as *const libc::sockaddr, addr_len) < 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }

            if libc::listen(fd, 128) < 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }

            Ok(fd)
        }
    }

    fn create_tcp_socket(&self, addr: &str) -> std::io::Result<RawFd> {
        use std::net::TcpListener;

        // Handle port-only or host:port
        let bind_addr = if addr.contains(':') {
            addr.to_string()
        } else {
            format!("0.0.0.0:{}", addr)
        };

        let listener = TcpListener::bind(&bind_addr)?;
        let fd = listener.as_raw_fd();
        std::mem::forget(listener);
        Ok(fd)
    }

    fn create_udp_socket(&self, addr: &str) -> std::io::Result<RawFd> {
        use std::net::UdpSocket;

        let bind_addr = if addr.contains(':') {
            addr.to_string()
        } else {
            format!("0.0.0.0:{}", addr)
        };

        let socket = UdpSocket::bind(&bind_addr)?;
        let fd = socket.as_raw_fd();
        std::mem::forget(socket);
        Ok(fd)
    }

    fn create_unix_dgram_socket(&self, path: &str, socket: &Socket) -> std::io::Result<RawFd> {
        use std::os::unix::net::UnixDatagram;

        // Remove existing socket
        let _ = std::fs::remove_file(path);

        // Create parent directory
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let sock = UnixDatagram::bind(path)?;

        // Set permissions
        if let Some(mode) = socket.socket.socket_mode {
            let perms = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(path, perms)?;
        }

        let fd = sock.as_raw_fd();
        std::mem::forget(sock);
        Ok(fd)
    }

    fn create_fifo(&self, path: &str, socket: &Socket) -> std::io::Result<RawFd> {
        use std::ffi::CString;

        // Remove existing FIFO
        let _ = std::fs::remove_file(path);

        // Create parent directory
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let mode = socket.socket.socket_mode.unwrap_or(0o644);
        let c_path = CString::new(path).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid path")
        })?;

        unsafe {
            if libc::mkfifo(c_path.as_ptr(), mode) < 0 {
                return Err(std::io::Error::last_os_error());
            }

            let fd = libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(fd)
        }
    }

    /// Stop a socket unit (close listening sockets)
    pub(super) async fn stop_socket(
        &mut self,
        name: &str,
        socket: &Socket,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        log::info!("Stopping socket {}", name);

        // Close all socket FDs
        if let Some(fds) = self.socket_fds.remove(name) {
            for fd in fds {
                unsafe { libc::close(fd) };
            }
        }

        // Remove socket files if RemoveOnStop=yes
        if socket.socket.remove_on_stop {
            for listener in &socket.socket.listeners {
                if listener.address.starts_with('/') {
                    let _ = std::fs::remove_file(&listener.address);
                }
            }
        }

        if let Some(state) = self.states.get_mut(name) {
            state.set_stopped(0);
        }

        log::info!("{} stopped", name);
        Ok(())
    }

    /// Get listening socket FDs for a service (for socket activation)
    pub fn get_socket_fds(&self, service_name: &str) -> Vec<RawFd> {
        // Check if service has explicit Sockets= directive
        if let Some(service) = self.units.get(service_name).and_then(|u| u.as_service()) {
            if !service.service.sockets.is_empty() {
                // Collect FDs from all named sockets
                let mut fds = Vec::new();
                for socket_name in &service.service.sockets {
                    if let Some(socket_fds) = self.socket_fds.get(socket_name) {
                        fds.extend(socket_fds.iter().copied());
                    }
                }
                if !fds.is_empty() {
                    return fds;
                }
            }
        }

        // Fall back to name matching: find socket unit that activates this service
        for (socket_name, unit) in &self.units {
            if let Some(socket) = unit.as_socket() {
                if socket.service_name() == service_name {
                    if let Some(fds) = self.socket_fds.get(socket_name) {
                        return fds.clone();
                    }
                }
            }
        }
        Vec::new()
    }

    /// Take the socket activation receiver (for use in event loops)
    pub fn take_socket_activation_rx(
        &mut self,
    ) -> Option<mpsc::Receiver<socket_watcher::SocketActivation>> {
        self.socket_activation_rx.take()
    }

    /// Process a socket activation message (start the associated service)
    pub async fn handle_socket_activation(
        &mut self,
        activation: socket_watcher::SocketActivation,
    ) -> Result<(), ManagerError> {
        log::info!(
            "Socket activation: {} triggered by {}",
            activation.service_name,
            activation.socket_name
        );

        // Check if service is already running
        if let Some(state) = self.states.get(&activation.service_name) {
            if state.is_active() {
                log::debug!(
                    "{} already running, skipping activation",
                    activation.service_name
                );
                return Ok(());
            }
        }

        // Start the service
        self.start(&activation.service_name).await
    }
}
