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

use std::path::{Path, PathBuf};
use std::io;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";

pub struct CgroupManager {
    root: PathBuf,
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
        let value = if max == u64::MAX { "max".to_string() } else { max.to_string() };
        std::fs::write(&file, value)?;
        Ok(())
    }

    /// Watch for cgroup becoming empty (polls cgroup.events)
    /// Returns a channel that signals when the cgroup is empty
    pub fn watch_empty(&self, cgroup_path: PathBuf) -> io::Result<tokio::sync::oneshot::Receiver<()>> {
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

/// Create a scope for a logind session
pub async fn create_session_scope(
    cgroup_manager: &CgroupManager,
    name: &str,           // e.g., "session-1.scope"
    slice: &str,          // e.g., "user-1000.slice"
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
