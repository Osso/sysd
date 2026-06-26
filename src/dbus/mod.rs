//! D-Bus interface for systemd compatibility
//!
//! Implements org.freedesktop.systemd1 interfaces required for logind compatibility.
//!
//! Key interfaces:
//! - Manager: StartUnit, StopUnit, StartTransientUnit, etc.
//! - Unit: ActiveState, SubState properties
//! - Scope: Abandon method

mod manager;
pub mod scope;
pub mod unit;

pub use manager::ManagerInterface;
pub use scope::ScopeInterface;
pub use unit::UnitInterface;

use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::{connection::Builder, zvariant::ObjectPath, Connection};

use crate::manager::Manager;

/// D-Bus server state
pub struct DbusServer {
    connection: Connection,
}

impl DbusServer {
    /// Start the D-Bus server on the system bus (for system mode)
    pub async fn new(manager: Arc<RwLock<Manager>>) -> zbus::Result<Self> {
        Self::new_system(manager).await
    }

    /// Start the D-Bus server on the system bus
    pub async fn new_system(manager: Arc<RwLock<Manager>>) -> zbus::Result<Self> {
        let manager_iface = ManagerInterface::new(manager.clone());

        let connection = Builder::system()?
            .name("org.freedesktop.systemd1")?
            .serve_at("/org/freedesktop/systemd1", manager_iface)?
            .build()
            .await?;

        // Set the D-Bus connection on the Manager for scope registration
        {
            let mut mgr = manager.write().await;
            mgr.set_dbus_connection(connection.clone());
        }

        Ok(Self { connection })
    }

    /// Start the D-Bus server on the session bus (for user mode)
    ///
    /// In user mode, we connect to the session bus and provide the same
    /// org.freedesktop.systemd1 interface that user-level tools expect.
    pub async fn new_session(manager: Arc<RwLock<Manager>>) -> zbus::Result<Self> {
        let manager_iface = ManagerInterface::new(manager.clone());

        let connection = Builder::session()?
            .name("org.freedesktop.systemd1")?
            .serve_at("/org/freedesktop/systemd1", manager_iface)?
            .build()
            .await?;

        // Set the D-Bus connection on the Manager
        {
            let mut mgr = manager.write().await;
            mgr.set_dbus_connection(connection.clone());
        }

        Ok(Self { connection })
    }

    /// Get connection for registering dynamic unit objects
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Register a unit's D-Bus object path
    pub async fn register_unit(&self, unit_id: &str, iface: UnitInterface) -> zbus::Result<()> {
        let path = make_object_path(unit_id);
        self.connection.object_server().at(path, iface).await?;
        Ok(())
    }

    /// Register a scope's D-Bus object path (for transient scopes from logind)
    pub async fn register_scope(
        &self,
        scope_id: &str,
        unit_iface: UnitInterface,
        scope_iface: ScopeInterface,
    ) -> zbus::Result<()> {
        let path = make_object_path(scope_id);
        let server = self.connection.object_server();
        server.at(path.clone(), unit_iface).await?;
        server.at(path, scope_iface).await?;
        Ok(())
    }

    /// Unregister a unit when it's unloaded
    pub async fn unregister_unit(&self, unit_id: &str) -> zbus::Result<bool> {
        let path = make_object_path(unit_id);
        self.connection
            .object_server()
            .remove::<UnitInterface, _>(path)
            .await
    }
}

/// Convert unit name to D-Bus ObjectPath
fn make_object_path(unit_id: &str) -> ObjectPath<'static> {
    let path_str = unit_object_path(unit_id);
    ObjectPath::try_from(path_str).unwrap().into()
}

/// Convert unit name to D-Bus object path string
/// e.g., "docker.service" -> "/org/freedesktop/systemd1/unit/docker_2eservice"
pub fn unit_object_path(unit_id: &str) -> String {
    let escaped: String = unit_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_string()
            } else {
                format!("_{:02x}", c as u32)
            }
        })
        .collect();

    format!("/org/freedesktop/systemd1/unit/{}", escaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgroups::CgroupManager;
    use crate::dbus::scope::ScopeState;
    use crate::dbus::unit::UnitState;

    #[test]
    fn test_unit_object_path() {
        assert_eq!(
            unit_object_path("docker.service"),
            "/org/freedesktop/systemd1/unit/docker_2eservice"
        );
        assert_eq!(
            unit_object_path("user@1000.service"),
            "/org/freedesktop/systemd1/unit/user_401000_2eservice"
        );
        assert_eq!(
            unit_object_path("session-1.scope"),
            "/org/freedesktop/systemd1/unit/session_2d1_2escope"
        );
    }

    #[tokio::test]
    async fn dbus_server_registers_units_and_scopes_on_existing_connection() {
        let Ok(connection) = Connection::session().await else {
            return;
        };
        let server = DbusServer { connection };
        let unit_id = format!("sysd-test-{}.service", std::process::id());
        let scope_id = format!("sysd-test-{}.scope", std::process::id());
        let unit_state = Arc::new(RwLock::new(UnitState::new(
            unit_id.clone(),
            "Test Unit".to_string(),
        )));

        server
            .register_unit(&unit_id, UnitInterface::new(Arc::clone(&unit_state)))
            .await
            .unwrap();
        assert!(server.connection().unique_name().is_some());
        assert!(server.unregister_unit(&unit_id).await.unwrap());

        let scope_unit = UnitInterface::new(unit_state);
        let scope_state = Arc::new(RwLock::new(ScopeState {
            name: scope_id.clone(),
            cgroup_path: "/sys/fs/cgroup/user.slice/sysd-test.scope".to_string(),
            abandoned: false,
        }));
        let scope = ScopeInterface::new(scope_state, Arc::new(CgroupManager::default()));

        server
            .register_scope(&scope_id, scope_unit, scope)
            .await
            .unwrap();
        let path = make_object_path(&scope_id);
        let object_server = server.connection().object_server();
        let _ = object_server.remove::<UnitInterface, _>(path.clone()).await;
        let _ = object_server.remove::<ScopeInterface, _>(path).await;
    }

    #[tokio::test]
    async fn dbus_server_new_session_registers_manager_interface_when_available() {
        let manager = Arc::new(RwLock::new(Manager::new_user()));
        let Ok(server) = DbusServer::new_session(Arc::clone(&manager)).await else {
            return;
        };

        assert!(server.connection().unique_name().is_some());
        assert!(manager
            .read()
            .await
            .scope_manager()
            .dbus_connection()
            .is_some());
    }
}
