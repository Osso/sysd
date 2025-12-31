//! IPC protocol for sysd daemon communication
//!
//! Defines request/response types for CLI ↔ daemon communication.

use serde::{Deserialize, Serialize};

pub const SOCKET_PATH: &str = "/run/sysd.sock";

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
