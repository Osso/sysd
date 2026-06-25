// sd_notify protocol implementation
//
// Listens on NOTIFY_SOCKET for service readiness notifications.
// Protocol: https://www.freedesktop.org/software/systemd/man/sd_notify.html

use std::collections::HashMap;
use std::io::IoSliceMut;
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

use nix::sys::socket::{recvmsg, setsockopt, sockopt, ControlMessageOwned, MsgFlags};

/// Messages from services via sd_notify
#[derive(Debug, Clone)]
pub struct NotifyMessage {
    /// Service that sent the message (matched by socket credentials)
    pub pid: u32,
    /// Parsed key-value pairs from the message
    pub fields: HashMap<String, String>,
    /// M19: File descriptors passed via SCM_RIGHTS (for FDSTORE=1)
    pub fds: Vec<RawFd>,
}

impl NotifyMessage {
    /// Check if this is a READY=1 notification
    pub fn is_ready(&self) -> bool {
        self.fields.get("READY").map(|v| v == "1").unwrap_or(false)
    }

    /// Check if this is a STOPPING=1 notification
    pub fn is_stopping(&self) -> bool {
        self.fields
            .get("STOPPING")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    /// Check if this is a WATCHDOG=1 ping
    pub fn is_watchdog(&self) -> bool {
        self.fields
            .get("WATCHDOG")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    /// M19: Check if this is a FDSTORE=1 notification (file descriptor storage)
    pub fn is_fdstore(&self) -> bool {
        self.fields
            .get("FDSTORE")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    /// M19: Check if this is a FDSTOREREMOVE=1 notification (remove stored FD)
    pub fn is_fdstoreremove(&self) -> bool {
        self.fields
            .get("FDSTOREREMOVE")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    /// M19: Get FDNAME if present (names the stored FD)
    pub fn fdname(&self) -> Option<&str> {
        self.fields.get("FDNAME").map(|s| s.as_str())
    }

    /// Get STATUS message if present
    pub fn status(&self) -> Option<&str> {
        self.fields.get("STATUS").map(|s| s.as_str())
    }

    /// Get MAINPID if present
    pub fn main_pid(&self) -> Option<u32> {
        self.fields.get("MAINPID").and_then(|s| s.parse().ok())
    }
}

/// Async notify socket using tokio
pub struct AsyncNotifyListener {
    /// Socket kept alive to maintain binding (receiver task has its own Arc)
    _socket: Arc<tokio::net::UnixDatagram>,
    socket_path: PathBuf,
}

impl AsyncNotifyListener {
    /// Create a new async notify socket and spawn the receiver task
    /// Returns the listener (for socket_path) and a channel receiver
    pub fn new(socket_path: &Path) -> std::io::Result<(Self, mpsc::Receiver<NotifyMessage>)> {
        prepare_socket_path(socket_path)?;
        let socket = Arc::new(create_notify_socket(socket_path)?);
        let (tx, rx) = mpsc::channel(64);
        spawn_notify_receiver(Arc::clone(&socket), tx);

        Ok((
            Self {
                _socket: socket,
                socket_path: socket_path.to_path_buf(),
            },
            rx,
        ))
    }

    /// Get the socket path for passing to services
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

fn prepare_socket_path(socket_path: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(socket_path);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn create_notify_socket(socket_path: &Path) -> std::io::Result<tokio::net::UnixDatagram> {
    let socket = tokio::net::UnixDatagram::bind(socket_path)?;
    setsockopt(&socket.as_fd(), sockopt::PassCred, &true)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    set_notify_socket_permissions(socket_path)?;
    Ok(socket)
}

fn set_notify_socket_permissions(socket_path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o777))
}

fn spawn_notify_receiver(socket: Arc<tokio::net::UnixDatagram>, tx: mpsc::Sender<NotifyMessage>) {
    tokio::spawn(async move {
        receive_notify_messages(socket, tx).await;
    });
}

async fn receive_notify_messages(
    socket: Arc<tokio::net::UnixDatagram>,
    tx: mpsc::Sender<NotifyMessage>,
) {
    let mut buf = [0u8; 4096];
    let mut cmsg_buf = vec![0u8; 1024];
    loop {
        let recv = receive_notify_packet(&socket, &mut buf, &mut cmsg_buf).await;
        let packet = match recv {
            PacketState::Received(packet) => packet,
            PacketState::Retry => continue,
            PacketState::Closed => break,
        };
        if !forward_notify_packet(&tx, packet, &buf).await {
            break;
        }
    }
}

enum PacketState {
    Received((usize, u32, Vec<RawFd>)),
    Retry,
    Closed,
}

async fn receive_notify_packet(
    socket: &tokio::net::UnixDatagram,
    buf: &mut [u8],
    cmsg_buf: &mut Vec<u8>,
) -> PacketState {
    if socket.readable().await.is_err() {
        return PacketState::Closed;
    }
    let result = socket.try_io(tokio::io::Interest::READABLE, || {
        recv_with_creds(socket.as_fd(), buf, cmsg_buf)
    });
    match result {
        Ok(packet) => PacketState::Received(packet),
        Err(e) if should_retry_notify_read(&e) => PacketState::Retry,
        Err(e) => {
            log::error!("Notify socket error: {}", e);
            PacketState::Closed
        }
    }
}

fn should_retry_notify_read(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == Some(libc::EAGAIN)
        || error.raw_os_error() == Some(libc::EWOULDBLOCK)
}

async fn forward_notify_packet(
    tx: &mpsc::Sender<NotifyMessage>,
    (len, pid, fds): (usize, u32, Vec<RawFd>),
    buf: &[u8],
) -> bool {
    let Ok(msg) = std::str::from_utf8(&buf[..len]) else {
        close_received_fds(fds);
        return true;
    };
    log::info!("NOTIFY_RAW: pid={}, len={}, msg={:?}", pid, len, msg.trim());
    let notify_msg = parse_notify_message(msg, pid, fds);
    tx.send(notify_msg).await.is_ok()
}

fn close_received_fds(fds: Vec<RawFd>) {
    for fd in fds {
        unsafe { libc::close(fd) };
    }
}

impl Drop for AsyncNotifyListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Receive a message from the socket with sender credentials and optional FDs
fn recv_with_creds(
    fd: BorrowedFd<'_>,
    buf: &mut [u8],
    cmsg_buf: &mut Vec<u8>,
) -> std::io::Result<(usize, u32, Vec<RawFd>)> {
    let mut iov = [IoSliceMut::new(buf)];

    let msg = recvmsg::<()>(fd.as_raw_fd(), &mut iov, Some(cmsg_buf), MsgFlags::empty()).map_err(
        |e| {
            // Convert nix::Errno to std::io::Error preserving the raw OS error code
            // This is critical for EAGAIN/EWOULDBLOCK handling in the caller
            std::io::Error::from_raw_os_error(e as i32)
        },
    )?;

    // Extract PID from SCM_CREDENTIALS and FDs from SCM_RIGHTS
    let mut sender_pid = 0u32;
    let mut received_fds = Vec::new();

    if let Ok(cmsgs) = msg.cmsgs() {
        for cmsg in cmsgs {
            match cmsg {
                ControlMessageOwned::ScmCredentials(creds) => {
                    sender_pid = creds.pid() as u32;
                }
                ControlMessageOwned::ScmRights(fds) => {
                    // M19: File descriptors for FDSTORE
                    received_fds.extend(fds);
                }
                _ => {}
            }
        }
    }

    Ok((msg.bytes, sender_pid, received_fds))
}

/// Parse a notify message into key-value pairs
fn parse_notify_message(msg: &str, pid: u32, fds: Vec<RawFd>) -> NotifyMessage {
    let mut fields = HashMap::new();

    for line in msg.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.to_string(), value.to_string());
        }
    }

    NotifyMessage { pid, fields, fds }
}

/// Default socket path
pub const NOTIFY_SOCKET_PATH: &str = "/run/sysd/notify";

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempRoot(PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(label: &str) -> TempRoot {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sysd-notify-{label}-{}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempRoot(dir)
    }

    #[test]
    fn test_parse_notify_message() {
        let msg = parse_notify_message("READY=1\nSTATUS=Running\n", 1234, vec![]);
        assert!(msg.is_ready());
        assert_eq!(msg.status(), Some("Running"));
        assert_eq!(msg.pid, 1234);
        assert!(msg.fds.is_empty());
    }

    #[test]
    fn test_parse_stopping() {
        let msg = parse_notify_message("STOPPING=1", 5678, vec![]);
        assert!(msg.is_stopping());
        assert!(!msg.is_ready());
    }

    #[test]
    fn test_parse_mainpid() {
        let msg = parse_notify_message("MAINPID=9999\nREADY=1", 1000, vec![]);
        assert_eq!(msg.main_pid(), Some(9999));
        assert!(msg.is_ready());
    }

    #[test]
    fn test_parse_fdstore() {
        let msg = parse_notify_message("FDSTORE=1\nFDNAME=myfd", 2000, vec![5, 6]);
        assert!(msg.is_fdstore());
        assert_eq!(msg.fdname(), Some("myfd"));
        assert_eq!(msg.fds.len(), 2);
    }

    #[test]
    fn test_parse_fdstoreremove() {
        let msg = parse_notify_message("FDSTOREREMOVE=1\nFDNAME=myfd", 2000, vec![]);
        assert!(msg.is_fdstoreremove());
        assert_eq!(msg.fdname(), Some("myfd"));
    }

    #[test]
    fn notify_message_helpers_reject_non_one_values_and_bad_main_pid() {
        let msg = parse_notify_message(
            "READY=0\nSTOPPING=no\nWATCHDOG=false\nFDSTORE=2\nFDSTOREREMOVE=0\nMAINPID=oops\nIGNORED\nKEY=value=with=equals",
            4321,
            vec![],
        );

        assert!(!msg.is_ready());
        assert!(!msg.is_stopping());
        assert!(!msg.is_watchdog());
        assert!(!msg.is_fdstore());
        assert!(!msg.is_fdstoreremove());
        assert_eq!(msg.main_pid(), None);
        assert_eq!(msg.status(), None);
        assert_eq!(msg.fields.get("KEY").unwrap(), "value=with=equals");
    }

    #[tokio::test]
    async fn socket_path_helpers_remove_stale_socket_and_set_world_permissions() {
        let root = temp_dir("path");
        let socket_path = root.0.join("nested/notify.sock");
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        std::fs::write(&socket_path, "stale").unwrap();

        prepare_socket_path(&socket_path).unwrap();
        assert!(!socket_path.exists());

        let socket = create_notify_socket(&socket_path).unwrap();

        assert!(socket_path.exists());
        assert_eq!(
            std::fs::metadata(&socket_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o777
        );
        drop(socket);
    }

    #[tokio::test]
    async fn async_listener_receives_notify_message_and_drop_removes_socket() {
        let root = temp_dir("listener");
        let socket_path = root.0.join("notify.sock");

        let (listener, mut rx) = AsyncNotifyListener::new(&socket_path).unwrap();
        assert_eq!(listener.socket_path(), socket_path.as_path());

        let sender = tokio::net::UnixDatagram::unbound().unwrap();
        sender
            .send_to(b"READY=1\nWATCHDOG=1\nSTATUS=booted\nMAINPID=42", &socket_path)
            .await
            .unwrap();

        let msg = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(msg.is_ready());
        assert!(msg.is_watchdog());
        assert_eq!(msg.status(), Some("booted"));
        assert_eq!(msg.main_pid(), Some(42));
        assert_eq!(msg.pid, std::process::id());

        drop(listener);
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn forward_notify_packet_sends_valid_utf8_and_stops_when_receiver_closed() {
        let (tx, mut rx) = mpsc::channel(1);
        let buf = b"READY=1\nSTATUS=ok";

        assert!(forward_notify_packet(&tx, (buf.len(), 77, Vec::new()), buf).await);
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.pid, 77);
        assert!(msg.is_ready());
        assert_eq!(msg.status(), Some("ok"));

        drop(rx);
        assert!(!forward_notify_packet(&tx, (buf.len(), 77, Vec::new()), buf).await);
    }

    #[tokio::test]
    async fn forward_notify_packet_closes_fds_for_invalid_utf8() {
        let (tx, _rx) = mpsc::channel(1);
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let invalid = [0xff, 0xfe];

        assert!(forward_notify_packet(&tx, (invalid.len(), 1, vec![fds[0]]), &invalid).await);

        let mut byte = [0u8; 1];
        let read_result = unsafe { libc::read(fds[0], byte.as_mut_ptr().cast(), 1) };
        assert_eq!(read_result, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        unsafe { libc::close(fds[1]) };
    }

    #[test]
    fn retry_predicate_accepts_would_block_eagain_and_ewouldblock_only() {
        assert!(should_retry_notify_read(&std::io::Error::from(
            std::io::ErrorKind::WouldBlock
        )));
        assert!(should_retry_notify_read(
            &std::io::Error::from_raw_os_error(libc::EAGAIN)
        ));
        assert!(should_retry_notify_read(
            &std::io::Error::from_raw_os_error(libc::EWOULDBLOCK)
        ));
        assert!(!should_retry_notify_read(&std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused
        )));
    }
}
