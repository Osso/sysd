//! Unit enable/disable operations
//!
//! Handles symlink creation/removal for WantedBy=, RequiredBy=, Also=, and Alias=.

use std::path::PathBuf;

use super::{Manager, ManagerError};

impl Manager {
    /// Enable a unit (create symlinks based on [Install] section)
    pub async fn enable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let mut created = Vec::new();
        let mut to_enable = vec![self.normalize_name(name)];
        let mut enabled: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(unit_name) = to_enable.pop() {
            if enabled.contains(&unit_name) {
                continue;
            }
            enabled.insert(unit_name.clone());

            // Load the unit to get its Install section
            if !self.units.contains_key(&unit_name) {
                match self.load(&unit_name).await {
                    Ok(_) => {}
                    Err(ManagerError::NotFound(_)) => {
                        log::warn!("Also= unit {} not found, skipping", unit_name);
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            let unit = self
                .units
                .get(&unit_name)
                .ok_or_else(|| ManagerError::NotFound(unit_name.clone()))?;

            let install = match unit.install_section() {
                Some(i) => i,
                None => {
                    log::debug!("Unit {} has no Install section", unit_name);
                    continue;
                }
            };

            if install.wanted_by.is_empty()
                && install.required_by.is_empty()
                && install.alias.is_empty()
            {
                log::debug!("Unit {} has empty Install section", unit_name);
                continue;
            }

            // Find the unit file path
            let unit_path = self.find_unit(&unit_name)?;

            // Clone lists to avoid borrow issues
            let also_units = install.also.clone();
            let aliases = install.alias.clone();
            let wanted_by = install.wanted_by.clone();
            let required_by = install.required_by.clone();

            // Create symlinks in .wants directories
            for target in &wanted_by {
                let link = self.create_wants_link(&unit_name, target, &unit_path)?;
                created.push(link);
            }

            // Create symlinks in .requires directories
            for target in &required_by {
                let link = self.create_requires_link(&unit_name, target, &unit_path)?;
                created.push(link);
            }

            // Create alias symlinks
            for alias in &aliases {
                let link = self.create_alias_link(alias, &unit_path)?;
                created.push(link);
            }

            // Queue Also= units for enabling
            for also in also_units {
                if !enabled.contains(&also) {
                    to_enable.push(also);
                }
            }
        }

        if created.is_empty() {
            return Err(ManagerError::NoInstallSection(self.normalize_name(name)));
        }

        Ok(created)
    }

    /// Disable a unit (remove symlinks)
    pub async fn disable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let mut removed = Vec::new();
        let mut to_disable = vec![self.normalize_name(name)];
        let mut disabled: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(unit_name) = to_disable.pop() {
            if disabled.contains(&unit_name) {
                continue;
            }
            disabled.insert(unit_name.clone());

            // Load to get Install section
            if !self.units.contains_key(&unit_name) {
                match self.load(&unit_name).await {
                    Ok(_) => {}
                    Err(ManagerError::NotFound(_)) => {
                        log::debug!("Also= unit {} not found, skipping", unit_name);
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            let unit = self
                .units
                .get(&unit_name)
                .ok_or_else(|| ManagerError::NotFound(unit_name.clone()))?;

            let install = match unit.install_section() {
                Some(i) => i,
                None => {
                    log::debug!("Unit {} has no Install section", unit_name);
                    continue;
                }
            };

            // Clone lists to avoid borrow issues
            let also_units = install.also.clone();
            let aliases = install.alias.clone();
            let wanted_by = install.wanted_by.clone();
            let required_by = install.required_by.clone();

            // Remove from .wants directories
            for target in &wanted_by {
                if let Some(link) = self.remove_wants_link(&unit_name, target)? {
                    removed.push(link);
                }
            }

            // Remove from .requires directories
            for target in &required_by {
                if let Some(link) = self.remove_requires_link(&unit_name, target)? {
                    removed.push(link);
                }
            }

            // Remove alias symlinks
            for alias in &aliases {
                if let Some(link) = self.remove_alias_link(alias)? {
                    removed.push(link);
                }
            }

            // Queue Also= units for disabling
            for also in also_units {
                if !disabled.contains(&also) {
                    to_disable.push(also);
                }
            }
        }

        Ok(removed)
    }

    /// Create a symlink in target.wants/
    pub(super) fn create_wants_link(
        &self,
        unit_name: &str,
        target: &str,
        unit_path: &PathBuf,
    ) -> Result<PathBuf, ManagerError> {
        let wants_dir = PathBuf::from("/etc/systemd/system").join(format!("{}.wants", target));
        std::fs::create_dir_all(&wants_dir).map_err(|e| ManagerError::Io(e.to_string()))?;

        let link_path = wants_dir.join(unit_name);
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    /// Create a symlink in target.requires/
    pub(super) fn create_requires_link(
        &self,
        unit_name: &str,
        target: &str,
        unit_path: &PathBuf,
    ) -> Result<PathBuf, ManagerError> {
        let requires_dir =
            PathBuf::from("/etc/systemd/system").join(format!("{}.requires", target));
        std::fs::create_dir_all(&requires_dir).map_err(|e| ManagerError::Io(e.to_string()))?;

        let link_path = requires_dir.join(unit_name);
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    /// Remove symlink from target.wants/
    pub(super) fn remove_wants_link(
        &self,
        unit_name: &str,
        target: &str,
    ) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system")
            .join(format!("{}.wants", target))
            .join(unit_name);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    /// Remove symlink from target.requires/
    pub(super) fn remove_requires_link(
        &self,
        unit_name: &str,
        target: &str,
    ) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system")
            .join(format!("{}.requires", target))
            .join(unit_name);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    /// Create an alias symlink (Alias= in [Install])
    pub(super) fn create_alias_link(
        &self,
        alias: &str,
        unit_path: &PathBuf,
    ) -> Result<PathBuf, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system").join(alias);

        // Remove existing if present
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    /// Remove an alias symlink
    pub(super) fn remove_alias_link(&self, alias: &str) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = PathBuf::from("/etc/systemd/system").join(alias);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    /// Check if a unit is enabled
    pub async fn is_enabled(&mut self, name: &str) -> Result<String, ManagerError> {
        let name = self.normalize_name(name);

        // Load to get Install section
        if !self.units.contains_key(&name) {
            self.load(&name).await?;
        }

        let unit = self
            .units
            .get(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        // No install section = static (can't be enabled/disabled)
        let Some(install) = unit.install_section() else {
            return Ok("static".to_string());
        };

        if install.wanted_by.is_empty()
            && install.required_by.is_empty()
            && install.alias.is_empty()
        {
            return Ok("static".to_string());
        }

        // Check if any symlinks exist
        for target in &install.wanted_by {
            let link_path = PathBuf::from("/etc/systemd/system")
                .join(format!("{}.wants", target))
                .join(&name);
            if link_path.exists() || link_path.is_symlink() {
                return Ok("enabled".to_string());
            }
        }

        for target in &install.required_by {
            let link_path = PathBuf::from("/etc/systemd/system")
                .join(format!("{}.requires", target))
                .join(&name);
            if link_path.exists() || link_path.is_symlink() {
                return Ok("enabled".to_string());
            }
        }

        // Check alias symlinks
        for alias in &install.alias {
            let link_path = PathBuf::from("/etc/systemd/system").join(alias);
            if link_path.exists() || link_path.is_symlink() {
                return Ok("enabled".to_string());
            }
        }

        Ok("disabled".to_string())
    }
}
