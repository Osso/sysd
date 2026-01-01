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
pub mod fstab;
pub mod getty;
pub mod manager;
pub mod pid1;
pub mod protocol;
pub mod units;

// Re-exports for D-Bus interfaces
pub use units::{InstallSection, Service, ServiceSection, ServiceType, UnitSection};
