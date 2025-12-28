//! org.freedesktop.systemd1.Scope interface
//!
//! Scopes are transient units created at runtime (not from unit files).
//! logind creates session scopes to contain user login sessions.
//!
//! Key method:
//! - Abandon: Stop tracking the scope, let it die when empty

use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::{interface, fdo};

use crate::cgroups::CgroupManager;

/// State for a scope unit
pub struct ScopeState {
    pub name: String,
    pub cgroup_path: String,
    pub abandoned: bool,
}

pub struct ScopeInterface {
    state: Arc<RwLock<ScopeState>>,
    #[allow(dead_code)]
    cgroup_manager: Arc<CgroupManager>,
}

impl ScopeInterface {
    pub fn new(state: Arc<RwLock<ScopeState>>, cgroup_manager: Arc<CgroupManager>) -> Self {
        Self { state, cgroup_manager }
    }
}

#[interface(name = "org.freedesktop.systemd1.Scope")]
impl ScopeInterface {
    /// Stop tracking this scope
    ///
    /// After abandoning:
    /// - We stop monitoring the cgroup
    /// - The scope will be cleaned up when empty
    /// - logind calls this when a session ends
    async fn abandon(&self) -> fdo::Result<()> {
        let mut state = self.state.write().await;

        if state.abandoned {
            return Ok(());
        }

        log::info!("Abandoning scope: {}", state.name);
        state.abandoned = true;

        // Stop monitoring this cgroup for emptiness
        // The cgroup will be cleaned up by the kernel when all processes exit

        Ok(())
    }

    /// Controller (always "scope" for scopes)
    #[zbus(property)]
    async fn controller(&self) -> String {
        "scope".to_string()
    }
}
