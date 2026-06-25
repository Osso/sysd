//! Slice unit operations
//!
//! Handles cgroup hierarchy organization through slice units.

use crate::units::Slice;

use super::{Manager, ManagerError};

impl Manager {
    /// Start a slice unit (create cgroup directory and mark active)
    pub(super) async fn start_slice(
        &mut self,
        name: &str,
        slice: &Slice,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        // Check if already active
        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        // Create cgroup directory for the slice
        let cgroup_path = slice.cgroup_path();
        log::info!("Starting slice {} (cgroup: {})", name, cgroup_path);

        if let Some(ref cgroup_mgr) = self.cgroup_manager {
            // Create the cgroup directory
            let path = std::path::Path::new(&cgroup_path);
            if !path.exists() {
                if let Err(e) = std::fs::create_dir_all(path) {
                    log::warn!("Failed to create cgroup dir {}: {}", cgroup_path, e);
                } else {
                    log::debug!("Created cgroup directory {}", cgroup_path);
                }
            }
            // Note: We don't need to move any processes - slices just organize the hierarchy
            let _ = cgroup_mgr; // silence unused warning
        }

        // Mark as active immediately (slices have no process)
        if let Some(state) = self.states.get_mut(name) {
            state.set_running(0);
        }

        log::info!("{} reached", name);
        Ok(())
    }

    /// Stop a slice unit (mark inactive, optionally clean up cgroup)
    pub(super) async fn stop_slice(
        &mut self,
        name: &str,
        slice: &Slice,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        let cgroup_path = slice.cgroup_path();
        log::info!("Stopping slice {} (cgroup: {})", name, cgroup_path);

        // Note: We don't remove cgroup directories on slice stop
        // The cgroup may still contain running services
        // Cleanup happens when the cgroup becomes empty

        if let Some(state) = self.states.get_mut(name) {
            state.set_stopped(0);
        }

        log::info!("{} stopped", name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{ActiveState, ServiceState};

    fn manager_with_slice(name: &str) -> (Manager, Slice) {
        let mut manager = Manager::new_user();
        manager
            .states
            .insert(name.to_string(), ServiceState::new());
        (manager, Slice::new(name.to_string()))
    }

    #[tokio::test]
    async fn start_slice_marks_slice_active_and_rejects_invalid_state() {
        let (mut manager, slice) = manager_with_slice("demo.slice");

        assert!(matches!(
            manager.start_slice("missing.slice", &slice).await,
            Err(ManagerError::NotFound(name)) if name == "missing.slice"
        ));

        manager.start_slice("demo.slice", &slice).await.unwrap();
        let state = manager.states.get("demo.slice").unwrap();
        assert_eq!(state.active, ActiveState::Active);

        assert!(matches!(
            manager.start_slice("demo.slice", &slice).await,
            Err(ManagerError::AlreadyActive(name)) if name == "demo.slice"
        ));
    }

    #[tokio::test]
    async fn stop_slice_requires_active_state_and_marks_slice_inactive() {
        let (mut manager, slice) = manager_with_slice("demo.slice");

        assert!(matches!(
            manager.stop_slice("missing.slice", &slice).await,
            Err(ManagerError::NotFound(name)) if name == "missing.slice"
        ));
        assert!(matches!(
            manager.stop_slice("demo.slice", &slice).await,
            Err(ManagerError::NotActive(name)) if name == "demo.slice"
        ));

        manager
            .states
            .get_mut("demo.slice")
            .unwrap()
            .set_running(0);
        manager.stop_slice("demo.slice", &slice).await.unwrap();
        let state = manager.states.get("demo.slice").unwrap();
        assert_eq!(state.active, ActiveState::Inactive);
    }
}
