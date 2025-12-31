//! Socket activation watcher
//!
//! Monitors listening sockets and triggers service activation on connection.

use std::os::unix::io::RawFd;
use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tokio::sync::mpsc;

/// Message sent when a socket is ready for activation
#[derive(Debug)]
pub struct SocketActivation {
    /// Name of the socket unit
    pub socket_name: String,
    /// Name of the service to start
    pub service_name: String,
}

/// Watch a socket for incoming connections and send activation message
pub async fn watch_socket(
    socket_name: String,
    service_name: String,
    fds: Vec<RawFd>,
    tx: mpsc::Sender<SocketActivation>,
) {
    if fds.is_empty() {
        return;
    }

    // Use the first FD for simplicity (most sockets have one listener)
    let fd = fds[0];

    // Wrap the FD in AsyncFd for async I/O
    let async_fd = match AsyncFd::new(fd) {
        Ok(afd) => afd,
        Err(e) => {
            log::error!("{}: failed to create AsyncFd: {}", socket_name, e);
            return;
        }
    };

    log::debug!("{}: watching fd {} for connections", socket_name, fd);

    // Wait for the socket to become readable (connection pending)
    loop {
        match async_fd.ready(Interest::READABLE).await {
            Ok(mut guard) => {
                // Socket is readable - trigger activation
                log::info!(
                    "{}: connection pending, activating {}",
                    socket_name,
                    service_name
                );

                // Send activation message
                if let Err(e) = tx
                    .send(SocketActivation {
                        socket_name: socket_name.clone(),
                        service_name: service_name.clone(),
                    })
                    .await
                {
                    log::error!("{}: failed to send activation: {}", socket_name, e);
                }

                // Clear readiness so we wait again
                guard.clear_ready();

                // After first activation, stop watching
                // The service now owns the socket and handles connections
                break;
            }
            Err(e) => {
                log::error!("{}: error waiting for socket: {}", socket_name, e);
                break;
            }
        }
    }
}

