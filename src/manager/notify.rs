//! sd_notify protocol implementation
//!
//! Listens on NOTIFY_SOCKET for service readiness notifications.
//! Protocol: https://www.freedesktop.org/software/systemd/man/sd_notify.html

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Messages from services via sd_notify
#[derive(Debug, Clone)]
pub struct NotifyMessage {
    /// Service that sent the message (matched by socket credentials)
    pub pid: u32,
    /// Parsed key-value pairs from the message
    pub fields: HashMap<String, String>,
}

impl NotifyMessage {
    /// Check if this is a READY=1 notification
    pub fn is_ready(&self) -> bool {
        self.fields.get("READY").map(|v| v == "1").unwrap_or(false)
    }

    /// Check if this is a STOPPING=1 notification
    pub fn is_stopping(&self) -> bool {
        self.fields.get("STOPPING").map(|v| v == "1").unwrap_or(false)
    }

    /// Check if this is a WATCHDOG=1 ping
    pub fn is_watchdog(&self) -> bool {
        self.fields.get("WATCHDOG").map(|v| v == "1").unwrap_or(false)
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

        // Make it world-writable so services can send to it
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o777))?;

        let socket = Arc::new(socket);
        let socket_clone = Arc::clone(&socket);

        // Spawn receiver task
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match socket_clone.recv(&mut buf).await {
                    Ok(len) => {
                        if let Ok(msg) = std::str::from_utf8(&buf[..len]) {
                            let notify_msg = parse_notify_message(msg, 0);
                            if tx.send(notify_msg).await.is_err() {
                                break; // Channel closed
                            }
                        }
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

/// Parse a notify message into key-value pairs
fn parse_notify_message(msg: &str, pid: u32) -> NotifyMessage {
    let mut fields = HashMap::new();

    for line in msg.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.to_string(), value.to_string());
        }
    }

    NotifyMessage { pid, fields }
}

/// Default socket path
pub const NOTIFY_SOCKET_PATH: &str = "/run/sysd/notify";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_notify_message() {
        let msg = parse_notify_message("READY=1\nSTATUS=Running\n", 1234);
        assert!(msg.is_ready());
        assert_eq!(msg.status(), Some("Running"));
        assert_eq!(msg.pid, 1234);
    }

    #[test]
    fn test_parse_stopping() {
        let msg = parse_notify_message("STOPPING=1", 5678);
        assert!(msg.is_stopping());
        assert!(!msg.is_ready());
    }

    #[test]
    fn test_parse_mainpid() {
        let msg = parse_notify_message("MAINPID=9999\nREADY=1", 1000);
        assert_eq!(msg.main_pid(), Some(9999));
        assert!(msg.is_ready());
    }
}
