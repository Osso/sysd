//! sd_notify protocol implementation
//!
//! Listens on NOTIFY_SOCKET for service readiness notifications.
//! Protocol: https://www.freedesktop.org/software/systemd/man/sd_notify.html

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
    #[allow(dead_code)]
    socket: Arc<tokio::net::UnixDatagram>,
    socket_path: PathBuf,
}

impl AsyncNotifyListener {
    /// Create a new async notify socket and spawn the receiver task
    /// Returns the listener (for socket_path) and a channel receiver
    pub fn new(socket_path: &Path) -> std::io::Result<(Self, mpsc::Receiver<NotifyMessage>)> {
        // Remove existing socket if present
        let _ = std::fs::remove_file(socket_path);

        // Create parent directory if needed
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Create the socket
        let socket = tokio::net::UnixDatagram::bind(socket_path)?;

        // Enable SO_PASSCRED to receive sender credentials
        setsockopt(&socket.as_fd(), sockopt::PassCred, &true)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        // Make it world-writable so services can send to it
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o777))?;

        let socket = Arc::new(socket);
        let socket_clone = Arc::clone(&socket);

        // Spawn receiver task
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            // Larger buffer for control messages to accommodate SCM_RIGHTS FDs
            let mut cmsg_buf = vec![0u8; 1024];

            loop {
                // Wait for socket to be readable
                if socket_clone.readable().await.is_err() {
                    break;
                }

                // Try to receive with credentials and optional FDs using recvmsg
                let result = socket_clone.try_io(tokio::io::Interest::READABLE, || {
                    recv_with_creds(socket_clone.as_fd(), &mut buf, &mut cmsg_buf)
                });

                match result {
                    Ok((len, pid, fds)) => {
                        if let Ok(msg) = std::str::from_utf8(&buf[..len]) {
                            let notify_msg = parse_notify_message(msg, pid, fds);
                            if tx.send(notify_msg).await.is_err() {
                                break; // Channel closed
                            }
                        } else {
                            // Close any received FDs if we can't parse the message
                            for fd in fds {
                                unsafe { libc::close(fd) };
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Socket not ready, continue to wait
                        continue;
                    }
                    Err(e) => {
                        log::error!("Notify socket error: {}", e);
                        break;
                    }
                }
            }
        });

        Ok((
            Self {
                socket,
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

    let msg = recvmsg::<()>(fd.as_raw_fd(), &mut iov, Some(cmsg_buf), MsgFlags::empty())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

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
}
