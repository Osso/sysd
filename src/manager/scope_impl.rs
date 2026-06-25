// Scope management
//
// Handles transient scope units created by logind for session management.
// Scopes don't have unit files - they're created at runtime via D-Bus.

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
        let cgroup_path = create_scope_cgroup_path(&self.cgroup_manager, name, slice, pids).await?;
        if let Some(conn) = &self.dbus_connection {
            register_scope_dbus_objects(
                conn,
                &self.cgroup_manager,
                name,
                description,
                &cgroup_path,
            )
            .await?;
        }

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

async fn create_scope_cgroup_path(
    cgroup_manager: &Option<Arc<CgroupManager>>,
    name: &str,
    slice: &str,
    pids: &[u32],
) -> Result<PathBuf, ManagerError> {
    let Some(cgroup_manager) = cgroup_manager else {
        log::warn!(
            "No cgroup manager available, scope {} will not have cgroup isolation",
            name
        );
        return Ok(PathBuf::from(format!("/sys/fs/cgroup/{}/{}", slice, name)));
    };
    create_session_scope(cgroup_manager, name, slice, pids, None)
        .await
        .map_err(ManagerError::from)
}

async fn register_scope_dbus_objects(
    conn: &zbus::Connection,
    cgroup_manager: &Option<Arc<CgroupManager>>,
    name: &str,
    description: Option<&str>,
    cgroup_path: &PathBuf,
) -> Result<(), ManagerError> {
    let desc = description.unwrap_or(name).to_string();
    let unit_iface = build_scope_unit_interface(name, &desc).await;
    let scope_iface = build_scope_interface(name, cgroup_path, cgroup_manager);
    let path = unit_object_path(name);
    let obj_path = zbus::zvariant::ObjectPath::try_from(path.as_str())
        .map_err(|e| ManagerError::StartFailed(e.to_string()))?;
    register_scope_interfaces(conn, obj_path, unit_iface, scope_iface).await?;
    log::info!("Registered D-Bus objects for scope {}", name);
    Ok(())
}

async fn build_scope_unit_interface(name: &str, description: &str) -> UnitInterface {
    let unit_state = Arc::new(RwLock::new(UnitState::new(
        name.to_string(),
        description.to_string(),
    )));
    unit_state.write().await.set_active();
    UnitInterface::new(unit_state)
}

fn build_scope_interface(
    name: &str,
    cgroup_path: &PathBuf,
    cgroup_manager: &Option<Arc<CgroupManager>>,
) -> ScopeInterface {
    let scope_state = Arc::new(RwLock::new(ScopeState {
        name: name.to_string(),
        cgroup_path: cgroup_path.to_string_lossy().to_string(),
        abandoned: false,
    }));
    let cgroup_manager = cgroup_manager
        .clone()
        .unwrap_or_else(|| Arc::new(CgroupManager::default()));
    ScopeInterface::new(scope_state, cgroup_manager)
}

async fn register_scope_interfaces(
    conn: &zbus::Connection,
    obj_path: zbus::zvariant::ObjectPath<'_>,
    unit_iface: UnitInterface,
    scope_iface: ScopeInterface,
) -> Result<(), ManagerError> {
    let server = conn.object_server();
    let _: bool = server.at(obj_path.clone(), unit_iface).await.map_err(|e| {
        ManagerError::StartFailed(format!("Failed to register Unit interface: {}", e))
    })?;
    let _: bool = server.at(obj_path, scope_iface).await.map_err(|e| {
        ManagerError::StartFailed(format!("Failed to register Scope interface: {}", e))
    })?;
    Ok(())
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
        mgr.scopes
            .insert("session-1.scope".to_string(), path.clone());
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

    #[tokio::test]
    async fn register_without_cgroup_or_dbus_tracks_scope_with_default_slice() {
        let mut mgr = ScopeManager::new(None);

        let path = mgr
            .register("session-11.scope", None, Some("Session 11"), &[1000, 1001])
            .await
            .unwrap();

        assert_eq!(
            path,
            PathBuf::from("/sys/fs/cgroup/user.slice/session-11.scope")
        );
        assert!(mgr.exists("session-11.scope"));
        assert_eq!(mgr.get_cgroup_path("session-11.scope"), Some(&path));
    }

    #[tokio::test]
    async fn register_without_cgroup_uses_explicit_slice_and_unregister_removes_tracking() {
        let mut mgr = ScopeManager::new(None);

        let path = mgr
            .register("session-22.scope", Some("user-1000.slice"), None, &[])
            .await
            .unwrap();

        assert_eq!(
            path,
            PathBuf::from("/sys/fs/cgroup/user-1000.slice/session-22.scope")
        );
        assert!(mgr.exists("session-22.scope"));

        mgr.unregister("session-22.scope").await.unwrap();

        assert!(!mgr.exists("session-22.scope"));
        assert!(mgr.get_cgroup_path("session-22.scope").is_none());
    }

    #[tokio::test]
    async fn unregister_missing_scope_is_ok_without_dbus_or_cgroup_manager() {
        let mut mgr = ScopeManager::new(None);

        mgr.unregister("missing.scope").await.unwrap();

        assert!(mgr.scopes.is_empty());
    }

    #[tokio::test]
    async fn create_scope_cgroup_path_falls_back_when_cgroup_manager_is_absent() {
        let path =
            create_scope_cgroup_path(&None, "session-33.scope", "custom.slice", &[1, 2, 3])
                .await
                .unwrap();

        assert_eq!(
            path,
            PathBuf::from("/sys/fs/cgroup/custom.slice/session-33.scope")
        );
    }

    #[tokio::test]
    async fn scope_interface_builders_construct_unit_and_scope_interfaces() {
        let _unit_iface = build_scope_unit_interface("session-44.scope", "Session 44").await;
        let cgroup_path = PathBuf::from("/sys/fs/cgroup/user.slice/session-44.scope");
        let _scope_iface = build_scope_interface("session-44.scope", &cgroup_path, &None);
    }
}
