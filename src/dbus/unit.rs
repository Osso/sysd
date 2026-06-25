//! org.freedesktop.systemd1.Unit interface
//!
//! Properties that logind queries:
//! - ActiveState: "active", "inactive", "failed", etc.

use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::interface;

/// Runtime state for a unit's D-Bus interface
pub struct UnitState {
    pub name: String,
    pub description: String,
    pub active_state: String,
    pub sub_state: String,
}

impl UnitState {
    pub fn new(name: String, description: String) -> Self {
        Self {
            name,
            description,
            active_state: "inactive".into(),
            sub_state: "dead".into(),
        }
    }

    pub fn set_active(&mut self) {
        self.active_state = "active".into();
        self.sub_state = "running".into();
    }

    pub fn set_inactive(&mut self) {
        self.active_state = "inactive".into();
        self.sub_state = "dead".into();
    }

    pub fn set_failed(&mut self) {
        self.active_state = "failed".into();
        self.sub_state = "failed".into();
    }
}

pub struct UnitInterface {
    state: Arc<RwLock<UnitState>>,
}

impl UnitInterface {
    pub fn new(state: Arc<RwLock<UnitState>>) -> Self {
        Self { state }
    }
}

#[interface(name = "org.freedesktop.systemd1.Unit")]
impl UnitInterface {
    /// Unit identifier (e.g., "docker.service")
    #[zbus(property)]
    async fn id(&self) -> String {
        self.state.read().await.name.clone()
    }

    /// Human-readable description
    #[zbus(property)]
    async fn description(&self) -> String {
        self.state.read().await.description.clone()
    }

    /// High-level state: "active", "inactive", "activating", "deactivating", "failed"
    /// This is what logind checks to see if a scope is running
    #[zbus(property)]
    async fn active_state(&self) -> String {
        self.state.read().await.active_state.clone()
    }

    /// More detailed state: "running", "dead", "failed", "waiting", etc.
    #[zbus(property)]
    async fn sub_state(&self) -> String {
        self.state.read().await.sub_state.clone()
    }

    /// Load state: "loaded", "not-found", "error", etc.
    #[zbus(property)]
    async fn load_state(&self) -> String {
        "loaded".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unit_state_transitions_and_interface_properties_are_reported() {
        let state = Arc::new(RwLock::new(UnitState::new(
            "demo.service".to_string(),
            "Demo Service".to_string(),
        )));
        let interface = UnitInterface::new(Arc::clone(&state));

        assert_eq!(interface.id().await, "demo.service");
        assert_eq!(interface.description().await, "Demo Service");
        assert_eq!(interface.active_state().await, "inactive");
        assert_eq!(interface.sub_state().await, "dead");
        assert_eq!(interface.load_state().await, "loaded");

        state.write().await.set_active();
        assert_eq!(interface.active_state().await, "active");
        assert_eq!(interface.sub_state().await, "running");

        state.write().await.set_failed();
        assert_eq!(interface.active_state().await, "failed");
        assert_eq!(interface.sub_state().await, "failed");

        state.write().await.set_inactive();
        assert_eq!(interface.active_state().await, "inactive");
        assert_eq!(interface.sub_state().await, "dead");
    }
}
