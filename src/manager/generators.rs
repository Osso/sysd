//! Built-in generator support
//!
//! Replaces systemd-fstab-generator and systemd-getty-generator with built-in parsing.

use std::path::Path;

use super::{Manager, ManagerError, ServiceState, Unit};

impl Manager {
    /// Load mount units from /etc/fstab
    ///
    /// Replaces systemd-fstab-generator - parses fstab directly and creates
    /// Mount units for entries that should be mounted at boot.
    pub fn load_fstab(&mut self) -> Result<usize, ManagerError> {
        self.load_fstab_from(Path::new("/etc/fstab"))
    }

    /// Load mount units from a specific fstab file (for testing)
    pub fn load_fstab_from(&mut self, path: &Path) -> Result<usize, ManagerError> {
        use crate::fstab::generate_mount_units;

        if !path.exists() {
            log::debug!("No fstab at {}, skipping", path.display());
            return Ok(0);
        }

        let mounts = generate_mount_units(path)?;
        let count = mounts.len();

        // Collect mount names to add to local-fs.target
        let mut local_fs_mounts = Vec::new();

        for mount in mounts {
            let name = mount.name.clone();

            // Skip if already loaded (e.g., from a .mount file)
            if self.units.contains_key(&name) {
                log::debug!("Mount {} already loaded, skipping fstab entry", name);
                continue;
            }

            log::debug!("Loading mount from fstab: {}", name);
            local_fs_mounts.push(name.clone());
            self.states.insert(name.clone(), ServiceState::new());
            self.units.insert(name, Unit::Mount(mount));
        }

        // Add fstab mounts to local-fs.target's requirements
        // This makes local-fs.target pull in these mounts during boot
        if !local_fs_mounts.is_empty() {
            if let Some(Unit::Target(ref mut target)) = self.units.get_mut("local-fs.target") {
                let mut existing_requires: std::collections::HashSet<String> =
                    target.unit.requires.iter().cloned().collect();
                for mount_name in &local_fs_mounts {
                    if existing_requires.insert(mount_name.clone()) {
                        target.unit.requires.push(mount_name.clone());
                    }
                }
                log::debug!(
                    "Added {} fstab mounts to local-fs.target requirements",
                    local_fs_mounts.len()
                );
            } else {
                // local-fs.target not loaded yet - store for later
                // For now, we'll load it first
                log::debug!("local-fs.target not loaded, fstab mounts may not be pulled in");
            }
        }

        log::info!("Loaded {} mount units from {}", count, path.display());
        Ok(count)
    }

    /// Load getty units from kernel command line (/proc/cmdline)
    ///
    /// Replaces systemd-getty-generator - parses console= parameters and creates
    /// getty services for serial and virtual consoles.
    pub fn load_gettys(&mut self) -> Result<usize, ManagerError> {
        self.load_gettys_from(Path::new("/proc/cmdline"))
    }

    /// Load getty units from a specific cmdline file (for testing)
    pub fn load_gettys_from(&mut self, path: &Path) -> Result<usize, ManagerError> {
        use crate::getty::generate_getty_services;

        if !path.exists() {
            log::debug!("No cmdline at {}, loading default gettys", path.display());
            return self.load_default_gettys();
        }

        let services = generate_getty_services(path)?;

        // If no console= parameters, load default virtual console gettys
        if services.is_empty() {
            log::debug!("No console= in cmdline, loading default gettys");
            return self.load_default_gettys();
        }

        let count = services.len();

        for svc in services {
            let name = svc.name.clone();

            // Skip if already loaded
            if self.units.contains_key(&name) {
                log::debug!("Getty {} already loaded, skipping", name);
                continue;
            }

            log::debug!("Loading getty from cmdline: {}", name);
            self.states.insert(name.clone(), ServiceState::new());
            self.units.insert(name, Unit::Service(svc));
        }

        log::info!("Loaded {} getty units from {}", count, path.display());
        Ok(count)
    }

    /// Load default virtual console gettys (tty1-tty6)
    pub(super) fn load_default_gettys(&mut self) -> Result<usize, ManagerError> {
        use crate::getty::generate_default_gettys;

        let services = generate_default_gettys();
        let count = services.len();

        for svc in services {
            let name = svc.name.clone();

            if self.units.contains_key(&name) {
                continue;
            }

            self.states.insert(name.clone(), ServiceState::new());
            self.units.insert(name, Unit::Service(svc));
        }

        log::info!("Loaded {} default getty units", count);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Target;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(label: &str) -> TempDir {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sysd-generators-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    #[test]
    fn load_fstab_skips_missing_file_and_loads_mount_units_into_local_fs_target() {
        let root = temp_dir("fstab");
        let fstab = root.0.join("fstab");
        let mut manager = Manager::new();
        manager.units.insert(
            "local-fs.target".to_string(),
            Unit::Target(Target::new("local-fs.target".to_string())),
        );

        assert_eq!(manager.load_fstab_from(&root.0.join("missing")).unwrap(), 0);

        std::fs::write(
            &fstab,
            "/dev/sda1 /boot ext4 defaults 0 2\n/dev/sda2 /home ext4 noauto 0 2\nserver:/share /mnt/share nfs4 _netdev 0 0\n",
        )
        .unwrap();

        assert_eq!(manager.load_fstab_from(&fstab).unwrap(), 2);
        assert!(matches!(manager.units.get("boot.mount"), Some(Unit::Mount(_))));
        assert!(matches!(
            manager.units.get("mnt-share.mount"),
            Some(Unit::Mount(_))
        ));
        assert!(!manager.units.contains_key("home.mount"));

        let local_fs = manager.units.get("local-fs.target").unwrap().as_target().unwrap();
        assert!(local_fs.unit.requires.contains(&"boot.mount".to_string()));
        assert!(local_fs.unit.requires.contains(&"mnt-share.mount".to_string()));
    }

    #[test]
    fn load_fstab_does_not_replace_existing_mount_units() {
        let root = temp_dir("fstab-existing");
        let fstab = root.0.join("fstab");
        let mut manager = Manager::new();
        let existing = crate::units::Mount::new("boot.mount".to_string());
        manager
            .units
            .insert("boot.mount".to_string(), Unit::Mount(existing));

        std::fs::write(&fstab, "/dev/sda1 /boot ext4 defaults 0 2\n").unwrap();

        assert_eq!(manager.load_fstab_from(&fstab).unwrap(), 1);
        assert!(!manager.states.contains_key("boot.mount"));
    }

    #[test]
    fn load_gettys_uses_defaults_for_missing_or_consoleless_cmdline() {
        let root = temp_dir("getty-defaults");
        let cmdline = root.0.join("cmdline");
        let mut manager = Manager::new();

        assert_eq!(manager.load_gettys_from(&root.0.join("missing")).unwrap(), 6);
        assert!(manager.units.contains_key("getty@tty1.service"));
        assert!(manager.units.contains_key("getty@tty6.service"));

        std::fs::write(&cmdline, "quiet splash").unwrap();
        assert_eq!(manager.load_gettys_from(&cmdline).unwrap(), 6);
        assert_eq!(
            manager
                .units
                .keys()
                .filter(|name| name.starts_with("getty@tty"))
                .count(),
            6
        );
    }

    #[test]
    fn load_gettys_from_cmdline_loads_serial_and_virtual_consoles() {
        let root = temp_dir("getty-cmdline");
        let cmdline = root.0.join("cmdline");
        let mut manager = Manager::new();
        std::fs::write(&cmdline, "console=ttyS0,115200 console=tty1").unwrap();

        assert_eq!(manager.load_gettys_from(&cmdline).unwrap(), 2);
        assert!(manager.units.contains_key("serial-getty@ttyS0.service"));
        assert!(manager.units.contains_key("getty@tty1.service"));
        assert!(manager.states.contains_key("serial-getty@ttyS0.service"));
        assert!(manager.states.contains_key("getty@tty1.service"));
    }
}
