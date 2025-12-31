//! Common unit type that wraps Service, Target, Mount, and Slice

use super::{InstallSection, Mount, Service, Slice, Target, UnitSection};

/// A unit can be a Service, Target, Mount, or Slice
#[derive(Debug, Clone)]
pub enum Unit {
    Service(Service),
    Target(Target),
    Mount(Mount),
    Slice(Slice),
}

impl Unit {
    /// Get the unit name
    pub fn name(&self) -> &str {
        match self {
            Unit::Service(s) => &s.name,
            Unit::Target(t) => &t.name,
            Unit::Mount(m) => &m.name,
            Unit::Slice(s) => &s.name,
        }
    }

    /// Get the [Unit] section (common to all types)
    pub fn unit_section(&self) -> &UnitSection {
        match self {
            Unit::Service(s) => &s.unit,
            Unit::Target(t) => &t.unit,
            Unit::Mount(m) => &m.unit,
            Unit::Slice(s) => &s.unit,
        }
    }

    /// Get the [Install] section
    pub fn install_section(&self) -> Option<&InstallSection> {
        match self {
            Unit::Service(s) => Some(&s.install),
            Unit::Target(_) | Unit::Slice(_) => None, // Targets and slices don't have install sections
            Unit::Mount(m) => Some(&m.install),
        }
    }

    /// Check if this is a service
    pub fn is_service(&self) -> bool {
        matches!(self, Unit::Service(_))
    }

    /// Check if this is a target
    pub fn is_target(&self) -> bool {
        matches!(self, Unit::Target(_))
    }

    /// Check if this is a mount
    pub fn is_mount(&self) -> bool {
        matches!(self, Unit::Mount(_))
    }

    /// Check if this is a slice
    pub fn is_slice(&self) -> bool {
        matches!(self, Unit::Slice(_))
    }

    /// Get as service if it is one
    pub fn as_service(&self) -> Option<&Service> {
        match self {
            Unit::Service(s) => Some(s),
            _ => None,
        }
    }

    /// Get as target if it is one
    pub fn as_target(&self) -> Option<&Target> {
        match self {
            Unit::Target(t) => Some(t),
            _ => None,
        }
    }

    /// Get as mount if it is one
    pub fn as_mount(&self) -> Option<&Mount> {
        match self {
            Unit::Mount(m) => Some(m),
            _ => None,
        }
    }

    /// Get as slice if it is one
    pub fn as_slice(&self) -> Option<&Slice> {
        match self {
            Unit::Slice(s) => Some(s),
            _ => None,
        }
    }

    /// Get all units this depends on (After + Requires + Wants)
    pub fn dependencies(&self) -> Vec<&String> {
        let unit = self.unit_section();
        unit.after
            .iter()
            .chain(unit.requires.iter())
            .chain(unit.wants.iter())
            .collect()
    }

    /// Get units from .wants directory (for targets)
    pub fn wants_dir(&self) -> &[String] {
        match self {
            Unit::Target(t) => &t.wants_dir,
            Unit::Service(_) | Unit::Mount(_) | Unit::Slice(_) => &[],
        }
    }
}
