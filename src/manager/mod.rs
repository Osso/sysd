//! Service manager
//!
//! Loads, starts, stops, and monitors services.

mod process;
mod state;

pub use process::SpawnError;
pub use state::{ActiveState, ServiceState, SubState};

use std::collections::HashMap;
use std::path::PathBuf;
use tokio::process::Child;

use crate::units::{self, Service};

/// Service manager that tracks and controls services
pub struct Manager {
    /// Loaded service definitions
    services: HashMap<String, Service>,
    /// Runtime state for each service
    states: HashMap<String, ServiceState>,
    /// Running child processes
    processes: HashMap<String, Child>,
    /// Unit search paths
    unit_paths: Vec<PathBuf>,
}

impl Manager {
    /// Create a new service manager
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            states: HashMap::new(),
            processes: HashMap::new(),
            unit_paths: vec![
                PathBuf::from("/etc/systemd/system"),
                PathBuf::from("/usr/lib/systemd/system"),
            ],
        }
    }

    /// Load a service by name
    pub async fn load(&mut self, name: &str) -> Result<(), ManagerError> {
        // Add .service suffix if not present
        let name = if name.ends_with(".service") {
            name.to_string()
        } else {
            format!("{}.service", name)
        };

        // Find the unit file
        let path = self.find_unit(&name)?;

        // Parse it
        let service = units::load_service(&path).await
            .map_err(|e| ManagerError::Parse(e.to_string()))?;

        // Initialize state
        self.states.insert(name.clone(), ServiceState::new());
        self.services.insert(name, service);

        Ok(())
    }

    /// Load a service from a specific path
    pub async fn load_from_path(&mut self, path: &std::path::Path) -> Result<(), ManagerError> {
        let service = units::load_service(path).await
            .map_err(|e| ManagerError::Parse(e.to_string()))?;

        let name = service.name.clone();
        self.states.insert(name.clone(), ServiceState::new());
        self.services.insert(name, service);

        Ok(())
    }

    /// Find a unit file in search paths
    fn find_unit(&self, name: &str) -> Result<PathBuf, ManagerError> {
        for base in &self.unit_paths {
            let path = base.join(name);
            if path.exists() {
                return Ok(path);
            }
        }
        Err(ManagerError::NotFound(name.to_string()))
    }

    /// Start a service
    pub async fn start(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);

        // Load if not already loaded
        if !self.services.contains_key(&name) {
            self.load(&name).await?;
        }

        let service = self.services.get(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        let state = self.states.get_mut(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        // Check if already running
        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name));
        }

        // Update state to starting
        state.set_starting();

        // Spawn the process
        let child = process::spawn_service(service)?;
        let pid = child.id().unwrap_or(0);

        // Update state to running
        state.set_running(pid);

        // Store the child process
        self.processes.insert(name.clone(), child);

        log::info!("Started {} (PID {})", name, pid);
        Ok(())
    }

    /// Stop a service
    pub async fn stop(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);

        let state = self.states.get_mut(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name));
        }

        state.set_stopping();

        // Get the child process
        if let Some(mut child) = self.processes.remove(&name) {
            // Send SIGTERM
            if let Some(pid) = child.id() {
                log::info!("Stopping {} (PID {})", name, pid);
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }

            // Wait for exit (with timeout)
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                child.wait()
            ).await {
                Ok(Ok(status)) => {
                    let code = status.code().unwrap_or(-1);
                    state.set_stopped(code);
                    log::info!("Stopped {} (exit code {})", name, code);
                }
                Ok(Err(e)) => {
                    state.set_failed(e.to_string());
                }
                Err(_) => {
                    // Timeout - send SIGKILL
                    log::warn!("Timeout stopping {}, sending SIGKILL", name);
                    if let Some(pid) = child.id() {
                        unsafe {
                            libc::kill(pid as i32, libc::SIGKILL);
                        }
                    }
                    let _ = child.wait().await;
                    state.set_stopped(-9);
                }
            }
        } else {
            state.set_stopped(0);
        }

        Ok(())
    }

    /// Get service status
    pub fn status(&self, name: &str) -> Option<&ServiceState> {
        let name = self.normalize_name(name);
        self.states.get(&name)
    }

    /// Get service definition
    pub fn get_service(&self, name: &str) -> Option<&Service> {
        let name = self.normalize_name(name);
        self.services.get(&name)
    }

    /// List loaded services
    pub fn list(&self) -> impl Iterator<Item = (&String, &ServiceState)> {
        self.states.iter()
    }

    /// Normalize service name (add .service suffix if needed)
    fn normalize_name(&self, name: &str) -> String {
        if name.ends_with(".service") {
            name.to_string()
        } else {
            format!("{}.service", name)
        }
    }

    /// Check on running processes and update states
    pub async fn reap(&mut self) {
        let mut exited = Vec::new();

        for (name, child) in &mut self.processes {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited.push((name.clone(), status.code().unwrap_or(-1)));
                }
                Ok(None) => {
                    // Still running
                }
                Err(e) => {
                    log::error!("Error checking {}: {}", name, e);
                }
            }
        }

        for (name, code) in exited {
            self.processes.remove(&name);
            if let Some(state) = self.states.get_mut(&name) {
                if code == 0 {
                    state.set_stopped(code);
                    log::info!("{} exited cleanly", name);
                } else {
                    state.set_failed(format!("Exit code {}", code));
                    log::warn!("{} failed with exit code {}", name, code);
                }
            }
        }
    }
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("Service not found: {0}")]
    NotFound(String),

    #[error("Failed to parse service: {0}")]
    Parse(String),

    #[error("Service already active: {0}")]
    AlreadyActive(String),

    #[error("Service not active: {0}")]
    NotActive(String),

    #[error("Failed to spawn: {0}")]
    Spawn(#[from] SpawnError),
}
