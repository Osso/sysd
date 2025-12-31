//! Slice unit definitions
//!
//! Slices organize the cgroup hierarchy and provide resource management
//! for groups of services. They only have [Unit] and [Install] sections.
//! Starting a slice creates its cgroup directory.

use super::service::UnitSection;

/// A parsed .slice unit
#[derive(Debug, Clone)]
pub struct Slice {
    pub name: String,
    pub unit: UnitSection,
}

impl Slice {
    pub fn new(name: String) -> Self {
        Self {
            name,
            unit: UnitSection::default(),
        }
    }

    /// Convert slice name to cgroup path
    /// e.g., "system.slice" -> "/sys/fs/cgroup/system.slice"
    /// e.g., "user-1000.slice" -> "/sys/fs/cgroup/user.slice/user-1000.slice"
    pub fn cgroup_path(&self) -> String {
        // Handle nested slices (e.g., user-1000.slice under user.slice)
        if self.name.starts_with("user-") && self.name != "user.slice" {
            format!("/sys/fs/cgroup/user.slice/{}", self.name)
        } else if self.name.starts_with("machine-") && self.name != "machine.slice" {
            format!("/sys/fs/cgroup/machine.slice/{}", self.name)
        } else if self.name.starts_with("system-") && self.name != "system.slice" {
            format!("/sys/fs/cgroup/system.slice/{}", self.name)
        } else {
            format!("/sys/fs/cgroup/{}", self.name)
        }
    }
}
