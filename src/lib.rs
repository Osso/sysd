//! sysd - Minimal systemd-compatible init
//!
//! A Rust implementation that:
//! - Parses systemd .service unit files
//! - Provides D-Bus interface compatible with systemd-logind
//! - Manages cgroups v2 for process containment
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │                      sysd                        │
//! ├─────────────────────────────────────────────────┤
//! │  Unit Parser  │  Service Manager │  D-Bus API   │
//! ├─────────────────────────────────────────────────┤
//! │               Cgroup Manager                     │
//! └─────────────────────────────────────────────────┘
//! ```

pub mod cgroups;
pub mod dbus;
pub mod units;

// Re-exports for D-Bus interfaces
pub use units::{Service, ServiceType, UnitSection, ServiceSection, InstallSection};

/// Runtime state for the service manager
pub mod runtime {
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Holds runtime state shared with D-Bus interfaces
    #[derive(Default)]
    pub struct RuntimeInfo {
        /// Loaded units by name
        pub units: HashMap<String, crate::Service>,
        // TODO: add process tracking, fd store, etc.
    }

    pub type SharedRuntime = Arc<RwLock<RuntimeInfo>>;

    impl RuntimeInfo {
        pub fn new() -> Self {
            Self::default()
        }
    }
}
