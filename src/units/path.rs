//! Path unit definitions
//!
//! Path units watch for file system changes and activate associated units
//! when specified paths exist, change, or become non-empty.

use super::service::{InstallSection, UnitSection};

/// A parsed .path unit
#[derive(Debug, Clone)]
pub struct Path {
    pub name: String,
    pub unit: UnitSection,
    pub path: PathSection,
    pub install: InstallSection,
}

/// The [Path] section of a path unit
#[derive(Debug, Clone, Default)]
pub struct PathSection {
    /// Watch for path existence
    pub path_exists: Vec<String>,
    /// Watch for path existence with glob
    pub path_exists_glob: Vec<String>,
    /// Watch for path changes (attribute or content)
    pub path_changed: Vec<String>,
    /// Watch for path modifications (content only)
    pub path_modified: Vec<String>,
    /// Watch for directory to become non-empty
    pub directory_not_empty: Vec<String>,
    /// The unit to activate (defaults to same name with .service)
    pub unit: Option<String>,
    /// Create the watched directory if it doesn't exist
    pub make_directory: bool,
    /// Mode for created directory
    pub directory_mode: Option<u32>,
    /// Trigger regardless of any other dependency
    pub trigger_limit_interval_sec: Option<std::time::Duration>,
    /// Maximum triggers within the interval
    pub trigger_limit_burst: Option<u32>,
}

impl Path {
    pub fn new(name: String) -> Self {
        Self {
            name,
            unit: UnitSection::default(),
            path: PathSection::default(),
            install: InstallSection::default(),
        }
    }

    /// Get the unit this path activates (defaults to same name with .service)
    pub fn activated_unit(&self) -> String {
        self.path.unit.clone().unwrap_or_else(|| {
            self.name
                .strip_suffix(".path")
                .map(|n| format!("{}.service", n))
                .unwrap_or_else(|| format!("{}.service", self.name))
        })
    }

    /// Check if any watch condition is configured
    pub fn has_watches(&self) -> bool {
        !self.path.path_exists.is_empty()
            || !self.path.path_exists_glob.is_empty()
            || !self.path.path_changed.is_empty()
            || !self.path.path_modified.is_empty()
            || !self.path.directory_not_empty.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn new_sets_defaults_and_activated_unit_falls_back_to_service_name() {
        let unit = Path::new("watch.path".to_string());

        assert_eq!(unit.name, "watch.path");
        assert_eq!(unit.activated_unit(), "watch.service");
        assert!(!unit.has_watches());
    }

    #[test]
    fn activated_unit_uses_explicit_unit_and_detects_all_watch_types() {
        let mut unit = Path::new("watch.path".to_string());
        unit.path.unit = Some("custom.service".to_string());
        unit.path.path_exists.push("/tmp/ready".to_string());
        unit.path.path_exists_glob.push("/tmp/*.ready".to_string());
        unit.path.path_changed.push("/etc/example.conf".to_string());
        unit.path.path_modified.push("/var/lib/state".to_string());
        unit.path
            .directory_not_empty
            .push("/var/spool/jobs".to_string());
        unit.path.make_directory = true;
        unit.path.directory_mode = Some(0o750);
        unit.path.trigger_limit_interval_sec = Some(Duration::from_secs(60));
        unit.path.trigger_limit_burst = Some(3);

        assert_eq!(unit.activated_unit(), "custom.service");
        assert!(unit.has_watches());
        assert!(unit.path.make_directory);
        assert_eq!(unit.path.directory_mode, Some(0o750));
        assert_eq!(
            unit.path.trigger_limit_interval_sec,
            Some(Duration::from_secs(60))
        );
        assert_eq!(unit.path.trigger_limit_burst, Some(3));
    }
}
