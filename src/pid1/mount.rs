//! Essential filesystem mounting for PID 1
//!
//! Mounts the virtual filesystems required for a functioning Linux system:
//! - /proc (process information)
//! - /sys (sysfs)
//! - /dev (device nodes, devtmpfs)
//! - /run (runtime data)
//! - /sys/fs/cgroup (cgroup v2 unified hierarchy)

use nix::mount::{mount, MsFlags};
use std::fs;
use std::path::Path;

/// Mount information for an essential filesystem
struct MountPoint {
    source: &'static str,
    target: &'static str,
    fstype: &'static str,
    flags: MsFlags,
    data: Option<&'static str>,
}

/// Essential mounts required for boot
const ESSENTIAL_MOUNTS: &[MountPoint] = &[
    // /proc - process information
    MountPoint {
        source: "proc",
        target: "/proc",
        fstype: "proc",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NODEV).union(MsFlags::MS_NOEXEC),
        data: None,
    },
    // /sys - sysfs
    MountPoint {
        source: "sysfs",
        target: "/sys",
        fstype: "sysfs",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NODEV).union(MsFlags::MS_NOEXEC),
        data: None,
    },
    // /dev - device nodes (devtmpfs)
    MountPoint {
        source: "devtmpfs",
        target: "/dev",
        fstype: "devtmpfs",
        flags: MsFlags::MS_NOSUID,
        data: Some("mode=0755"),
    },
    // /dev/pts - pseudo-terminal devices
    MountPoint {
        source: "devpts",
        target: "/dev/pts",
        fstype: "devpts",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NOEXEC),
        data: Some("gid=5,mode=0620,ptmxmode=0666"),
    },
    // /dev/shm - shared memory
    MountPoint {
        source: "tmpfs",
        target: "/dev/shm",
        fstype: "tmpfs",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NODEV),
        data: Some("mode=1777"),
    },
    // /run - runtime data
    MountPoint {
        source: "tmpfs",
        target: "/run",
        fstype: "tmpfs",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NODEV),
        data: Some("mode=0755"),
    },
    // /sys/fs/cgroup - cgroup v2 unified hierarchy
    MountPoint {
        source: "cgroup2",
        target: "/sys/fs/cgroup",
        fstype: "cgroup2",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NODEV).union(MsFlags::MS_NOEXEC),
        data: None,
    },
];

/// Mount all essential filesystems
pub fn mount_essential_filesystems() -> Result<(), MountError> {
    for mp in ESSENTIAL_MOUNTS {
        mount_one(mp)?;
    }

    // Create essential directories in /run
    create_run_dirs()?;

    log::info!("Essential filesystems mounted");
    Ok(())
}

/// Mount a single filesystem
fn mount_one(mp: &MountPoint) -> Result<(), MountError> {
    let target = Path::new(mp.target);

    // Skip if already mounted (check if target has different device than parent)
    if is_mountpoint(target) {
        log::debug!("{} already mounted, skipping", mp.target);
        return Ok(());
    }

    // Create mount point if needed
    if !target.exists() {
        fs::create_dir_all(target).map_err(|e| MountError::CreateDir {
            path: mp.target.to_string(),
            source: e,
        })?;
    }

    // Mount the filesystem
    mount(
        Some(mp.source),
        target,
        Some(mp.fstype),
        mp.flags,
        mp.data,
    )
    .map_err(|e| MountError::Mount {
        target: mp.target.to_string(),
        fstype: mp.fstype.to_string(),
        source: e,
    })?;

    log::debug!("Mounted {} on {}", mp.fstype, mp.target);
    Ok(())
}

/// Check if a path is a mount point
fn is_mountpoint(path: &Path) -> bool {
    // Check /proc/mounts if available
    if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
        let path_str = path.to_string_lossy();
        for line in mounts.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == path_str {
                return true;
            }
        }
        return false;
    }

    // Fallback: compare device IDs (parent vs target)
    if !path.exists() {
        return false;
    }

    let parent = match path.parent() {
        Some(p) if p.exists() => p,
        _ => return false,
    };

    // Use stat to compare device IDs
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(path), fs::metadata(parent)) {
        (Ok(target_meta), Ok(parent_meta)) => target_meta.dev() != parent_meta.dev(),
        _ => false,
    }
}

/// Create essential directories under /run
fn create_run_dirs() -> Result<(), MountError> {
    let dirs = ["/run/lock", "/run/user"];
    for dir in dirs {
        let path = Path::new(dir);
        if !path.exists() {
            fs::create_dir_all(path).map_err(|e| MountError::CreateDir {
                path: dir.to_string(),
                source: e,
            })?;
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum MountError {
    #[error("Failed to create directory {path}: {source}")]
    CreateDir {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to mount {fstype} on {target}: {source}")]
    Mount {
        target: String,
        fstype: String,
        #[source]
        source: nix::Error,
    },
}
