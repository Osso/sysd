//! Common unit type that wraps Service, Target, Mount, Slice, Socket, Timer, and Path

use super::{InstallSection, Mount, PathUnit, Service, Slice, Socket, Target, Timer, UnitSection};

/// A unit can be a Service, Target, Mount, Slice, Socket, Timer, or Path
#[derive(Debug, Clone)]
pub enum Unit {
    Service(Service),
    Target(Target),
    Mount(Mount),
    Slice(Slice),
    Socket(Socket),
    Timer(Timer),
    Path(PathUnit),
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
            Unit::Path(p) => &p.name,
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
            Unit::Path(p) => &p.unit,
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
            Unit::Path(p) => Some(&p.install),
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

    /// Check if this is a path
    pub fn is_path(&self) -> bool {
        matches!(self, Unit::Path(_))
    }

    /// Get the unit type as a string (service, target, mount, slice, socket, timer, path)
    pub fn unit_type(&self) -> &'static str {
        match self {
            Unit::Service(_) => "service",
            Unit::Target(_) => "target",
            Unit::Mount(_) => "mount",
            Unit::Slice(_) => "slice",
            Unit::Socket(_) => "socket",
            Unit::Timer(_) => "timer",
            Unit::Path(_) => "path",
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

    /// Get as path if it is one
    pub fn as_path(&self) -> Option<&PathUnit> {
        match self {
            Unit::Path(p) => Some(p),
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
            Unit::Service(_)
            | Unit::Mount(_)
            | Unit::Slice(_)
            | Unit::Socket(_)
            | Unit::Timer(_)
            | Unit::Path(_) => &[],
        }
    }

    /// Set the unit name (used for template instantiation)
    /// For services, this also updates the instance field based on the new name
    pub fn set_name(&mut self, new_name: String) {
        match self {
            Unit::Service(s) => s.set_name(new_name),
            Unit::Target(t) => t.name = new_name,
            Unit::Mount(m) => m.name = new_name,
            Unit::Slice(sl) => sl.name = new_name,
            Unit::Socket(so) => so.set_name(new_name),
            Unit::Timer(t) => t.name = new_name,
            Unit::Path(p) => p.name = new_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_unit(name: &str) -> Unit {
        Unit::Service(Service::new(name.to_string()))
    }

    #[test]
    fn names_types_and_accessors_match_each_variant() {
        let units = vec![
            (service_unit("api.service"), "api.service", "service"),
            (
                Unit::Target(Target::new("multi-user.target".to_string())),
                "multi-user.target",
                "target",
            ),
            (
                Unit::Mount(Mount::new("var-lib.mount".to_string())),
                "var-lib.mount",
                "mount",
            ),
            (
                Unit::Slice(Slice::new("system.slice".to_string())),
                "system.slice",
                "slice",
            ),
            (
                Unit::Socket(Socket::new("api.socket".to_string())),
                "api.socket",
                "socket",
            ),
            (
                Unit::Timer(Timer::new("api.timer".to_string())),
                "api.timer",
                "timer",
            ),
            (
                Unit::Path(PathUnit::new("api.path".to_string())),
                "api.path",
                "path",
            ),
        ];

        for (unit, name, unit_type) in units {
            assert_eq!(unit.name(), name);
            assert_eq!(unit.unit_type(), unit_type);
            assert_eq!(unit.is_service(), unit_type == "service");
            assert_eq!(unit.is_target(), unit_type == "target");
            assert_eq!(unit.is_mount(), unit_type == "mount");
            assert_eq!(unit.is_slice(), unit_type == "slice");
            assert_eq!(unit.is_socket(), unit_type == "socket");
            assert_eq!(unit.is_timer(), unit_type == "timer");
            assert_eq!(unit.is_path(), unit_type == "path");
            assert_eq!(unit.as_service().is_some(), unit_type == "service");
            assert_eq!(unit.as_target().is_some(), unit_type == "target");
            assert_eq!(unit.as_mount().is_some(), unit_type == "mount");
            assert_eq!(unit.as_slice().is_some(), unit_type == "slice");
            assert_eq!(unit.as_socket().is_some(), unit_type == "socket");
            assert_eq!(unit.as_timer().is_some(), unit_type == "timer");
            assert_eq!(unit.as_path().is_some(), unit_type == "path");
        }
    }

    #[test]
    fn install_section_is_available_for_installable_variants_only() {
        assert!(service_unit("api.service").install_section().is_some());
        assert!(Unit::Mount(Mount::new("var-lib.mount".to_string()))
            .install_section()
            .is_some());
        assert!(Unit::Socket(Socket::new("api.socket".to_string()))
            .install_section()
            .is_some());
        assert!(Unit::Timer(Timer::new("api.timer".to_string()))
            .install_section()
            .is_some());
        assert!(Unit::Path(PathUnit::new("api.path".to_string()))
            .install_section()
            .is_some());
        assert!(Unit::Target(Target::new("multi-user.target".to_string()))
            .install_section()
            .is_none());
        assert!(Unit::Slice(Slice::new("system.slice".to_string()))
            .install_section()
            .is_none());
    }

    #[test]
    fn dependencies_chain_after_requires_and_wants() {
        let mut service = Service::new("api.service".to_string());
        service.unit.after = vec!["network.target".to_string()];
        service.unit.requires = vec!["dbus.service".to_string()];
        service.unit.wants = vec!["logger.service".to_string()];
        let unit = Unit::Service(service);

        let deps: Vec<&str> = unit
            .dependencies()
            .into_iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            deps,
            vec!["network.target", "dbus.service", "logger.service"]
        );
    }

    #[test]
    fn wants_dir_only_reports_target_wants() {
        let mut target = Target::new("multi-user.target".to_string());
        target.wants_dir = vec!["ssh.service".to_string()];

        assert_eq!(Unit::Target(target).wants_dir(), ["ssh.service"]);
        assert!(service_unit("api.service").wants_dir().is_empty());
    }

    #[test]
    fn set_name_updates_each_variant_and_service_instance() {
        let mut service = service_unit("worker@.service");
        service.set_name("worker@one.service".to_string());
        assert_eq!(service.name(), "worker@one.service");
        assert_eq!(
            service.as_service().unwrap().instance.as_deref(),
            Some("one")
        );

        let mut target = Unit::Target(Target::new("old.target".to_string()));
        target.set_name("new.target".to_string());
        assert_eq!(target.name(), "new.target");

        let mut mount = Unit::Mount(Mount::new("old.mount".to_string()));
        mount.set_name("new.mount".to_string());
        assert_eq!(mount.name(), "new.mount");

        let mut slice = Unit::Slice(Slice::new("old.slice".to_string()));
        slice.set_name("new.slice".to_string());
        assert_eq!(slice.name(), "new.slice");

        let mut socket = Unit::Socket(Socket::new("old.socket".to_string()));
        socket.set_name("new.socket".to_string());
        assert_eq!(socket.name(), "new.socket");

        let mut timer = Unit::Timer(Timer::new("old.timer".to_string()));
        timer.set_name("new.timer".to_string());
        assert_eq!(timer.name(), "new.timer");

        let mut path = Unit::Path(PathUnit::new("old.path".to_string()));
        path.set_name("new.path".to_string());
        assert_eq!(path.name(), "new.path");
    }
}
