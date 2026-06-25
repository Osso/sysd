impl Manager {
    /// Stop a service
    pub async fn stop(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);
        if let Some(result) = self.stop_non_service_unit(&name).await {
            return result;
        }
        self.mark_unit_stopping(&name)?;
        let (kill_mode, send_sighup) = self.stop_signal_config(&name);
        self.stop_main_process(&name, &kill_mode, send_sighup).await;
        self.cleanup_stopped_service(&name);
        self.run_stop_post_commands(&name).await;
        Ok(())
    }

    async fn stop_non_service_unit(&mut self, name: &str) -> Option<Result<(), ManagerError>> {
        if let Some(mount) = self.units.get(name).and_then(|u| u.as_mount()).cloned() {
            return Some(self.stop_mount(name, &mount).await);
        }
        if let Some(slice) = self.units.get(name).and_then(|u| u.as_slice()).cloned() {
            return Some(self.stop_slice(name, &slice).await);
        }
        if let Some(socket) = self.units.get(name).and_then(|u| u.as_socket()).cloned() {
            return Some(self.stop_socket(name, &socket).await);
        }
        if self.units.get(name).is_some_and(|u| u.is_timer()) {
            return Some(self.stop_timer(name).await);
        }
        if self.units.get(name).is_some_and(|u| u.is_path()) {
            return Some(self.stop_path(name).await);
        }
        None
    }

    fn mark_unit_stopping(&mut self, name: &str) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;
        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }
        state.set_stopping();
        Ok(())
    }

    fn stop_signal_config(&self, name: &str) -> (KillMode, bool) {
        self.units
            .get(name)
            .and_then(|u| u.as_service())
            .map(|s| (s.service.kill_mode.clone(), s.service.send_sighup))
            .unwrap_or((KillMode::default(), false))
    }

    async fn stop_main_process(&mut self, name: &str, kill_mode: &KillMode, send_sighup: bool) {
        if let Some(mut child) = self.processes.remove(name) {
            Self::send_signals_to_child(&mut child, kill_mode, send_sighup, name).await;
            self.wait_for_child_exit(name, child).await;
            return;
        }
        if let Some(state) = self.states.get_mut(name) {
            state.set_stopped(0);
        }
    }

    fn cleanup_stopped_service(&mut self, name: &str) {
        self.cleanup_service_cgroup_after_stop(name);
        self.cleanup_runtime_dirs(name);
        self.watchdog_deadlines.remove(name);
        self.release_dynamic_uid_after_stop(name);
        self.close_stored_fds_after_stop(name);
    }

    fn cleanup_service_cgroup_after_stop(&mut self, name: &str) {
        if self.cgroup_paths.remove(name).is_none() {
            return;
        }
        let slice = self
            .units
            .get(name)
            .and_then(|u| u.as_service())
            .and_then(|s| s.service.slice.as_deref());
        let Some(cgroup_mgr) = self.cgroup_manager.as_ref() else {
            return;
        };
        if let Err(e) = cgroup_mgr.cleanup_service_cgroup(name, slice) {
            log::debug!("Failed to clean up cgroup for {}: {}", name, e);
        }
    }

    fn release_dynamic_uid_after_stop(&mut self, name: &str) {
        let Some(uid) = self.dynamic_uids.remove(name) else {
            return;
        };
        self.dynamic_user_manager.release(uid);
        log::debug!("Released dynamic UID {} for {}", uid, name);
    }

    fn close_stored_fds_after_stop(&mut self, name: &str) {
        let Some(fds) = self.fd_store.remove(name) else {
            return;
        };
        for (fd_name, fd) in fds {
            log::debug!("Closing stored FD {} ('{}') for {}", fd, fd_name, name);
            unsafe { libc::close(fd) };
        }
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

    /// List all loaded units with their types and states
    pub fn list_units(&self) -> Vec<(&String, &Unit, Option<&ServiceState>)> {
        self.units
            .iter()
            .map(|(name, unit)| (name, unit, self.states.get(name)))
            .collect()
    }

    /// Set the D-Bus connection for scope registration
    pub fn set_dbus_connection(&mut self, conn: zbus::Connection) {
        self.scope_manager.set_dbus_connection(conn);
    }

    /// Get the cgroup manager
    pub fn cgroup_manager(&self) -> Option<&CgroupManager> {
        self.cgroup_manager.as_ref()
    }

    /// Get the scope manager
    pub fn scope_manager(&self) -> &ScopeManager {
        &self.scope_manager
    }

    /// Get the scope manager mutably
    pub fn scope_manager_mut(&mut self) -> &mut ScopeManager {
        &mut self.scope_manager
    }

    /// Register a transient scope (called by D-Bus StartTransientUnit)
    pub async fn register_scope(
        &mut self,
        name: &str,
        slice: Option<&str>,
        description: Option<&str>,
        pids: &[u32],
    ) -> Result<PathBuf, ManagerError> {
        let path = self
            .scope_manager
            .register(name, slice, description, pids)
            .await?;
        // Track in states for list/status queries
        self.states
            .insert(name.to_string(), ServiceState::running_scope());
        Ok(path)
    }

    /// Unregister a scope (called when scope is abandoned or empty)
    pub async fn unregister_scope(&mut self, name: &str) -> Result<(), ManagerError> {
        self.states.remove(name);
        self.scope_manager.unregister(name).await
    }

    /// Normalize unit name (add .service suffix if no suffix present)
    fn normalize_name(&self, name: &str) -> String {
        if name.ends_with(".service")
            || name.ends_with(".target")
            || name.ends_with(".mount")
            || name.ends_with(".socket")
            || name.ends_with(".path")
            || name.ends_with(".slice")
            || name.ends_with(".timer")
        {
            name.to_string()
        } else {
            format!("{}.service", name)
        }
    }

    /// M20: Get boot plan without starting (for dry-run)
    pub async fn get_boot_plan(&mut self, target: &str) -> Result<Vec<String>, ManagerError> {
        let name = self.normalize_name(target);
        self.resolve_start_order(&name).await
    }

    /// M20: Reload all unit files from disk
    pub async fn reload_units(&mut self) -> Result<usize, ManagerError> {
        let unit_names: Vec<String> = self.units.keys().cloned().collect();
        let mut reloaded = 0;

        for name in unit_names {
            // Skip scopes (transient, not from files)
            if name.ends_with(".scope") {
                continue;
            }

            // Find the unit file
            let path = match self.find_unit(&name) {
                Ok(p) => p,
                Err(_) => {
                    log::debug!("Unit {} no longer exists on disk, keeping in memory", name);
                    continue;
                }
            };

            // Re-parse it
            match units::load_unit(&path).await {
                Ok(new_unit) => {
                    self.units.insert(name.clone(), new_unit);
                    reloaded += 1;
                    log::debug!("Reloaded {}", name);
                }
                Err(e) => {
                    log::warn!("Failed to reload {}: {}", name, e);
                }
            }
        }

        log::info!("Reloaded {} unit files", reloaded);
        Ok(reloaded)
    }

    /// M20: Sync units - reload files and restart changed services
    pub async fn sync_units(&mut self) -> Result<Vec<String>, ManagerError> {
        let old_hashes = self.snapshot_service_hashes();
        self.reload_units().await?;
        let mut restarted = Vec::new();
        for (name, old_hash) in old_hashes {
            if !self.service_config_changed(&name, old_hash) {
                continue;
            }
            if self.restart_changed_running_service(&name).await {
                restarted.push(name);
            }
        }
        Ok(restarted)
    }

    fn snapshot_service_hashes(&self) -> HashMap<String, u64> {
        self.units
            .iter()
            .filter_map(|(name, unit)| {
                unit.as_service()
                    .map(|svc| (name.clone(), service_config_hash(svc)))
            })
            .collect()
    }

    fn service_config_changed(&self, name: &str, old_hash: u64) -> bool {
        self.units
            .get(name)
            .and_then(|unit| unit.as_service())
            .map(|svc| service_config_hash(svc) != old_hash)
            .unwrap_or(false)
    }

    async fn restart_changed_running_service(&mut self, name: &str) -> bool {
        if !self.states.get(name).is_some_and(ServiceState::is_active) {
            return false;
        }
        log::info!("{} config changed, restarting", name);
        match self.restart(name).await {
            Ok(()) => true,
            Err(e) => {
                log::warn!("Failed to restart {}: {}", name, e);
                false
            }
        }
    }

    /// M20: Switch to target, stopping units not in its dependency tree
    pub async fn switch_target(&mut self, target: &str) -> Result<Vec<String>, ManagerError> {
        let target = self.normalize_name(target);

        // Get all units needed by the target
        let needed: std::collections::HashSet<String> = self
            .resolve_start_order(&target)
            .await?
            .into_iter()
            .collect();

        // Find running units not in the needed set
        let to_stop: Vec<String> = self
            .states
            .iter()
            .filter(|(name, state)| state.is_active() && !needed.contains(*name))
            .map(|(name, _)| name.clone())
            .collect();

        // Stop unneeded units
        for name in &to_stop {
            log::info!("Stopping {} (not needed by {})", name, target);
            if let Err(e) = self.stop(name).await {
                log::warn!("Failed to stop {}: {}", name, e);
            }
        }

        // Start the target with dependencies
        self.start_with_deps(&target).await?;

        Ok(to_stop)
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

    /// Import environment variables (for user session management)
    pub fn import_environment(&mut self, vars: Vec<(String, String)>) {
        for (key, value) in vars {
            self.user_environment.insert(key, value);
        }
        log::info!(
            "Imported {} environment variables",
            self.user_environment.len()
        );
    }

    /// Unset environment variables
    pub fn unset_environment(&mut self, names: &[String]) {
        for name in names {
            self.user_environment.remove(name);
        }
        log::info!("Unset {} environment variables", names.len());
    }

    /// Get imported environment variables (to be passed to spawned services)
    pub fn get_user_environment(&self) -> &HashMap<String, String> {
        &self.user_environment
    }

    /// Reset failed state of all units
    pub fn reset_failed(&mut self) {
        for (name, state) in self.states.iter_mut() {
            if state.active == ActiveState::Failed {
                log::info!("Resetting failed state of {}", name);
                state.active = ActiveState::Inactive;
                state.sub = SubState::Dead;
                state.error = None;
            }
        }
    }
}

fn service_cgroup_limits(service: &Service) -> CgroupLimits {
    CgroupLimits {
        memory_max: service.service.memory_max,
        cpu_quota: service.service.cpu_quota,
        tasks_max: service.service.tasks_max,
    }
}

fn default_instance_for_unit(unit: &Unit) -> Option<String> {
    match unit {
        Unit::Service(s) => s.install.default_instance.clone(),
        Unit::Socket(s) => s.install.default_instance.clone(),
        Unit::Timer(t) => t.install.default_instance.clone(),
        _ => None,
    }
}

fn oneshot_completion_result(
    result: Result<std::process::Output, std::io::Error>,
) -> (Option<i32>, Option<String>) {
    match result {
        Ok(output) => {
            let code = output.status.code().unwrap_or(-1);
            if code == 0 {
                (Some(0), None)
            } else {
                (Some(code), Some(format!("exit code {}", code)))
            }
        }
        Err(e) => (None, Some(e.to_string())),
    }
}

fn service_config_hash(service: &Service) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    service.service.exec_start.hash(&mut hasher);
    service.service.environment.hash(&mut hasher);
    service.service.environment_file.hash(&mut hasher);
    service.service.working_directory.hash(&mut hasher);
    service.service.user.hash(&mut hasher);
    service.service.group.hash(&mut hasher);
    hasher.finish()
}

fn push_unique_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

fn queue_dependency(to_load: &mut Vec<String>, queued: &mut HashSet<String>, dep: &str) {
    if queued.insert(dep.to_string()) {
        to_load.push(dep.to_string());
    }
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

/// M17: Clean up runtime directories when service stops
fn cleanup_runtime_directories(service: &crate::units::ServiceSection, service_name: &str) {
    let base_name = service_name
        .strip_suffix(".service")
        .unwrap_or(service_name);

    for name in &service.runtime_directory {
        let dir_name = if name.is_empty() {
            base_name
        } else {
            name.as_str()
        };
        let path = std::path::Path::new("/run").join(dir_name);
        if path.exists() {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                log::warn!(
                    "Failed to remove runtime directory {}: {}",
                    path.display(),
                    e
                );
            } else {
                log::debug!("Removed runtime directory: {}", path.display());
            }
        }
    }
}

/// Run a simple command (for ExecStopPost, etc.)
/// Parses the command line and runs it, waiting for completion
async fn run_simple_command(cmd_line: &str) -> Result<(), std::io::Error> {
    use tokio::process::Command;

    // Strip leading - (ignore errors) or + (run as root)
    let cmd_line = cmd_line
        .trim_start_matches('-')
        .trim_start_matches('+')
        .trim();

    // Split command line (simple split, doesn't handle quotes properly)
    let parts: Vec<&str> = cmd_line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(());
    }

    let program = parts[0];
    let args = &parts[1..];

    let status = Command::new(program).args(args).status().await?;

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Command exited with {}", status),
        ))
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

    #[error("Condition failed for {0}: {1}")]
    ConditionFailed(String, String),

    #[error("Unit has no [Install] section: {0}")]
    NoInstallSection(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("Failed to start: {0}")]
    StartFailed(String),

    #[error("Failed to stop: {0}")]
    StopFailed(String),

    #[error("Unit is masked: {0}")]
    Masked(String),
}

impl From<std::io::Error> for ManagerError {
    fn from(e: std::io::Error) -> Self {
        ManagerError::Io(e.to_string())
    }
}
