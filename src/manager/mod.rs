//! Service manager
//!
//! Loads, starts, stops, and monitors services and targets.

mod deps;
mod process;
mod state;

pub use deps::{CycleError, DepGraph};
pub use process::SpawnError;
pub use state::{ActiveState, ServiceState, SubState};

use std::collections::HashMap;
use std::path::PathBuf;
use tokio::process::Child;

use crate::units::{self, Service, Unit};

/// Service manager that tracks and controls units (services and targets)
pub struct Manager {
    /// Loaded unit definitions (services and targets)
    units: HashMap<String, Unit>,
    /// Runtime state for each unit
    states: HashMap<String, ServiceState>,
    /// Running child processes (only for services)
    processes: HashMap<String, Child>,
    /// Unit search paths
    unit_paths: Vec<PathBuf>,
}

impl Manager {
    /// Create a new service manager
    pub fn new() -> Self {
        Self {
            units: HashMap::new(),
            states: HashMap::new(),
            processes: HashMap::new(),
            unit_paths: vec![
                PathBuf::from("/etc/systemd/system"),
                PathBuf::from("/usr/lib/systemd/system"),
            ],
        }
    }

    /// Load a unit (service or target) by name
    pub async fn load(&mut self, name: &str) -> Result<(), ManagerError> {
        // Normalize the name
        let name = self.normalize_name(name);

        // Already loaded?
        if self.units.contains_key(&name) {
            return Ok(());
        }

        // Find the unit file
        let path = self.find_unit(&name)?;

        // Parse it
        let unit = units::load_unit(&path).await
            .map_err(|e| ManagerError::Parse(e.to_string()))?;

        // Initialize state
        self.states.insert(name.clone(), ServiceState::new());
        self.units.insert(name, unit);

        Ok(())
    }

    /// Load a unit from a specific path
    pub async fn load_from_path(&mut self, path: &std::path::Path) -> Result<(), ManagerError> {
        let unit = units::load_unit(path).await
            .map_err(|e| ManagerError::Parse(e.to_string()))?;

        let name = unit.name().to_string();
        self.states.insert(name.clone(), ServiceState::new());
        self.units.insert(name, unit);

        Ok(())
    }

    /// Find a unit file in search paths
    fn find_unit(&self, name: &str) -> Result<PathBuf, ManagerError> {
        for base in &self.unit_paths {
            let path = base.join(name);
            if path.exists() {
                return Ok(path);
            }
            // Also follow symlinks
            if path.is_symlink() {
                if let Ok(target) = std::fs::read_link(&path) {
                    if target.exists() {
                        return Ok(path);
                    }
                }
            }
        }
        Err(ManagerError::NotFound(name.to_string()))
    }

    /// Start a single service (no dependency resolution)
    pub async fn start(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);
        self.start_single(&name).await
    }

    /// Start a unit with all its dependencies
    pub async fn start_with_deps(&mut self, name: &str) -> Result<Vec<String>, ManagerError> {
        let name = self.normalize_name(name);

        // Load the target unit and discover all dependencies
        let order = self.resolve_start_order(&name).await?;

        log::info!("Start order for {}: {:?}", name, order);

        // Start units in order
        let mut started = Vec::new();
        for unit_name in &order {
            // Skip if already running
            if let Some(state) = self.states.get(unit_name) {
                if state.is_active() {
                    log::debug!("{} already running, skipping", unit_name);
                    continue;
                }
            }

            match self.start_single(unit_name).await {
                Ok(()) => {
                    started.push(unit_name.clone());
                }
                Err(ManagerError::IsTarget(_)) => {
                    // Targets don't need to be started, just mark as active
                    if let Some(state) = self.states.get_mut(unit_name) {
                        state.set_running(0);
                    }
                    log::debug!("Target {} reached", unit_name);
                }
                Err(e) => {
                    // Check if this is a hard dependency (Requires)
                    let is_required = self.units.get(&name)
                        .map(|u| u.unit_section().requires.contains(unit_name))
                        .unwrap_or(false);

                    if is_required {
                        log::error!("Required dependency {} failed: {}", unit_name, e);
                        return Err(e);
                    } else {
                        // Soft dependency (Wants) - log and continue
                        log::warn!("Optional dependency {} failed: {}", unit_name, e);
                    }
                }
            }
        }

        Ok(started)
    }

    /// Resolve start order for a unit and its dependencies
    async fn resolve_start_order(&mut self, name: &str) -> Result<Vec<String>, ManagerError> {
        // Load the target unit first
        if !self.units.contains_key(name) {
            self.load(name).await?;
        }

        // Collect all dependencies transitively
        let mut to_load: Vec<String> = vec![name.to_string()];
        let mut loaded: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(unit_name) = to_load.pop() {
            if loaded.contains(&unit_name) {
                continue;
            }

            // Try to load the unit
            if !self.units.contains_key(&unit_name) {
                if let Err(e) = self.load(&unit_name).await {
                    log::warn!("Could not load dependency {}: {}", unit_name, e);
                    // Skip missing dependencies
                    continue;
                }
            }

            loaded.insert(unit_name.clone());

            // Queue its dependencies
            if let Some(unit) = self.units.get(&unit_name) {
                let section = unit.unit_section();
                for dep in &section.after {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
                for dep in &section.requires {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
                for dep in &section.wants {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
                // Also include .wants directory entries for targets
                for dep in unit.wants_dir() {
                    if !loaded.contains(dep) {
                        to_load.push(dep.clone());
                    }
                }
            }
        }

        // Build dependency graph from loaded units
        let mut graph = deps::DepGraph::new();
        for unit in self.units.values() {
            if loaded.contains(unit.name()) {
                graph.add_unit(unit);
            }
        }

        // Get topological order
        graph.start_order_for(name)
            .map_err(|e| ManagerError::Cycle(e.nodes))
    }

    /// Start a single unit (internal, assumes already loaded)
    async fn start_single(&mut self, name: &str) -> Result<(), ManagerError> {
        // Load if not already loaded
        if !self.units.contains_key(name) {
            self.load(name).await?;
        }

        let unit = self.units.get(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        // Targets are synchronization points, no process to start
        if unit.is_target() {
            return Err(ManagerError::IsTarget(name.to_string()));
        }

        let service = unit.as_service()
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        let state = self.states.get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        // Check if already running
        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        // Update state to starting
        state.set_starting();

        // Spawn the process
        let child = process::spawn_service(service)?;
        let pid = child.id().unwrap_or(0);

        // Update state to running
        state.set_running(pid);

        // Store the child process
        self.processes.insert(name.to_string(), child);

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

    /// Restart a service (stop then start)
    pub async fn restart(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);

        // Stop if running (ignore NotActive error)
        match self.stop(&name).await {
            Ok(()) => {}
            Err(ManagerError::NotActive(_)) => {}
            Err(e) => return Err(e),
        }

        // Start
        self.start(&name).await
    }

    /// Enable a unit (create symlinks based on [Install] section)
    pub async fn enable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let name = self.normalize_name(name);

        // Load the unit to get its Install section
        if !self.units.contains_key(&name) {
            self.load(&name).await?;
        }

        let unit = self.units.get(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        let install = unit.install_section()
            .ok_or_else(|| ManagerError::NoInstallSection(name.clone()))?;

        if install.wanted_by.is_empty() && install.required_by.is_empty() {
            return Err(ManagerError::NoInstallSection(name.clone()));
        }

        // Find the unit file path
        let unit_path = self.find_unit(&name)?;

        let mut created = Vec::new();

        // Create symlinks in .wants directories
        for target in &install.wanted_by {
            let link = self.create_wants_link(&name, target, &unit_path)?;
            created.push(link);
        }

        // Create symlinks in .requires directories
        for target in &install.required_by {
            let link = self.create_requires_link(&name, target, &unit_path)?;
            created.push(link);
        }

        Ok(created)
    }

    /// Disable a unit (remove symlinks)
    pub async fn disable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let name = self.normalize_name(name);

        // Load to get Install section
        if !self.units.contains_key(&name) {
            self.load(&name).await?;
        }

        let unit = self.units.get(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        let install = unit.install_section()
            .ok_or_else(|| ManagerError::NoInstallSection(name.clone()))?;

        let mut removed = Vec::new();

        // Remove from .wants directories
        for target in &install.wanted_by {
            if let Some(link) = self.remove_wants_link(&name, target)? {
                removed.push(link);
            }
        }

        // Remove from .requires directories
        for target in &install.required_by {
            if let Some(link) = self.remove_requires_link(&name, target)? {
                removed.push(link);
            }
        }

        Ok(removed)
    }

    /// Create a symlink in target.wants/
    fn create_wants_link(&self, unit_name: &str, target: &str, unit_path: &PathBuf) -> Result<PathBuf, ManagerError> {
        let wants_dir = PathBuf::from("/etc/systemd/system").join(format!("{}.wants", target));
        std::fs::create_dir_all(&wants_dir)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        let link_path = wants_dir.join(unit_name);
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path)
                .map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    /// Create a symlink in target.requires/
    fn create_requires_link(&self, unit_name: &str, target: &str, unit_path: &PathBuf) -> Result<PathBuf, ManagerError> {
        let requires_dir = PathBuf::from("/etc/systemd/system").join(format!("{}.requires", target));
        std::fs::create_dir_all(&requires_dir)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        let link_path = requires_dir.join(unit_name);
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path)
                .map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    /// Remove symlink from target.wants/
    fn remove_wants_link(&self, unit_name: &str, target: &str) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system")
            .join(format!("{}.wants", target))
            .join(unit_name);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path)
                .map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    /// Remove symlink from target.requires/
    fn remove_requires_link(&self, unit_name: &str, target: &str) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system")
            .join(format!("{}.requires", target))
            .join(unit_name);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path)
                .map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    /// Check if a unit is enabled
    pub async fn is_enabled(&mut self, name: &str) -> Result<String, ManagerError> {
        let name = self.normalize_name(name);

        // Load to get Install section
        if !self.units.contains_key(&name) {
            self.load(&name).await?;
        }

        let unit = self.units.get(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        // No install section = static (can't be enabled/disabled)
        let Some(install) = unit.install_section() else {
            return Ok("static".to_string());
        };

        if install.wanted_by.is_empty() && install.required_by.is_empty() {
            return Ok("static".to_string());
        }

        // Check if any symlinks exist
        for target in &install.wanted_by {
            let link_path = PathBuf::from("/etc/systemd/system")
                .join(format!("{}.wants", target))
                .join(&name);
            if link_path.exists() || link_path.is_symlink() {
                return Ok("enabled".to_string());
            }
        }

        for target in &install.required_by {
            let link_path = PathBuf::from("/etc/systemd/system")
                .join(format!("{}.requires", target))
                .join(&name);
            if link_path.exists() || link_path.is_symlink() {
                return Ok("enabled".to_string());
            }
        }

        Ok("disabled".to_string())
    }

    /// Get service status
    pub fn status(&self, name: &str) -> Option<&ServiceState> {
        let name = self.normalize_name(name);
        self.states.get(&name)
    }

    /// Get service definition
    pub fn get_service(&self, name: &str) -> Option<&Service> {
        let name = self.normalize_name(name);
        self.units.get(&name).and_then(|u| u.as_service())
    }

    /// Get unit definition
    pub fn get_unit(&self, name: &str) -> Option<&Unit> {
        let name = self.normalize_name(name);
        self.units.get(&name)
    }

    /// List loaded units
    pub fn list(&self) -> impl Iterator<Item = (&String, &ServiceState)> {
        self.states.iter()
    }

    /// Normalize unit name (add .service suffix if no suffix present)
    fn normalize_name(&self, name: &str) -> String {
        if name.ends_with(".service") || name.ends_with(".target") {
            name.to_string()
        } else {
            format!("{}.service", name)
        }
    }

    /// Get the default target (resolves default.target symlink)
    pub fn get_default_target(&self) -> Result<String, ManagerError> {
        // Look for default.target in unit paths
        for base in &self.unit_paths {
            let path = base.join("default.target");
            if path.exists() || path.is_symlink() {
                // Resolve the symlink to get the actual target
                if let Ok(resolved) = std::fs::read_link(&path) {
                    // Extract just the filename
                    if let Some(name) = resolved.file_name().and_then(|n| n.to_str()) {
                        return Ok(name.to_string());
                    }
                }
                // If not a symlink or can't resolve, use as-is
                return Ok("default.target".to_string());
            }
        }
        Err(ManagerError::NotFound("default.target".to_string()))
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
    #[error("Unit not found: {0}")]
    NotFound(String),

    #[error("Failed to parse unit: {0}")]
    Parse(String),

    #[error("Unit already active: {0}")]
    AlreadyActive(String),

    #[error("Unit not active: {0}")]
    NotActive(String),

    #[error("Failed to spawn: {0}")]
    Spawn(#[from] SpawnError),

    #[error("Dependency cycle detected: {}", .0.join(" -> "))]
    Cycle(Vec<String>),

    #[error("Unit is a target (no process): {0}")]
    IsTarget(String),

    #[error("Unit has no [Install] section: {0}")]
    NoInstallSection(String),

    #[error("I/O error: {0}")]
    Io(String),
}
