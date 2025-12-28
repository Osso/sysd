//! Common unit type that wraps both Service and Target

use super::{Service, Target, UnitSection};

/// A unit can be either a Service or a Target
#[derive(Debug, Clone)]
pub enum Unit {
    Service(Service),
    Target(Target),
}

impl Unit {
    /// Get the unit name
    pub fn name(&self) -> &str {
        match self {
            Unit::Service(s) => &s.name,
            Unit::Target(t) => &t.name,
        }
    }

    /// Get the [Unit] section (common to both types)
    pub fn unit_section(&self) -> &UnitSection {
        match self {
            Unit::Service(s) => &s.unit,
            Unit::Target(t) => &t.unit,
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

    /// Get all units this depends on (After + Requires + Wants)
    pub fn dependencies(&self) -> Vec<&String> {
        let unit = self.unit_section();
        unit.after.iter()
            .chain(unit.requires.iter())
            .chain(unit.wants.iter())
            .collect()
    }

    /// Get units from .wants directory (for targets)
    pub fn wants_dir(&self) -> &[String] {
        match self {
            Unit::Target(t) => &t.wants_dir,
            Unit::Service(_) => &[],
        }
    }
}
