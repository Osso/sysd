//! Path unit operations
//!
//! Handles watching filesystem paths and activating associated units.

use tokio::sync::mpsc;

use crate::units::PathUnit;

use super::{path_watcher, Manager, ManagerError};

impl Manager {
    /// Start a path unit (set up filesystem watches)
    pub(super) async fn start_path(
        &mut self,
        name: &str,
        path_unit: &PathUnit,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        log::info!("Starting path unit {}", name);

        // Create directories if MakeDirectory=true
        if path_unit.path.make_directory {
            let mode = path_unit.path.directory_mode.unwrap_or(0o755);
            for dir in path_unit
                .path
                .path_exists
                .iter()
                .chain(path_unit.path.directory_not_empty.iter())
            {
                if let Err(e) = create_directory(dir, mode) {
                    log::warn!("{}: failed to create directory {}: {}", name, dir, e);
                }
            }
        }

        // Build watch list
        let mut watches = Vec::new();

        for watch_path in &path_unit.path.path_exists {
            watches.push(path_watcher::PathWatch {
                path: watch_path.clone(),
                watch_type: path_watcher::WatchType::Exists,
            });
        }

        for watch_path in &path_unit.path.path_exists_glob {
            watches.push(path_watcher::PathWatch {
                path: watch_path.clone(),
                watch_type: path_watcher::WatchType::ExistsGlob,
            });
        }

        for watch_path in &path_unit.path.path_changed {
            watches.push(path_watcher::PathWatch {
                path: watch_path.clone(),
                watch_type: path_watcher::WatchType::Changed,
            });
        }

        for watch_path in &path_unit.path.path_modified {
            watches.push(path_watcher::PathWatch {
                path: watch_path.clone(),
                watch_type: path_watcher::WatchType::Modified,
            });
        }

        for watch_path in &path_unit.path.directory_not_empty {
            watches.push(path_watcher::PathWatch {
                path: watch_path.clone(),
                watch_type: path_watcher::WatchType::DirectoryNotEmpty,
            });
        }

        if watches.is_empty() {
            log::warn!("{}: no path watches configured", name);
        } else {
            let service_name = path_unit.activated_unit();
            let path_name = name.to_string();
            let tx = self.path_tx.clone();

            log::debug!(
                "{}: watching {} path(s), will activate {}",
                name,
                watches.len(),
                service_name
            );

            // Spawn path watcher task
            tokio::spawn(async move {
                path_watcher::watch_paths(path_name, service_name, watches, tx).await;
            });
        }

        // Mark as active
        if let Some(state) = self.states.get_mut(name) {
            state.set_running(0);
        }

        log::info!("{} active", name);
        Ok(())
    }

    /// Stop a path unit
    pub(super) async fn stop_path(&mut self, name: &str) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        log::info!("Stopping path unit {}", name);

        // Path watcher tasks will complete naturally when their channel is dropped
        // For now, we just mark the unit as stopped

        if let Some(state) = self.states.get_mut(name) {
            state.set_stopped(0);
        }

        log::info!("{} stopped", name);
        Ok(())
    }

    /// Take the path triggered receiver (for use in event loops)
    pub fn take_path_rx(&mut self) -> Option<mpsc::Receiver<path_watcher::PathTriggered>> {
        self.path_rx.take()
    }

    /// Process a path triggered message (start the associated service)
    pub async fn handle_path_triggered(
        &mut self,
        triggered: path_watcher::PathTriggered,
    ) -> Result<(), ManagerError> {
        log::info!(
            "Path triggered: {} activated by {} (path: {})",
            triggered.service_name,
            triggered.path_name,
            triggered.triggered_path
        );

        // Check if service is already running
        if let Some(state) = self.states.get(&triggered.service_name) {
            if state.is_active() {
                log::debug!(
                    "{} already running, skipping path activation",
                    triggered.service_name
                );
                return Ok(());
            }
        }

        // Start the service
        self.start(&triggered.service_name).await
    }
}

/// Create a directory with the specified mode
fn create_directory(path: &str, mode: u32) -> std::io::Result<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    let path = Path::new(path);
    if !path.exists() {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        log::debug!("Created directory {} with mode {:o}", path.display(), mode);
    }
    Ok(())
}
