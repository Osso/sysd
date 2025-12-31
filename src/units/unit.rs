//! Common unit type that wraps Service, Target, Mount, Slice, Socket, and Timer

use super::{InstallSection, Mount, Service, Slice, Socket, Target, Timer, UnitSection};

/// A unit can be a Service, Target, Mount, Slice, Socket, or Timer
#[derive(Debug, Clone)]
pub enum Unit {
    Service(Service),
    Target(Target),
    Mount(Mount),
    Slice(Slice),
    Socket(Socket),
    Timer(Timer),
}

impl Unit {
    /// Get the unit name
    pub fn name(&self) -> &str {
        match self {
            Unit::Service(s) => &s.name,
            Unit::Target(t) => &t.name,
            Unit::Mount(m) => &m.name,
            Unit::Slice(s) => &s.name,
            Unit::Socket(s) => &s.name,
            Unit::Timer(t) => &t.name,
        }
    }

    /// Get the [Unit] section (common to all types)
    pub fn unit_section(&self) -> &UnitSection {
        match self {
            Unit::Service(s) => &s.unit,
            Unit::Target(t) => &t.unit,
            Unit::Mount(m) => &m.unit,
            Unit::Slice(s) => &s.unit,
            Unit::Socket(s) => &s.unit,
            Unit::Timer(t) => &t.unit,
        }
    }

    /// Get the [Install] section
    pub fn install_section(&self) -> Option<&InstallSection> {
        match self {
            Unit::Service(s) => Some(&s.install),
            Unit::Target(_) | Unit::Slice(_) => None,
            Unit::Mount(m) => Some(&m.install),
            Unit::Socket(s) => Some(&s.install),
            Unit::Timer(t) => Some(&t.install),
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

    /// Check if this is a socket
    pub fn is_socket(&self) -> bool {
        matches!(self, Unit::Socket(_))
    }

    /// Check if this is a timer
    pub fn is_timer(&self) -> bool {
        matches!(self, Unit::Timer(_))
    }

    /// Get the unit type as a string (service, target, mount, slice, socket, timer)
    pub fn unit_type(&self) -> &'static str {
        match self {
            Unit::Service(_) => "service",
            Unit::Target(_) => "target",
            Unit::Mount(_) => "mount",
            Unit::Slice(_) => "slice",
            Unit::Socket(_) => "socket",
            Unit::Timer(_) => "timer",
        }
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

    /// Get as socket if it is one
    pub fn as_socket(&self) -> Option<&Socket> {
        match self {
            Unit::Socket(s) => Some(s),
            _ => None,
        }
    }

    /// Get as timer if it is one
    pub fn as_timer(&self) -> Option<&Timer> {
        match self {
            Unit::Timer(t) => Some(t),
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
            Unit::Service(_) | Unit::Mount(_) | Unit::Slice(_) | Unit::Socket(_) | Unit::Timer(_) => &[],
        }
    }
}
