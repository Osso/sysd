// Socket activation watcher
//
// Monitors listening sockets and triggers service activation on connection.

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
    let Some(&fd) = fds.first() else {
        return;
    };

    // Wrap the FD in AsyncFd for async I/O
    let async_fd = match AsyncFd::new(fd) {
        Ok(afd) => afd,
        Err(e) => {
            log::error!("{}: failed to create AsyncFd: {}", socket_name, e);
            return;
        }
    };

    log::debug!("{}: watching fd {} for connections", socket_name, fd);
    if let Ok(mut guard) = wait_for_socket_readable(&async_fd, &socket_name).await {
        send_activation_message(&tx, &socket_name, &service_name).await;
        guard.clear_ready();
    }
}

async fn wait_for_socket_readable<'a>(
    async_fd: &'a AsyncFd<RawFd>,
    socket_name: &str,
) -> Result<tokio::io::unix::AsyncFdReadyGuard<'a, RawFd>, ()> {
    match async_fd.ready(Interest::READABLE).await {
        Ok(guard) => Ok(guard),
        Err(e) => {
            log::error!("{}: error waiting for socket: {}", socket_name, e);
            Err(())
        }
    }
}

async fn send_activation_message(
    tx: &mpsc::Sender<SocketActivation>,
    socket_name: &str,
    service_name: &str,
) {
    log::info!(
        "{}: connection pending, activating {}",
        socket_name,
        service_name
    );
    let message = SocketActivation {
        socket_name: socket_name.to_string(),
        service_name: service_name.to_string(),
    };
    if let Err(e) = tx.send(message).await {
        log::error!("{}: failed to send activation: {}", socket_name, e);
    }
}
