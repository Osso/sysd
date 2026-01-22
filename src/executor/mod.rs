//! Executor pattern for process spawning
//!
//! This module implements the sd-executor pattern from systemd to avoid
//! copy-on-write memory issues when forking from PID 1.
//!
//! Instead of doing sandbox setup in pre_exec (after fork, before exec),
//! we serialize all execution config and spawn a small executor binary
//! that deserializes and applies the config before exec'ing the target.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::path::PathBuf;

/// Serializable execution configuration
///
/// Contains everything needed to set up the execution environment
/// and spawn the target process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecConfig {
    /// Program to execute
    pub program: String,
    /// Arguments to pass
    pub args: Vec<String>,
    /// Working directory
    pub working_directory: Option<PathBuf>,

    // Environment
    /// Environment variables to set
    pub environment: HashMap<String, String>,
    /// Environment variables to unset
    pub unset_environment: Vec<String>,

    // Credentials
    /// User ID to run as
    pub uid: Option<u32>,
    /// Group ID to run as
    pub gid: Option<u32>,

    // Resource limits
    /// LimitNOFILE (max open files)
    pub limit_nofile: Option<u64>,
    /// LimitNPROC (max processes)
    pub limit_nproc: Option<u64>,
    /// LimitCORE (core dump size)
    pub limit_core: Option<u64>,

    // OOM
    /// OOMScoreAdjust (-1000 to 1000)
    pub oom_score_adjust: Option<i32>,

    // Socket activation
    /// Socket FD positions (will be at 3, 4, 5, ...)
    pub socket_fd_count: usize,
    /// Names for socket FDs (for LISTEN_FDNAMES)
    pub socket_fd_names: Vec<String>,

    // TTY
    /// StandardInput type
    pub std_input: StdInputConfig,
    /// TTY device path for StandardInput=tty
    pub tty_path: Option<PathBuf>,
    /// Reset TTY before use
    pub tty_reset: bool,

    // Security/Sandbox settings
    pub sandbox: SandboxConfig,
}

/// StandardInput configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum StdInputConfig {
    #[default]
    Null,
    Tty,
    TtyForce,
    TtyFail,
}

/// Sandbox/security configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxConfig {
    // Basic security
    pub no_new_privileges: bool,

    // Filesystem protection
    pub protect_system: ProtectSystemConfig,
    pub protect_home: ProtectHomeConfig,
    pub private_tmp: bool,
    pub private_devices: bool,
    pub private_network: bool,
    pub protect_kernel_modules: bool,
    pub protect_proc: ProtectProcConfig,

    // Capabilities
    pub capability_bounding_set: Vec<String>,
    pub ambient_capabilities: Vec<String>,

    // Namespace restrictions
    pub restrict_namespaces: Option<Vec<String>>,

    // Path restrictions
    pub read_write_paths: Vec<PathBuf>,
    pub read_only_paths: Vec<PathBuf>,
    pub inaccessible_paths: Vec<PathBuf>,

    // Seccomp
    pub system_call_filter: Vec<String>,
    pub system_call_error_number: Option<i32>,
    pub system_call_architectures: Vec<String>,

    // Device access
    pub device_policy: DevicePolicyConfig,
    pub device_allow: Vec<String>,

    // Extended security (M16)
    pub restrict_realtime: bool,
    pub protect_control_groups: bool,
    pub memory_deny_write_execute: bool,
    pub lock_personality: bool,
    pub protect_kernel_tunables: bool,
    pub protect_kernel_logs: bool,
    pub protect_clock: bool,
    pub protect_hostname: bool,
    pub ignore_sigpipe: bool,
    pub restrict_suid_sgid: bool,
    pub restrict_address_families: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum ProtectSystemConfig {
    #[default]
    No,
    Yes,
    Full,
    Strict,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum ProtectHomeConfig {
    #[default]
    No,
    Yes,
    ReadOnly,
    Tmpfs,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum ProtectProcConfig {
    #[default]
    Default,
    Invisible,
    Ptraceable,
    NoAccess,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum DevicePolicyConfig {
    #[default]
    Auto,
    Closed,
    Strict,
}

impl ExecConfig {
    /// Serialize to MessagePack bytes
    pub fn serialize(&self) -> Result<Vec<u8>, String> {
        rmp_serde::to_vec(self).map_err(|e| format!("Failed to serialize ExecConfig: {}", e))
    }

    /// Deserialize from MessagePack bytes
    pub fn deserialize(data: &[u8]) -> Result<Self, String> {
        rmp_serde::from_slice(data).map_err(|e| format!("Failed to deserialize ExecConfig: {}", e))
    }
}

/// Write ExecConfig to a memfd and return the fd
pub fn serialize_to_memfd(config: &ExecConfig) -> Result<RawFd, String> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::io::FromRawFd;

    let data = config.serialize()?;

    // Create memfd
    let name = CString::new("sysd-exec").unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!(
            "memfd_create failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    // Write data
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(&data)
        .map_err(|e| format!("Failed to write to memfd: {}", e))?;

    // Seek to beginning for reading
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|e| format!("Failed to seek memfd: {}", e))?;

    // Don't close the fd - we're returning it
    let fd = std::mem::ManuallyDrop::new(file);
    use std::os::unix::io::AsRawFd;
    Ok(fd.as_raw_fd())
}

/// Read ExecConfig from a file descriptor
pub fn deserialize_from_fd(fd: RawFd) -> Result<ExecConfig, String> {
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| format!("Failed to read from fd: {}", e))?;

    // Don't close the fd - caller may need it
    std::mem::forget(file);

    ExecConfig::deserialize(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exec_config_roundtrip() {
        let config = ExecConfig {
            program: "/bin/echo".to_string(),
            args: vec!["hello".to_string(), "world".to_string()],
            working_directory: Some(PathBuf::from("/tmp")),
            environment: [("FOO".to_string(), "bar".to_string())]
                .into_iter()
                .collect(),
            unset_environment: vec!["BAZ".to_string()],
            uid: Some(1000),
            gid: Some(1000),
            limit_nofile: Some(65535),
            limit_nproc: None,
            limit_core: Some(0),
            oom_score_adjust: Some(-500),
            socket_fd_count: 2,
            socket_fd_names: vec!["connection".to_string(), "varlink".to_string()],
            std_input: StdInputConfig::Null,
            tty_path: None,
            tty_reset: false,
            sandbox: SandboxConfig {
                no_new_privileges: true,
                private_tmp: true,
                ..Default::default()
            },
        };

        let data = config.serialize().unwrap();
        let config2 = ExecConfig::deserialize(&data).unwrap();

        assert_eq!(config.program, config2.program);
        assert_eq!(config.args, config2.args);
        assert_eq!(config.uid, config2.uid);
        assert_eq!(config.sandbox.no_new_privileges, config2.sandbox.no_new_privileges);
    }
}
