//! Target unit definitions
//!
//! Targets are synchronization points that group services together.
//! They only have [Unit] and [Install] sections.

use super::service::UnitSection;

/// A parsed .target unit
#[derive(Debug, Clone)]
pub struct Target {
    pub name: String,
    pub unit: UnitSection,
    /// Services/targets pulled in by .wants directory
    pub wants_dir: Vec<String>,
}

impl Target {
    pub fn new(name: String) -> Self {
        Self {
            name,
            unit: UnitSection::default(),
            wants_dir: Vec::new(),
        }
    }
}
