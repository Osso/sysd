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
                for mount_name in &local_fs_mounts {
                    if !target.unit.requires.contains(mount_name) {
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
