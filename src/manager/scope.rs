//! Scope management
//!
//! Handles transient scope units created by logind for session management.
//! Scopes don't have unit files - they're created at runtime via D-Bus.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::cgroups::{create_session_scope, CgroupManager};
use crate::dbus::scope::ScopeState;
use crate::dbus::unit::UnitState;
use crate::dbus::{unit_object_path, ScopeInterface, UnitInterface};
use crate::manager::ManagerError;

/// Manages transient scope units
pub struct ScopeManager {
    /// Active scopes (scope name -> cgroup path)
    scopes: HashMap<String, PathBuf>,
    /// D-Bus connection for registering scope objects
    dbus_connection: Option<zbus::Connection>,
    /// Cgroup manager reference
    cgroup_manager: Option<Arc<CgroupManager>>,
}

impl ScopeManager {
    pub fn new(cgroup_manager: Option<CgroupManager>) -> Self {
        Self {
            scopes: HashMap::new(),
            dbus_connection: None,
            cgroup_manager: cgroup_manager.map(Arc::new),
        }
    }

    /// Set the D-Bus connection for scope registration
    pub fn set_dbus_connection(&mut self, conn: zbus::Connection) {
        self.dbus_connection = Some(conn);
    }

    /// Get the D-Bus connection
    pub fn dbus_connection(&self) -> Option<&zbus::Connection> {
        self.dbus_connection.as_ref()
    }

    /// Get the cgroup manager
    pub fn cgroup_manager(&self) -> Option<&CgroupManager> {
        self.cgroup_manager.as_deref()
    }

    /// Check if a scope exists
    pub fn exists(&self, name: &str) -> bool {
        self.scopes.contains_key(name)
    }

    /// Get scope cgroup path
    pub fn get_cgroup_path(&self, name: &str) -> Option<&PathBuf> {
        self.scopes.get(name)
    }

    /// List all active scopes
    pub fn list(&self) -> impl Iterator<Item = (&String, &PathBuf)> {
        self.scopes.iter()
    }

    /// Register a transient scope (called by D-Bus StartTransientUnit)
    ///
    /// Creates the cgroup, moves PIDs, registers D-Bus objects, and tracks the scope.
    pub async fn register(
        &mut self,
        name: &str,
        slice: Option<&str>,
        description: Option<&str>,
        pids: &[u32],
    ) -> Result<PathBuf, ManagerError> {
        let slice = slice.unwrap_or("user.slice");

        // Create cgroup and move PIDs
        let cgroup_path = if let Some(cgroup_mgr) = &self.cgroup_manager {
            create_session_scope(cgroup_mgr, name, slice, pids, None).await?
        } else {
            // No cgroup manager - create a fake path for tracking
            log::warn!(
                "No cgroup manager available, scope {} will not have cgroup isolation",
                name
            );
            PathBuf::from(format!("/sys/fs/cgroup/{}/{}", slice, name))
        };

        // Register D-Bus objects if connection available
        if let Some(conn) = &self.dbus_connection {
            let desc = description.unwrap_or(name).to_string();

            // Create unit state (active since scope is running)
            let unit_state: Arc<RwLock<UnitState>> =
                Arc::new(RwLock::new(UnitState::new(name.to_string(), desc.clone())));
            {
                let mut state: tokio::sync::RwLockWriteGuard<'_, UnitState> =
                    unit_state.write().await;
                state.set_active();
            }
            let unit_iface = UnitInterface::new(unit_state);

            // Create scope state
            let scope_state: Arc<RwLock<ScopeState>> = Arc::new(RwLock::new(ScopeState {
                name: name.to_string(),
                cgroup_path: cgroup_path.to_string_lossy().to_string(),
                abandoned: false,
            }));
            let cgroup_mgr_arc = self
                .cgroup_manager
                .clone()
                .unwrap_or_else(|| Arc::new(CgroupManager::default()));
            let scope_iface = ScopeInterface::new(scope_state, cgroup_mgr_arc);

            // Register at the unit's D-Bus path
            let path = unit_object_path(name);
            let obj_path = zbus::zvariant::ObjectPath::try_from(path.as_str())
                .map_err(|e| ManagerError::StartFailed(e.to_string()))?;

            let server = conn.object_server();
            let _: bool = server
                .at(obj_path.clone(), unit_iface)
                .await
                .map_err(|e| {
                    ManagerError::StartFailed(format!("Failed to register Unit interface: {}", e))
                })?;
            let _: bool = server.at(obj_path, scope_iface).await.map_err(|e| {
                ManagerError::StartFailed(format!("Failed to register Scope interface: {}", e))
            })?;

            log::info!("Registered D-Bus objects for scope {}", name);
        }

        // Track the scope
        self.scopes.insert(name.to_string(), cgroup_path.clone());

        log::info!("Scope {} created at {}", name, cgroup_path.display());
        Ok(cgroup_path)
    }

    /// Unregister a scope (called when scope is abandoned or empty)
    pub async fn unregister(&mut self, name: &str) -> Result<(), ManagerError> {
        // Remove from tracking
        self.scopes.remove(name);

        // Unregister D-Bus objects
        if let Some(conn) = &self.dbus_connection {
            let path = unit_object_path(name);
            let obj_path = zbus::zvariant::ObjectPath::try_from(path.as_str())
                .map_err(|e| ManagerError::StopFailed(e.to_string()))?;

            let server = conn.object_server();
            let _ = server.remove::<UnitInterface, _>(obj_path.clone()).await;
            let _ = server.remove::<ScopeInterface, _>(obj_path).await;

            log::info!("Unregistered D-Bus objects for scope {}", name);
        }

        // Clean up cgroup if it exists and is empty
        if let Some(cgroup_mgr) = &self.cgroup_manager {
            let cgroup_path = PathBuf::from(format!("/sys/fs/cgroup/user.slice/{}", name));
            if cgroup_path.exists() {
                if let Err(e) = cgroup_mgr.remove_cgroup(&cgroup_path) {
                    log::debug!("Could not remove cgroup for {}: {}", name, e);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_manager_new() {
        let mgr = ScopeManager::new(None);
        assert!(mgr.scopes.is_empty());
        assert!(mgr.dbus_connection.is_none());
    }

    #[test]
    fn test_scope_exists() {
        let mut mgr = ScopeManager::new(None);
        assert!(!mgr.exists("session-1.scope"));

        mgr.scopes
            .insert("session-1.scope".to_string(), PathBuf::from("/test"));
        assert!(mgr.exists("session-1.scope"));
    }

    #[test]
    fn test_scope_get_cgroup_path() {
        let mut mgr = ScopeManager::new(None);
        assert!(mgr.get_cgroup_path("session-1.scope").is_none());

        let path = PathBuf::from("/sys/fs/cgroup/user.slice/session-1.scope");
        mgr.scopes.insert("session-1.scope".to_string(), path.clone());
        assert_eq!(mgr.get_cgroup_path("session-1.scope"), Some(&path));
    }

    #[test]
    fn test_scope_list() {
        let mut mgr = ScopeManager::new(None);
        assert_eq!(mgr.list().count(), 0);

        mgr.scopes.insert(
            "session-1.scope".to_string(),
            PathBuf::from("/sys/fs/cgroup/user.slice/session-1.scope"),
        );
        mgr.scopes.insert(
            "session-2.scope".to_string(),
            PathBuf::from("/sys/fs/cgroup/user.slice/session-2.scope"),
        );
        assert_eq!(mgr.list().count(), 2);
    }

    #[test]
    fn test_scope_manager_no_dbus_connection() {
        let mgr = ScopeManager::new(None);
        assert!(mgr.dbus_connection().is_none());
    }

    #[test]
    fn test_scope_manager_no_cgroup_manager() {
        let mgr = ScopeManager::new(None);
        assert!(mgr.cgroup_manager().is_none());
    }
}
