//! Unit enable/disable operations
//!
//! Handles symlink creation/removal for WantedBy=, RequiredBy=, Also=, and Alias=.

use std::collections::HashSet;
use std::path::PathBuf;

use super::{Manager, ManagerError};

struct InstallInfo {
    also: Vec<String>,
    aliases: Vec<String>,
    wanted_by: Vec<String>,
    required_by: Vec<String>,
    unit_path: PathBuf,
}

impl Manager {
    async fn load_install_info(
        &mut self,
        unit_name: &str,
        warn_not_found: bool,
    ) -> Result<Option<InstallInfo>, ManagerError> {
        if !self.units.contains_key(unit_name) {
            match self.load(unit_name).await {
                Ok(_) => {}
                Err(ManagerError::NotFound(_)) => {
                    if warn_not_found {
                        log::warn!("Also= unit {} not found, skipping", unit_name);
                    } else {
                        log::debug!("Also= unit {} not found, skipping", unit_name);
                    }
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }

        let unit = self
            .units
            .get(unit_name)
            .ok_or_else(|| ManagerError::NotFound(unit_name.to_string()))?;

        let install = match unit.install_section() {
            Some(i) => i,
            None => {
                log::debug!("Unit {} has no Install section", unit_name);
                return Ok(None);
            }
        };

        Ok(Some(InstallInfo {
            also: install.also.clone(),
            aliases: install.alias.clone(),
            wanted_by: install.wanted_by.clone(),
            required_by: install.required_by.clone(),
            unit_path: self.find_unit(unit_name)?,
        }))
    }

    pub async fn enable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let mut created = Vec::new();
        let initial_name = self.normalize_name(name);
        let mut worklist = vec![initial_name.clone()];
        let mut visited: HashSet<String> = HashSet::new();
        let mut queued: HashSet<String> = [initial_name].into_iter().collect();

        while let Some(unit_name) = worklist.pop() {
            if !visited.insert(unit_name.clone()) {
                continue;
            }

            let info = match self.load_install_info(&unit_name, true).await? {
                Some(i) => i,
                None => continue,
            };

            if info.wanted_by.is_empty() && info.required_by.is_empty() && info.aliases.is_empty() {
                log::debug!("Unit {} has empty Install section", unit_name);
                continue;
            }

            for target in &info.wanted_by {
                created.push(self.create_dep_link(&unit_name, target, &info.unit_path, "wants")?);
            }
            for target in &info.required_by {
                created.push(self.create_dep_link(
                    &unit_name,
                    target,
                    &info.unit_path,
                    "requires",
                )?);
            }
            for alias in &info.aliases {
                created.push(self.create_alias_link(alias, &info.unit_path)?);
            }

            for also in info.also {
                if queued.insert(also.clone()) {
                    worklist.push(also);
                }
            }
        }

        if created.is_empty() {
            return Err(ManagerError::NoInstallSection(self.normalize_name(name)));
        }

        Ok(created)
    }

    pub async fn disable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let mut removed = Vec::new();
        let initial_name = self.normalize_name(name);
        let mut worklist = vec![initial_name.clone()];
        let mut visited: HashSet<String> = HashSet::new();
        let mut queued: HashSet<String> = [initial_name].into_iter().collect();

        while let Some(unit_name) = worklist.pop() {
            if !visited.insert(unit_name.clone()) {
                continue;
            }

            let info = match self.load_install_info(&unit_name, false).await? {
                Some(i) => i,
                None => continue,
            };

            for target in &info.wanted_by {
                if let Some(link) = self.remove_dep_link(&unit_name, target, "wants")? {
                    removed.push(link);
                }
            }
            for target in &info.required_by {
                if let Some(link) = self.remove_dep_link(&unit_name, target, "requires")? {
                    removed.push(link);
                }
            }
            for alias in &info.aliases {
                if let Some(link) = self.remove_alias_link(alias)? {
                    removed.push(link);
                }
            }

            for also in info.also {
                if queued.insert(also.clone()) {
                    worklist.push(also);
                }
            }
        }

        Ok(removed)
    }

    pub(super) fn create_dep_link(
        &self,
        unit_name: &str,
        target: &str,
        unit_path: &PathBuf,
        suffix: &str,
    ) -> Result<PathBuf, ManagerError> {
        let dir = self.enable_dir().join(format!("{}.{}", target, suffix));
        std::fs::create_dir_all(&dir).map_err(|e| ManagerError::Io(e.to_string()))?;

        let link_path = dir.join(unit_name);
        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    pub(super) fn remove_dep_link(
        &self,
        unit_name: &str,
        target: &str,
        suffix: &str,
    ) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = self
            .enable_dir()
            .join(format!("{}.{}", target, suffix))
            .join(unit_name);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    pub(super) fn create_alias_link(
        &self,
        alias: &str,
        unit_path: &PathBuf,
    ) -> Result<PathBuf, ManagerError> {
        let link_path = self.enable_dir().join(alias);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
        }

        std::os::unix::fs::symlink(unit_path, &link_path)
            .map_err(|e| ManagerError::Io(e.to_string()))?;

        Ok(link_path)
    }

    pub(super) fn remove_alias_link(&self, alias: &str) -> Result<Option<PathBuf>, ManagerError> {
        let link_path = self.enable_dir().join(alias);

        if link_path.exists() || link_path.is_symlink() {
            std::fs::remove_file(&link_path).map_err(|e| ManagerError::Io(e.to_string()))?;
            Ok(Some(link_path))
        } else {
            Ok(None)
        }
    }

    pub async fn is_enabled(&mut self, name: &str) -> Result<String, ManagerError> {
        let name = self.normalize_name(name);

        if !self.units.contains_key(&name) {
            self.load(&name).await?;
        }

        let unit = self
            .units
            .get(&name)
            .ok_or_else(|| ManagerError::NotFound(name.clone()))?;

        let Some(install) = unit.install_section() else {
            return Ok("static".to_string());
        };

        if install.wanted_by.is_empty()
            && install.required_by.is_empty()
            && install.alias.is_empty()
        {
            return Ok("static".to_string());
        }

        for target in &install.wanted_by {
            if self.has_enable_link(&name, &format!("{}.wants", target)) {
                return Ok("enabled".to_string());
            }
        }

        for target in &install.required_by {
            if self.has_enable_link(&name, &format!("{}.requires", target)) {
                return Ok("enabled".to_string());
            }
        }

        for alias in &install.alias {
            if self.has_enable_link(alias, "") {
                return Ok("enabled".to_string());
            }
        }

        Ok("disabled".to_string())
    }

    fn has_enable_link(&self, entry: &str, dir: &str) -> bool {
        let base = PathBuf::from("/etc/systemd/system");
        let link_path = if dir.is_empty() {
            base.join(entry)
        } else {
            base.join(dir).join(entry)
        };
        link_path.exists() || link_path.is_symlink()
    }
}
