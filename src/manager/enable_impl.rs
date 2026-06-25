// Unit enable/disable operations
//
// Handles symlink creation/removal for WantedBy=, RequiredBy=, Also=, and Alias=.

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
    async fn ensure_install_unit_loaded(
        &mut self,
        unit_name: &str,
        warn_not_found: bool,
    ) -> Result<bool, ManagerError> {
        if self.units.contains_key(unit_name) {
            return Ok(true);
        }

        match self.load(unit_name).await {
            Ok(_) => Ok(true),
            Err(ManagerError::NotFound(_)) => {
                if warn_not_found {
                    log::warn!("Also= unit {} not found, skipping", unit_name);
                } else {
                    log::debug!("Also= unit {} not found, skipping", unit_name);
                }
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    async fn load_install_info(
        &mut self,
        unit_name: &str,
        warn_not_found: bool,
    ) -> Result<Option<InstallInfo>, ManagerError> {
        if !self
            .ensure_install_unit_loaded(unit_name, warn_not_found)
            .await?
        {
            return Ok(None);
        }

        let unit = self
            .units
            .get(unit_name)
            .ok_or_else(|| ManagerError::NotFound(unit_name.to_string()))?;

        let Some(install) = unit.install_section() else {
            log::debug!("Unit {} has no Install section", unit_name);
            return Ok(None);
        };

        Ok(Some(InstallInfo {
            also: install.also.clone(),
            aliases: install.alias.clone(),
            wanted_by: install.wanted_by.clone(),
            required_by: install.required_by.clone(),
            unit_path: self.find_unit(unit_name)?,
        }))
    }

    async fn collect_install_units(
        &mut self,
        initial_name: &str,
        warn_not_found: bool,
    ) -> Result<Vec<(String, InstallInfo)>, ManagerError> {
        let mut collected = Vec::new();
        let mut worklist = vec![initial_name.to_string()];
        let mut visited: HashSet<String> = HashSet::new();
        let mut queued: HashSet<String> = [initial_name.to_string()].into_iter().collect();

        while let Some(unit_name) = worklist.pop() {
            if !visited.insert(unit_name.clone()) {
                continue;
            }

            let info = match self.load_install_info(&unit_name, warn_not_found).await? {
                Some(i) => i,
                None => continue,
            };

            for also in &info.also {
                if queued.insert(also.clone()) {
                    worklist.push(also.clone());
                }
            }

            collected.push((unit_name, info));
        }

        Ok(collected)
    }

    fn install_has_links(info: &InstallInfo) -> bool {
        !(info.wanted_by.is_empty() && info.required_by.is_empty() && info.aliases.is_empty())
    }

    fn create_enable_links(
        &self,
        unit_name: &str,
        info: &InstallInfo,
    ) -> Result<Vec<PathBuf>, ManagerError> {
        let mut links = Vec::new();

        for target in &info.wanted_by {
            links.push(self.create_dep_link(unit_name, target, &info.unit_path, "wants")?);
        }
        for target in &info.required_by {
            links.push(self.create_dep_link(unit_name, target, &info.unit_path, "requires")?);
        }
        for alias in &info.aliases {
            links.push(self.create_alias_link(alias, &info.unit_path)?);
        }

        Ok(links)
    }

    fn remove_enable_links(
        &self,
        unit_name: &str,
        info: &InstallInfo,
    ) -> Result<Vec<PathBuf>, ManagerError> {
        let mut links = Vec::new();

        for target in &info.wanted_by {
            if let Some(link) = self.remove_dep_link(unit_name, target, "wants")? {
                links.push(link);
            }
        }
        for target in &info.required_by {
            if let Some(link) = self.remove_dep_link(unit_name, target, "requires")? {
                links.push(link);
            }
        }
        for alias in &info.aliases {
            if let Some(link) = self.remove_alias_link(alias)? {
                links.push(link);
            }
        }

        Ok(links)
    }

    pub async fn enable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let mut created = Vec::new();
        let initial_name = self.normalize_name(name);
        let units = self.collect_install_units(&initial_name, true).await?;

        for (unit_name, info) in units {
            if !Self::install_has_links(&info) {
                log::debug!("Unit {} has empty Install section", unit_name);
                continue;
            }

            created.extend(self.create_enable_links(&unit_name, &info)?);
        }

        if created.is_empty() {
            return Err(ManagerError::NoInstallSection(initial_name));
        }

        Ok(created)
    }

    pub async fn disable(&mut self, name: &str) -> Result<Vec<PathBuf>, ManagerError> {
        let mut removed = Vec::new();
        let initial_name = self.normalize_name(name);
        let units = self.collect_install_units(&initial_name, false).await?;

        for (unit_name, info) in units {
            removed.extend(self.remove_enable_links(&unit_name, &info)?);
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
        let base = self.enable_dir();
        let link_path = if dir.is_empty() {
            base.join(entry)
        } else {
            base.join(dir).join(entry)
        };
        link_path.exists() || link_path.is_symlink()
    }
}
