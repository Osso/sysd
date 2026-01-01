//! Fstab parser - generates .mount units from /etc/fstab
//!
//! Replaces systemd-fstab-generator with built-in parsing.
//!
//! Fstab format:
//! ```text
//! # <file system>  <mount point>  <type>  <options>  <dump>  <pass>
//! UUID=xxx         /              ext4    defaults   0       1
//! /dev/sda1        /boot          ext4    defaults   0       2
//! ```

use std::path::Path;

use crate::units::{Mount, MountSection};

/// A parsed fstab entry
#[derive(Debug, Clone)]
pub struct FstabEntry {
    /// Device, UUID, LABEL, or path to mount
    pub fs_spec: String,
    /// Mount point path
    pub mount_point: String,
    /// Filesystem type (ext4, btrfs, swap, etc.)
    pub fs_type: String,
    /// Mount options (comma-separated)
    pub options: String,
    /// Dump frequency (0 = no backup, 1 = backup)
    pub dump: u8,
    /// Fsck pass number (0 = skip, 1 = root, 2 = other)
    pub pass: u8,
}

impl FstabEntry {
    /// Check if this is a swap entry
    pub fn is_swap(&self) -> bool {
        self.fs_type == "swap" || self.mount_point == "none" || self.mount_point == "swap"
    }

    /// Check if this should be mounted at boot (not noauto)
    pub fn is_auto(&self) -> bool {
        !self.options.split(',').any(|o| o.trim() == "noauto")
    }

    /// Check if this is a network mount (nfs, cifs, etc.)
    pub fn is_network(&self) -> bool {
        matches!(
            self.fs_type.as_str(),
            "nfs" | "nfs4" | "cifs" | "smbfs" | "ncpfs" | "fuse.sshfs"
        ) || self.options.split(',').any(|o| o.trim() == "_netdev")
    }

    /// Check if this is a bind mount
    pub fn is_bind(&self) -> bool {
        self.options.split(',').any(|o| o.trim() == "bind" || o.trim() == "rbind")
    }

    /// Convert to a Mount unit
    pub fn to_mount_unit(&self) -> Mount {
        let name = Mount::name_from_mount_point(&self.mount_point);
        let mut mount = Mount::new(name);

        // Set mount section
        mount.mount = MountSection {
            what: self.fs_spec.clone(),
            r#where: self.mount_point.clone(),
            fs_type: if self.fs_type == "auto" {
                None
            } else {
                Some(self.fs_type.clone())
            },
            options: if self.options == "defaults" {
                None
            } else {
                Some(self.options.clone())
            },
            ..MountSection::default()
        };

        // Set unit section
        mount.unit.description = Some(format!("Mount {}", self.mount_point));

        // Add dependencies based on mount type
        if self.is_network() {
            mount.unit.after.push("network-online.target".to_string());
            mount.unit.wants.push("network-online.target".to_string());
        }

        // Root filesystem comes first
        if self.mount_point == "/" {
            mount.unit.default_dependencies = false;
        } else {
            // Other mounts depend on local-fs-pre.target
            mount.unit.after.push("local-fs-pre.target".to_string());
        }

        // Bind mounts depend on source being mounted
        if self.is_bind() {
            // The source is the 'what' field for bind mounts
            let source_mount = Mount::name_from_mount_point(&self.fs_spec);
            mount.unit.requires.push(source_mount.clone());
            mount.unit.after.push(source_mount);
        }

        mount
    }
}

/// Parse /etc/fstab and return entries
pub fn parse_fstab(path: &Path) -> std::io::Result<Vec<FstabEntry>> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_fstab_content(&content))
}

/// Parse fstab content (for testing)
pub fn parse_fstab_content(content: &str) -> Vec<FstabEntry> {
    content
        .lines()
        .filter_map(|line| parse_fstab_line(line))
        .collect()
}

/// Parse a single fstab line
fn parse_fstab_line(line: &str) -> Option<FstabEntry> {
    let line = line.trim();

    // Skip empty lines and comments
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    // Split on whitespace
    let fields: Vec<&str> = line.split_whitespace().collect();

    // Need at least 4 fields (fs_spec, mount_point, type, options)
    if fields.len() < 4 {
        return None;
    }

    Some(FstabEntry {
        fs_spec: fields[0].to_string(),
        mount_point: fields[1].to_string(),
        fs_type: fields[2].to_string(),
        options: fields[3].to_string(),
        dump: fields.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
        pass: fields.get(5).and_then(|s| s.parse().ok()).unwrap_or(0),
    })
}

/// Generate mount units from /etc/fstab
/// Returns units that should be started at boot (excludes swap, noauto)
pub fn generate_mount_units(fstab_path: &Path) -> std::io::Result<Vec<Mount>> {
    let entries = parse_fstab(fstab_path)?;

    let mounts: Vec<Mount> = entries
        .into_iter()
        .filter(|e| !e.is_swap() && e.is_auto())
        .map(|e| e.to_mount_unit())
        .collect();

    Ok(mounts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_FSTAB: &str = r#"
# /etc/fstab: static file system information.
#
# <file system>  <mount point>  <type>  <options>       <dump>  <pass>

# Root filesystem
UUID=12345678-1234-1234-1234-123456789abc  /  ext4  defaults  0  1

# Boot partition
/dev/sda1  /boot  ext4  defaults  0  2

# Home with options
UUID=abcdef12-3456-7890-abcd-ef1234567890  /home  ext4  defaults,noatime  0  2

# Swap
/dev/sda2  none  swap  sw  0  0

# tmpfs
tmpfs  /tmp  tmpfs  defaults,noatime,mode=1777  0  0

# NFS mount
server:/export  /mnt/nfs  nfs  defaults,_netdev  0  0

# Noauto mount (should be skipped)
/dev/sdb1  /mnt/usb  ext4  noauto,user  0  0

# Bind mount
/home/user/data  /srv/data  none  bind  0  0
"#;

    #[test]
    fn test_parse_fstab_content() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);
        assert_eq!(entries.len(), 8);
    }

    #[test]
    fn test_parse_root_entry() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);
        let root = entries.iter().find(|e| e.mount_point == "/").unwrap();

        assert_eq!(root.fs_spec, "UUID=12345678-1234-1234-1234-123456789abc");
        assert_eq!(root.fs_type, "ext4");
        assert_eq!(root.options, "defaults");
        assert_eq!(root.dump, 0);
        assert_eq!(root.pass, 1);
    }

    #[test]
    fn test_is_swap() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);
        let swap = entries.iter().find(|e| e.fs_type == "swap").unwrap();
        assert!(swap.is_swap());

        let root = entries.iter().find(|e| e.mount_point == "/").unwrap();
        assert!(!root.is_swap());
    }

    #[test]
    fn test_is_auto() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);

        let root = entries.iter().find(|e| e.mount_point == "/").unwrap();
        assert!(root.is_auto());

        let usb = entries.iter().find(|e| e.mount_point == "/mnt/usb").unwrap();
        assert!(!usb.is_auto());
    }

    #[test]
    fn test_is_network() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);

        let nfs = entries.iter().find(|e| e.mount_point == "/mnt/nfs").unwrap();
        assert!(nfs.is_network());

        let root = entries.iter().find(|e| e.mount_point == "/").unwrap();
        assert!(!root.is_network());
    }

    #[test]
    fn test_is_bind() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);

        let bind = entries.iter().find(|e| e.mount_point == "/srv/data").unwrap();
        assert!(bind.is_bind());

        let root = entries.iter().find(|e| e.mount_point == "/").unwrap();
        assert!(!root.is_bind());
    }

    #[test]
    fn test_to_mount_unit_root() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);
        let root = entries.iter().find(|e| e.mount_point == "/").unwrap();
        let unit = root.to_mount_unit();

        assert_eq!(unit.name, "-.mount");
        assert_eq!(unit.mount.what, "UUID=12345678-1234-1234-1234-123456789abc");
        assert_eq!(unit.mount.r#where, "/");
        assert_eq!(unit.mount.fs_type, Some("ext4".to_string()));
        assert!(unit.mount.options.is_none()); // defaults is removed
        assert!(!unit.unit.default_dependencies); // Root has no default deps
    }

    #[test]
    fn test_to_mount_unit_home() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);
        let home = entries.iter().find(|e| e.mount_point == "/home").unwrap();
        let unit = home.to_mount_unit();

        assert_eq!(unit.name, "home.mount");
        assert_eq!(unit.mount.r#where, "/home");
        assert_eq!(unit.mount.options, Some("defaults,noatime".to_string()));
        assert!(unit.unit.after.contains(&"local-fs-pre.target".to_string()));
    }

    #[test]
    fn test_to_mount_unit_nfs() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);
        let nfs = entries.iter().find(|e| e.mount_point == "/mnt/nfs").unwrap();
        let unit = nfs.to_mount_unit();

        assert_eq!(unit.name, "mnt-nfs.mount");
        assert!(unit.unit.after.contains(&"network-online.target".to_string()));
        assert!(unit.unit.wants.contains(&"network-online.target".to_string()));
    }

    #[test]
    fn test_to_mount_unit_bind() {
        let entries = parse_fstab_content(SAMPLE_FSTAB);
        let bind = entries.iter().find(|e| e.mount_point == "/srv/data").unwrap();
        let unit = bind.to_mount_unit();

        assert_eq!(unit.name, "srv-data.mount");
        // Bind mounts require the source to be mounted
        assert!(unit.unit.requires.contains(&"home-user-data.mount".to_string()));
        assert!(unit.unit.after.contains(&"home-user-data.mount".to_string()));
    }

    #[test]
    fn test_generate_mount_units_filters() {
        // Create a temp fstab
        let temp_dir = std::env::temp_dir();
        let fstab_path = temp_dir.join("test_fstab");
        std::fs::write(&fstab_path, SAMPLE_FSTAB).unwrap();

        let units = generate_mount_units(&fstab_path).unwrap();

        // Should exclude swap and noauto
        assert!(!units.iter().any(|u| u.mount.fs_type == Some("swap".to_string())));
        assert!(!units.iter().any(|u| u.name == "mnt-usb.mount"));

        // Should include others
        assert!(units.iter().any(|u| u.name == "-.mount"));
        assert!(units.iter().any(|u| u.name == "home.mount"));
        assert!(units.iter().any(|u| u.name == "tmp.mount"));

        std::fs::remove_file(&fstab_path).ok();
    }

    #[test]
    fn test_parse_minimal_entry() {
        let content = "/dev/sda1 /boot ext4 defaults";
        let entries = parse_fstab_content(content);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fs_spec, "/dev/sda1");
        assert_eq!(entries[0].mount_point, "/boot");
        assert_eq!(entries[0].fs_type, "ext4");
        assert_eq!(entries[0].options, "defaults");
        assert_eq!(entries[0].dump, 0);
        assert_eq!(entries[0].pass, 0);
    }

    #[test]
    fn test_skip_comments_and_empty() {
        let content = r#"
# This is a comment
   # Indented comment

/dev/sda1 /boot ext4 defaults

"#;
        let entries = parse_fstab_content(content);
        assert_eq!(entries.len(), 1);
    }
}
