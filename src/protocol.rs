//! IPC protocol for sysd daemon communication
//!
//! Defines request/response types for CLI ↔ daemon communication.

use serde::{Deserialize, Serialize};

/// System socket path (requires root)
pub const SOCKET_PATH: &str = "/run/sysd.sock";

/// Get socket path for user mode
/// Returns /run/user/<uid>/sysd.sock for non-root users
pub fn user_socket_path() -> String {
    let uid = unsafe { libc::getuid() };
    format!("/run/user/{}/sysd.sock", uid)
}

/// Get socket path based on mode
pub fn socket_path(user_mode: bool) -> String {
    if user_mode {
        user_socket_path()
    } else {
        SOCKET_PATH.to_string()
    }
}

/// Request from CLI to daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// List all units (optionally filtered by type)
    List {
        user: bool,
        unit_type: Option<String>,
    },
    /// Start a unit
    Start { name: String },
    /// Stop a unit
    Stop { name: String },
    /// Restart a unit
    Restart { name: String },
    /// Enable a unit (create symlinks for boot)
    Enable { name: String },
    /// Disable a unit (remove symlinks)
    Disable { name: String },
    /// Check if unit is enabled
    IsEnabled { name: String },
    /// Get unit status
    Status { name: String },
    /// Get unit dependencies
    Deps { name: String },
    /// Get default boot target
    GetBootTarget,
    /// Boot to default target
    Boot { dry_run: bool },
    /// Reload unit files from disk
    ReloadUnitFiles,
    /// Sync units (reload + restart changed)
    SyncUnits,
    /// Switch to target (stop unrelated units)
    SwitchTarget { target: String },
    /// Ping (health check)
    Ping,
}

/// Unit info returned by list/status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitInfo {
    pub name: String,
    pub unit_type: String,
    pub state: String,
    pub description: Option<String>,
}

/// Response from daemon to CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Success with no data
    Ok,
    /// List of units
    Units(Vec<UnitInfo>),
    /// Single unit status
    Status(UnitInfo),
    /// Dependencies as list of unit names
    Deps(Vec<String>),
    /// Boot target name
    BootTarget(String),
    /// Boot plan (units to start)
    BootPlan(Vec<String>),
    /// Enabled state (enabled, disabled, static, etc.)
    EnabledState(String),
    /// Error with message
    Error(String),
    /// Pong (response to ping)
    Pong,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let requests = vec![
            Request::List {
                user: false,
                unit_type: None,
            },
            Request::Start {
                name: "docker.service".into(),
            },
            Request::Stop {
                name: "nginx.service".into(),
            },
            Request::Ping,
        ];

        for req in requests {
            let encoded = rmp_serde::to_vec(&req).unwrap();
            let decoded: Request = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(format!("{:?}", req), format!("{:?}", decoded));
        }
    }

    #[test]
    fn test_socket_path() {
        // System mode uses constant path
        assert_eq!(socket_path(false), SOCKET_PATH);

        // User mode uses /run/user/<uid>/sysd.sock
        let user_path = socket_path(true);
        assert!(user_path.starts_with("/run/user/"));
        assert!(user_path.ends_with("/sysd.sock"));
    }

    #[test]
    fn test_user_socket_path() {
        let path = user_socket_path();
        let uid = unsafe { libc::getuid() };
        assert_eq!(path, format!("/run/user/{}/sysd.sock", uid));
    }

    #[test]
    fn response_roundtrip() {
        let responses = vec![
            Response::Ok,
            Response::Error("test error".into()),
            Response::Units(vec![UnitInfo {
                name: "test.service".into(),
                unit_type: "service".into(),
                state: "running".into(),
                description: Some("Test service".into()),
            }]),
            Response::Pong,
        ];

        for resp in responses {
            let encoded = rmp_serde::to_vec(&resp).unwrap();
            let decoded: Response = rmp_serde::from_slice(&encoded).unwrap();
            assert_eq!(format!("{:?}", resp), format!("{:?}", decoded));
        }
    }
}
