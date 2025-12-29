//! Process spawning and management

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use tokio::process::{Child, Command};

use crate::units::Service;

/// Options for spawning a service
#[derive(Default)]
pub struct SpawnOptions {
    /// Path to NOTIFY_SOCKET for Type=notify services
    pub notify_socket: Option<String>,
}

/// Spawn a process for a service (convenience wrapper)
#[allow(dead_code)]
pub fn spawn_service(service: &Service) -> Result<Child, SpawnError> {
    spawn_service_with_options(service, &SpawnOptions::default())
}

/// Spawn a process for a service with options
pub fn spawn_service_with_options(service: &Service, options: &SpawnOptions) -> Result<Child, SpawnError> {
    let exec_start = service.service.exec_start.first()
        .ok_or_else(|| SpawnError::NoExecStart(service.name.clone()))?;

    let (program, args) = parse_command(exec_start)?;

    let mut cmd = Command::new(&program);
    cmd.args(&args);

    // Set working directory
    if let Some(wd) = &service.service.working_directory {
        cmd.current_dir(wd);
    }

    // Set environment
    cmd.env_clear();
    cmd.envs(std::env::vars()); // Inherit current env
    for (key, value) in &service.service.environment {
        cmd.env(key, value);
    }

    // Load environment files
    for env_file in &service.service.environment_file {
        if let Ok(vars) = load_env_file(env_file) {
            cmd.envs(vars);
        }
    }

    // Set NOTIFY_SOCKET for Type=notify services
    if let Some(socket_path) = &options.notify_socket {
        cmd.env("NOTIFY_SOCKET", socket_path);
    }

    // Set user/group if specified
    #[cfg(unix)]
    if let Some(user) = &service.service.user {
        if let Some(uid) = resolve_user(user) {
            unsafe {
                cmd.pre_exec(move || {
                    nix::unistd::setuid(nix::unistd::Uid::from_raw(uid))?;
                    Ok(())
                });
            }
        }
    }

    // Redirect stdout/stderr based on config
    // For now, inherit (we'll add journal support later)
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    cmd.stdin(Stdio::null());

    // Spawn the process
    let child = cmd.spawn().map_err(|e| SpawnError::Spawn(e.to_string()))?;

    Ok(child)
}

/// Parse a command line into program and arguments
fn parse_command(cmd: &str) -> Result<(String, Vec<String>), SpawnError> {
    // Handle special prefixes (-, @, +, !, !!)
    let cmd = cmd.trim_start_matches(|c| c == '-' || c == '@' || c == '+' || c == '!' );

    let parts = shlex::split(cmd)
        .ok_or_else(|| SpawnError::InvalidCommand(cmd.to_string()))?;

    if parts.is_empty() {
        return Err(SpawnError::InvalidCommand(cmd.to_string()));
    }

    let program = parts[0].clone();
    let args = parts[1..].to_vec();

    Ok((program, args))
}

/// Load environment variables from a file
fn load_env_file(path: &Path) -> Result<HashMap<String, String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let mut vars = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            // Remove quotes from value
            let value = value.trim_matches('"').trim_matches('\'');
            vars.insert(key.to_string(), value.to_string());
        }
    }

    Ok(vars)
}

/// Resolve username to UID
#[cfg(unix)]
fn resolve_user(user: &str) -> Option<u32> {
    // Try numeric UID first
    if let Ok(uid) = user.parse::<u32>() {
        return Some(uid);
    }

    // Look up by name
    use std::ffi::CString;
    let name = CString::new(user).ok()?;
    unsafe {
        let pwd = libc::getpwnam(name.as_ptr());
        if pwd.is_null() {
            None
        } else {
            Some((*pwd).pw_uid)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("Service {0} has no ExecStart")]
    NoExecStart(String),

    #[error("Invalid command: {0}")]
    InvalidCommand(String),

    #[error("Failed to spawn process: {0}")]
    Spawn(String),
}
