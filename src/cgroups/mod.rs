//! Cgroup v2 management
//!
//! Creates and manages the cgroup hierarchy:
//!
//! /sys/fs/cgroup/
//! ├── system.slice/           # System services
//! │   ├── docker.service/
//! │   └── nginx.service/
//! └── user.slice/             # User sessions (managed by logind)
//!     └── user-1000.slice/
//!         ├── session-1.scope/    # Login session
//!         └── user@1000.service/  # User manager

use std::io;
use std::path::{Path, PathBuf};

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const SYSTEM_SLICE: &str = "system.slice";

#[derive(Clone)]
pub struct CgroupManager {
    root: PathBuf,
}

impl Default for CgroupManager {
    fn default() -> Self {
        Self {
            root: PathBuf::from(CGROUP_ROOT),
        }
    }
}

impl CgroupManager {
    pub fn new() -> io::Result<Self> {
        let root = PathBuf::from(CGROUP_ROOT);

        // Verify cgroup2 is mounted
        if !root.join("cgroup.controllers").exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cgroup v2 not mounted at /sys/fs/cgroup",
            ));
        }

        Ok(Self { root })
    }

    /// Create a cgroup for a unit
    /// Returns the cgroup path
    pub fn create_cgroup(&self, slice: Option<&str>, unit_name: &str) -> io::Result<PathBuf> {
        let path = match slice {
            Some(s) => self.root.join(s).join(unit_name),
            None => self.root.join("system.slice").join(unit_name),
        };

        // Create parent slice if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        // Create the cgroup
        std::fs::create_dir_all(&path)?;

        log::info!("Created cgroup: {}", path.display());
        Ok(path)
    }

    /// Move a process into a cgroup
    pub fn add_pid(&self, cgroup_path: &Path, pid: u32) -> io::Result<()> {
        let procs_file = cgroup_path.join("cgroup.procs");
        std::fs::write(&procs_file, pid.to_string())?;
        log::debug!("Moved PID {} to {}", pid, cgroup_path.display());
        Ok(())
    }

    /// Move multiple processes into a cgroup
    pub fn add_pids(&self, cgroup_path: &Path, pids: &[u32]) -> io::Result<()> {
        for pid in pids {
            self.add_pid(cgroup_path, *pid)?;
        }
        Ok(())
    }

    /// Get all PIDs in a cgroup
    pub fn get_pids(&self, cgroup_path: &Path) -> io::Result<Vec<u32>> {
        let procs_file = cgroup_path.join("cgroup.procs");
        let content = std::fs::read_to_string(&procs_file)?;

        let pids: Vec<u32> = content
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();

        Ok(pids)
    }

    /// Check if cgroup is empty (no processes)
    pub fn is_empty(&self, cgroup_path: &Path) -> io::Result<bool> {
        let pids = self.get_pids(cgroup_path)?;
        Ok(pids.is_empty())
    }

    /// Remove an empty cgroup
    pub fn remove_cgroup(&self, cgroup_path: &Path) -> io::Result<()> {
        if !self.is_empty(cgroup_path)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cgroup is not empty",
            ));
        }

        std::fs::remove_dir(cgroup_path)?;
        log::info!("Removed cgroup: {}", cgroup_path.display());
        Ok(())
    }

    /// Set memory limit for a cgroup
    pub fn set_memory_max(&self, cgroup_path: &Path, bytes: u64) -> io::Result<()> {
        let file = cgroup_path.join("memory.max");
        std::fs::write(&file, bytes.to_string())?;
        Ok(())
    }

    /// Set CPU quota for a cgroup (percentage, e.g., 50 = 50%)
    pub fn set_cpu_quota(&self, cgroup_path: &Path, percent: u32) -> io::Result<()> {
        // CPU quota is expressed as microseconds per 100ms period
        // 50% = 50000us per 100000us period
        let quota_us = percent as u64 * 1000;
        let file = cgroup_path.join("cpu.max");
        std::fs::write(&file, format!("{} 100000", quota_us))?;
        Ok(())
    }

    /// Set max number of tasks/processes
    pub fn set_tasks_max(&self, cgroup_path: &Path, max: u64) -> io::Result<()> {
        let file = cgroup_path.join("pids.max");
        let value = if max == u64::MAX {
            "max".to_string()
        } else {
            max.to_string()
        };
        std::fs::write(&file, value)?;
        Ok(())
    }

    /// Watch for cgroup becoming empty (polls cgroup.events)
    /// Returns a channel that signals when the cgroup is empty
    pub fn watch_empty(
        &self,
        cgroup_path: PathBuf,
    ) -> io::Result<tokio::sync::oneshot::Receiver<()>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let events_file = cgroup_path.join("cgroup.events");

        tokio::spawn(async move {
            loop {
                if let Ok(content) = tokio::fs::read_to_string(&events_file).await {
                    if content.contains("populated 0") {
                        let _ = tx.send(());
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });

        Ok(rx)
    }
}

/// Resource limits for a cgroup
#[derive(Debug, Default, Clone)]
pub struct CgroupLimits {
    pub memory_max: Option<u64>, // bytes
    pub cpu_quota: Option<u32>,  // percentage
    pub tasks_max: Option<u32>,
    // Note: DeviceAllow is handled via mount namespace isolation in sandbox.rs
}

impl CgroupManager {
    /// Create a cgroup for a service, move the PID into it, and apply limits
    /// If slice is None, defaults to system.slice
    pub fn setup_service_cgroup(
        &self,
        service_name: &str,
        pid: u32,
        limits: &CgroupLimits,
        slice: Option<&str>,
    ) -> io::Result<PathBuf> {
        // Create the cgroup in the specified slice (or system.slice by default)
        let slice = slice.unwrap_or(SYSTEM_SLICE);
        let cgroup_path = self.create_cgroup(Some(slice), service_name)?;

        // Move the process into the cgroup
        self.add_pid(&cgroup_path, pid)?;

        // Apply resource limits
        if let Some(mem) = limits.memory_max {
            if let Err(e) = self.set_memory_max(&cgroup_path, mem) {
                log::warn!("Failed to set memory limit for {}: {}", service_name, e);
            }
        }

        if let Some(cpu) = limits.cpu_quota {
            if let Err(e) = self.set_cpu_quota(&cgroup_path, cpu) {
                log::warn!("Failed to set CPU quota for {}: {}", service_name, e);
            }
        }

        if let Some(tasks) = limits.tasks_max {
            if let Err(e) = self.set_tasks_max(&cgroup_path, tasks as u64) {
                log::warn!("Failed to set tasks limit for {}: {}", service_name, e);
            }
        }

        Ok(cgroup_path)
    }

    /// M19: Enable cgroup delegation for a service
    /// This allows the service to manage its own cgroup subtree
    pub fn enable_delegation(&self, cgroup_path: &Path) -> io::Result<()> {
        // Read available controllers from the cgroup
        let controllers_file = cgroup_path.join("cgroup.controllers");
        let available = std::fs::read_to_string(&controllers_file).unwrap_or_default();

        // Parse available controllers
        let controllers: Vec<&str> = available.split_whitespace().collect();
        if controllers.is_empty() {
            log::debug!(
                "No controllers available for delegation at {}",
                cgroup_path.display()
            );
            return Ok(());
        }

        // Enable all available controllers for subtree
        // Format: "+cpu +memory +io +pids" etc.
        let enable_str: String = controllers
            .iter()
            .map(|c| format!("+{}", c))
            .collect::<Vec<_>>()
            .join(" ");

        let subtree_control = cgroup_path.join("cgroup.subtree_control");
        if let Err(e) = std::fs::write(&subtree_control, &enable_str) {
            // May fail if controller not enabled in parent - this is okay
            log::debug!(
                "Could not enable delegation at {}: {} (tried: {})",
                cgroup_path.display(),
                e,
                enable_str
            );
        } else {
            log::debug!(
                "Enabled cgroup delegation at {}: {}",
                cgroup_path.display(),
                enable_str
            );
        }

        Ok(())
    }

    /// Clean up a service cgroup (remove if empty)
    /// If slice is None, defaults to system.slice
    pub fn cleanup_service_cgroup(&self, service_name: &str, slice: Option<&str>) -> io::Result<()> {
        let slice = slice.unwrap_or(SYSTEM_SLICE);
        let cgroup_path = self.root.join(slice).join(service_name);

        if !cgroup_path.exists() {
            return Ok(());
        }

        // Only remove if empty
        match self.is_empty(&cgroup_path) {
            Ok(true) => {
                self.remove_cgroup(&cgroup_path)?;
            }
            Ok(false) => {
                log::debug!(
                    "Cgroup {} not empty, skipping removal",
                    cgroup_path.display()
                );
            }
            Err(e) => {
                log::debug!("Could not check cgroup {}: {}", cgroup_path.display(), e);
            }
        }

        Ok(())
    }

    /// Get the cgroup path for a service
    pub fn service_cgroup_path(&self, service_name: &str) -> PathBuf {
        self.root.join(SYSTEM_SLICE).join(service_name)
    }
}

/// Create a scope for a logind session
pub async fn create_session_scope(
    cgroup_manager: &CgroupManager,
    name: &str,  // e.g., "session-1.scope"
    slice: &str, // e.g., "user-1000.slice"
    pids: &[u32],
    memory_max: Option<u64>,
) -> io::Result<PathBuf> {
    // Create the cgroup
    let cgroup_path = cgroup_manager.create_cgroup(Some(slice), name)?;

    // Set resource limits if specified
    if let Some(mem) = memory_max {
        cgroup_manager.set_memory_max(&cgroup_path, mem)?;
    }

    // Move initial processes into the scope
    cgroup_manager.add_pids(&cgroup_path, pids)?;

    Ok(cgroup_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cgroup_limits_default() {
        let limits = CgroupLimits::default();
        assert!(limits.memory_max.is_none());
        assert!(limits.cpu_quota.is_none());
        assert!(limits.tasks_max.is_none());
    }

    #[test]
    fn test_cgroup_limits_with_values() {
        let limits = CgroupLimits {
            memory_max: Some(1024 * 1024 * 1024), // 1GB
            cpu_quota: Some(50),                  // 50%
            tasks_max: Some(100),
        };
        assert_eq!(limits.memory_max, Some(1024 * 1024 * 1024));
        assert_eq!(limits.cpu_quota, Some(50));
        assert_eq!(limits.tasks_max, Some(100));
    }

    #[test]
    fn test_cgroup_manager_default() {
        let mgr = CgroupManager::default();
        assert_eq!(mgr.root, PathBuf::from("/sys/fs/cgroup"));
    }

    #[test]
    fn test_service_cgroup_path() {
        let mgr = CgroupManager::default();
        let path = mgr.service_cgroup_path("docker.service");
        assert_eq!(
            path,
            PathBuf::from("/sys/fs/cgroup/system.slice/docker.service")
        );
    }

    #[test]
    fn test_cgroup_manager_new_on_linux() {
        // This test verifies that CgroupManager::new() works on systems with cgroups
        let result = CgroupManager::new();
        // On most Linux systems this should succeed, but we don't fail the test
        // if cgroups aren't available (e.g., in containers)
        if result.is_ok() {
            let mgr = result.unwrap();
            assert_eq!(mgr.root, PathBuf::from("/sys/fs/cgroup"));
        }
    }
}
