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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watch_socket_returns_when_no_fds_are_available() {
        let (tx, mut rx) = mpsc::channel(1);

        watch_socket(
            "empty.socket".to_string(),
            "empty.service".to_string(),
            Vec::new(),
            tx,
        )
        .await;

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn activation_messages_include_socket_and_service_names() {
        let (tx, mut rx) = mpsc::channel(1);

        send_activation_message(&tx, "api.socket", "api.service").await;

        let message = rx.recv().await.unwrap();
        assert_eq!(message.socket_name, "api.socket");
        assert_eq!(message.service_name, "api.service");
    }

    #[tokio::test]
    async fn activation_message_send_tolerates_closed_receiver() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        send_activation_message(&tx, "closed.socket", "closed.service").await;
    }

    #[tokio::test]
    async fn watch_socket_sends_activation_when_listener_becomes_readable() {
        use std::os::unix::io::AsRawFd;

        let socket_path = std::env::temp_dir().join(format!(
            "sysd-socket-watcher-{}-{}.sock",
            std::process::id(),
            socket_name_suffix()
        ));
        let _ = std::fs::remove_file(&socket_path);
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (tx, mut rx) = mpsc::channel(1);

        let watcher = tokio::spawn(watch_socket(
            "ready.socket".to_string(),
            "ready.service".to_string(),
            vec![listener.as_raw_fd()],
            tx,
        ));
        let _client = std::os::unix::net::UnixStream::connect(&socket_path).unwrap();

        let message = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        watcher.await.unwrap();
        let _ = std::fs::remove_file(&socket_path);

        assert_eq!(message.socket_name, "ready.socket");
        assert_eq!(message.service_name, "ready.service");
    }

    fn socket_name_suffix() -> usize {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static NEXT: AtomicUsize = AtomicUsize::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }
}
