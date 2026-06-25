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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{ActiveState, ServiceState, SubState};
    use crate::units::{Service, Unit};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempRoot(std::path::PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(label: &str) -> TempRoot {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sysd-path-ops-{label}-{}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempRoot(dir)
    }

    fn path_unit(name: &str, configure: impl FnOnce(&mut PathUnit)) -> PathUnit {
        let mut unit = PathUnit::new(name.to_string());
        configure(&mut unit);
        unit
    }

    #[test]
    fn create_directory_creates_missing_dirs_with_mode_and_keeps_existing_dirs() {
        let root = temp_dir("mkdir");
        let dir = root.0.join("nested/path");

        create_directory(dir.to_str().unwrap(), 0o750).unwrap();
        create_directory(dir.to_str().unwrap(), 0o700).unwrap();

        assert!(dir.is_dir());
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[tokio::test]
    async fn start_path_requires_state_creates_directories_and_emits_initial_trigger() {
        let root = temp_dir("start");
        let ready_dir = root.0.join("ready");
        let spool_dir = root.0.join("spool");
        let mut manager = Manager::new();
        let mut rx = manager.take_path_rx().unwrap();
        let unit = path_unit("ready.path", |unit| {
            unit.path.make_directory = true;
            unit.path.directory_mode = Some(0o710);
            unit.path.path_exists.push(ready_dir.to_string_lossy().into_owned());
            unit.path
                .directory_not_empty
                .push(spool_dir.to_string_lossy().into_owned());
            unit.path.unit = Some("ready.service".to_string());
        });

        assert!(matches!(
            manager.start_path("ready.path", &unit).await,
            Err(ManagerError::NotFound(name)) if name == "ready.path"
        ));

        manager
            .states
            .insert("ready.path".to_string(), ServiceState::new());
        manager.start_path("ready.path", &unit).await.unwrap();

        assert!(ready_dir.is_dir());
        assert!(spool_dir.is_dir());
        assert_eq!(
            std::fs::metadata(&ready_dir).unwrap().permissions().mode() & 0o777,
            0o710
        );
        assert!(manager.states.get("ready.path").unwrap().is_active());

        let trigger = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(trigger.path_name, "ready.path");
        assert_eq!(trigger.service_name, "ready.service");
        assert_eq!(trigger.triggered_path, ready_dir.to_string_lossy());
    }

    #[tokio::test]
    async fn start_path_with_all_watch_types_marks_active_and_rejects_already_active() {
        let root = temp_dir("all-watch-types");
        let mut manager = Manager::new();
        let _rx = manager.take_path_rx().unwrap();
        let unit = path_unit("all.path", |unit| {
            unit.path
                .path_exists_glob
                .push(root.0.join("*.ready").to_string_lossy().into_owned());
            unit.path
                .path_changed
                .push(root.0.join("changed").to_string_lossy().into_owned());
            unit.path
                .path_modified
                .push(root.0.join("modified").to_string_lossy().into_owned());
        });
        manager
            .states
            .insert("all.path".to_string(), ServiceState::new());

        manager.start_path("all.path", &unit).await.unwrap();

        assert!(manager.states.get("all.path").unwrap().is_active());
        assert!(matches!(
            manager.start_path("all.path", &unit).await,
            Err(ManagerError::AlreadyActive(name)) if name == "all.path"
        ));
    }

    #[tokio::test]
    async fn start_path_without_watches_still_marks_unit_active() {
        let mut manager = Manager::new();
        let unit = path_unit("empty.path", |_| {});
        manager
            .states
            .insert("empty.path".to_string(), ServiceState::new());

        manager.start_path("empty.path", &unit).await.unwrap();

        assert!(manager.states.get("empty.path").unwrap().is_active());
    }

    #[tokio::test]
    async fn stop_path_requires_active_state_and_marks_stopped() {
        let mut manager = Manager::new();

        assert!(matches!(
            manager.stop_path("watch.path").await,
            Err(ManagerError::NotFound(name)) if name == "watch.path"
        ));

        manager
            .states
            .insert("watch.path".to_string(), ServiceState::new());
        assert!(matches!(
            manager.stop_path("watch.path").await,
            Err(ManagerError::NotActive(name)) if name == "watch.path"
        ));

        manager
            .states
            .get_mut("watch.path")
            .unwrap()
            .set_running(0);
        manager.stop_path("watch.path").await.unwrap();

        let state = manager.states.get("watch.path").unwrap();
        assert_eq!(state.active, ActiveState::Inactive);
        assert_eq!(state.sub, SubState::Exited);
    }

    #[test]
    fn take_path_receiver_returns_receiver_once() {
        let mut manager = Manager::new();

        assert!(manager.take_path_rx().is_some());
        assert!(manager.take_path_rx().is_none());
    }

    #[tokio::test]
    async fn handle_path_triggered_skips_active_service_and_reports_missing_service() {
        let mut manager = Manager::new();
        manager.units.insert(
            "ready.service".to_string(),
            Unit::Service(Service::new("ready.service".to_string())),
        );
        manager
            .states
            .insert("ready.service".to_string(), ServiceState::new());
        manager
            .states
            .get_mut("ready.service")
            .unwrap()
            .set_running(42);

        manager
            .handle_path_triggered(path_watcher::PathTriggered {
                path_name: "ready.path".to_string(),
                service_name: "ready.service".to_string(),
                triggered_path: "/tmp/ready".to_string(),
            })
            .await
            .unwrap();

        let err = manager
            .handle_path_triggered(path_watcher::PathTriggered {
                path_name: "missing.path".to_string(),
                service_name: "missing.service".to_string(),
                triggered_path: "/tmp/missing".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ManagerError::NotFound(name) if name == "missing.service"));
    }
}
