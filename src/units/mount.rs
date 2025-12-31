//! Mount unit definitions matching systemd .mount files
//!
//! Mount units control mount points on the system.
//! The unit name must correspond to the mount point path with slashes
//! replaced by dashes (e.g., /dev/hugepages → dev-hugepages.mount).

use super::{InstallSection, UnitSection};

/// [Mount] section - mount-specific configuration
#[derive(Debug, Clone)]
pub struct MountSection {
    /// What= - what to mount (device, path, or special filesystem)
    pub what: String,
    /// Where= - mount point path
    pub r#where: String,
    /// Type= - filesystem type (optional, auto-detected if not specified)
    pub fs_type: Option<String>,
    /// Options= - mount options (comma-separated)
    pub options: Option<String>,
    /// SloppyOptions= - ignore unknown mount options
    pub sloppy_options: bool,
    /// LazyUnmount= - use lazy unmount (MNT_DETACH)
    pub lazy_unmount: bool,
    /// ForceUnmount= - force unmount (MNT_FORCE)
    pub force_unmount: bool,
    /// ReadWriteOnly= - fail if can't mount read-write
    pub read_write_only: bool,
    /// DirectoryMode= - directory mode for mount point creation
    pub directory_mode: Option<u32>,
    /// TimeoutSec= - timeout for mount operation
    pub timeout_sec: Option<std::time::Duration>,
}

impl Default for MountSection {
    fn default() -> Self {
        Self {
            what: String::new(),
            r#where: String::new(),
            fs_type: None,
            options: None,
            sloppy_options: false,
            lazy_unmount: false,
            force_unmount: false,
            read_write_only: false,
            directory_mode: Some(0o755),
            timeout_sec: None,
        }
    }
}

/// Complete parsed mount unit
#[derive(Debug, Clone)]
pub struct Mount {
    pub name: String,
    pub unit: UnitSection,
    pub mount: MountSection,
    pub install: InstallSection,
}

impl Mount {
    pub fn new(name: String) -> Self {
        Self {
            name,
            unit: UnitSection::default(),
            mount: MountSection::default(),
            install: InstallSection::default(),
        }
    }

    /// Get the mount point path from the unit name
    /// e.g., "dev-hugepages.mount" → "/dev/hugepages"
    pub fn mount_point_from_name(name: &str) -> String {
        let name = name.strip_suffix(".mount").unwrap_or(name);
        if name == "-" {
            "/".to_string()
        } else {
            // Replace dashes with slashes, handling escaped dashes
            let mut result = String::from("/");
            let mut chars = name.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '-' {
                    result.push('/');
                } else if c == '\\' && chars.peek() == Some(&'-') {
                    // Escaped dash: \- → -
                    chars.next();
                    result.push('-');
                } else {
                    result.push(c);
                }
            }
            result
        }
    }

    /// Get the unit name from a mount point path
    /// e.g., "/dev/hugepages" → "dev-hugepages.mount"
    pub fn name_from_mount_point(path: &str) -> String {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            "-.mount".to_string()
        } else {
            // Replace slashes with dashes, escape existing dashes
            let escaped: String = path
                .chars()
                .map(|c| {
                    if c == '/' {
                        '-'
                    } else if c == '-' {
                        // Note: proper escaping would be \x2d but - is often used directly
                        '-'
                    } else {
                        c
                    }
                })
                .collect();
            format!("{}.mount", escaped)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mount_point_from_name() {
        assert_eq!(Mount::mount_point_from_name("-.mount"), "/");
        assert_eq!(
            Mount::mount_point_from_name("dev-hugepages.mount"),
            "/dev/hugepages"
        );
        assert_eq!(Mount::mount_point_from_name("tmp.mount"), "/tmp");
        assert_eq!(
            Mount::mount_point_from_name("sys-kernel-debug.mount"),
            "/sys/kernel/debug"
        );
        assert_eq!(
            Mount::mount_point_from_name("var-lib-docker.mount"),
            "/var/lib/docker"
        );
    }

    #[test]
    fn test_name_from_mount_point() {
        assert_eq!(Mount::name_from_mount_point("/"), "-.mount");
        assert_eq!(
            Mount::name_from_mount_point("/dev/hugepages"),
            "dev-hugepages.mount"
        );
        assert_eq!(Mount::name_from_mount_point("/tmp"), "tmp.mount");
        assert_eq!(
            Mount::name_from_mount_point("/sys/kernel/debug"),
            "sys-kernel-debug.mount"
        );
    }

    #[test]
    fn test_mount_default() {
        let mount = Mount::new("test.mount".to_string());
        assert_eq!(mount.name, "test.mount");
        assert!(mount.mount.what.is_empty());
        assert!(mount.mount.r#where.is_empty());
        assert_eq!(mount.mount.directory_mode, Some(0o755));
    }
}
