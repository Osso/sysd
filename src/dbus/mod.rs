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
    /// Start the D-Bus server on the system bus
    pub async fn new(manager: Arc<RwLock<Manager>>) -> zbus::Result<Self> {
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
}
