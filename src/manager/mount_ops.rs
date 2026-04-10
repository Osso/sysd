//! Mount unit operations
//!
//! Handles mounting and unmounting filesystems.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use crate::units::Mount;

use super::{Manager, ManagerError};

/// Write to kernel log (/dev/kmsg) - survives better than filesystem logs during early boot
fn kmsg(msg: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = writeln!(f, "sysd: {}", msg);
    }
    eprintln!("sysd: {}", msg);
}

fn ensure_mount_directory(mount_point: &str, mode: u32) {
    if !std::path::Path::new(mount_point).exists() {
        if let Err(e) = std::fs::create_dir_all(mount_point) {
            log::warn!("Failed to create mount point {}: {}", mount_point, e);
        } else if let Err(e) =
            std::fs::set_permissions(mount_point, std::fs::Permissions::from_mode(mode))
        {
            log::warn!("Failed to set permissions on {}: {}", mount_point, e);
        }
    }
}

fn parse_mount_options(options: &str) -> (nix::mount::MsFlags, Vec<&str>, Vec<&str>) {
    use nix::mount::MsFlags;

    let mut flags = MsFlags::empty();
    let mut data_options: Vec<&str> = Vec::new();
    let mut graceful_options: Vec<&str> = Vec::new();

    for opt in options.split(',') {
        let opt = opt.trim();
        match opt {
            "ro" | "read-only" => flags |= MsFlags::MS_RDONLY,
            "rw" => {}
            "nosuid" => flags |= MsFlags::MS_NOSUID,
            "nodev" => flags |= MsFlags::MS_NODEV,
            "noexec" => flags |= MsFlags::MS_NOEXEC,
            "noatime" => flags |= MsFlags::MS_NOATIME,
            "nodiratime" => flags |= MsFlags::MS_NODIRATIME,
            "relatime" => flags |= MsFlags::MS_RELATIME,
            "strictatime" => flags |= MsFlags::MS_STRICTATIME,
            "sync" => flags |= MsFlags::MS_SYNCHRONOUS,
            "dirsync" => flags |= MsFlags::MS_DIRSYNC,
            "silent" => flags |= MsFlags::MS_SILENT,
            "bind" => flags |= MsFlags::MS_BIND,
            "move" => flags |= MsFlags::MS_MOVE,
            "remount" => flags |= MsFlags::MS_REMOUNT,
            "defaults" => {}
            other => {
                if let Some(graceful_opt) = other.strip_prefix("x-systemd.graceful-option=") {
                    graceful_options.push(graceful_opt);
                    data_options.push(graceful_opt);
                    continue;
                }
                if other.starts_with("x-systemd.")
                    || other.starts_with("x-")
                    || other == "_netdev"
                    || other == "nofail"
                    || other.starts_with("comment=")
                {
                    continue;
                }
                data_options.push(other);
            }
        }
    }

    (flags, data_options, graceful_options)
}

fn mount_with_graceful_retry(
    what: &str,
    mount_point: &str,
    fs_type: &str,
    flags: nix::mount::MsFlags,
    data_options: &[&str],
    graceful_options: &[&str],
    name: &str,
) -> nix::Result<()> {
    use nix::mount::mount;

    let data = if data_options.is_empty() {
        None
    } else {
        Some(data_options.join(","))
    };

    let result = mount(
        Some(what),
        mount_point,
        Some(fs_type),
        flags,
        data.as_deref(),
    );

    if let Err(nix::errno::Errno::EINVAL) = result {
        if !graceful_options.is_empty() {
            log::info!(
                "{}: mount failed, retrying without graceful options: {:?}",
                name,
                graceful_options
            );
            let filtered: Vec<&str> = data_options
                .iter()
                .filter(|o| !graceful_options.contains(o))
                .copied()
                .collect();
            let data = if filtered.is_empty() {
                None
            } else {
                Some(filtered.join(","))
            };
            return mount(
                Some(what),
                mount_point,
                Some(fs_type),
                flags,
                data.as_deref(),
            );
        }
    }

    result
}

fn finalize_mount_result(
    name: &str,
    what: &str,
    mount_point: &str,
    result: nix::Result<()>,
    states: &mut std::collections::HashMap<String, super::ServiceState>,
) -> Result<(), ManagerError> {
    match result {
        Ok(()) => {
            log::info!("{} mounted successfully", name);
            if let Some(state) = states.get_mut(name) {
                state.set_running(0);
            }
            Ok(())
        }
        Err(e) => {
            let msg = format!("mount {} at {} failed: {}", what, mount_point, e);
            kmsg(&format!("MOUNT FAILED: {}", msg));
            log::error!("{}: {}", name, msg);
            if let Some(state) = states.get_mut(name) {
                state.set_failed(msg.clone());
            }
            Err(ManagerError::Io(msg))
        }
    }
}

fn finalize_umount_result(
    name: &str,
    result: nix::Result<()>,
    states: &mut std::collections::HashMap<String, super::ServiceState>,
) -> Result<(), ManagerError> {
    match result {
        Ok(()) => {
            log::info!("{} unmounted successfully", name);
            if let Some(state) = states.get_mut(name) {
                state.set_stopped(0);
            }
            Ok(())
        }
        Err(e) => {
            let msg = format!("umount failed: {}", e);
            log::error!("{}: {}", name, msg);
            if let Some(state) = states.get_mut(name) {
                state.set_failed(msg.clone());
            }
            Err(ManagerError::Io(msg))
        }
    }
}

impl Manager {
    /// Start a mount unit (execute mount operation)
    pub(super) async fn start_mount(
        &mut self,
        name: &str,
        mnt: &Mount,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        let mount_point = &mnt.mount.r#where;
        let what = &mnt.mount.what;
        let fs_type = mnt.mount.fs_type.as_deref().unwrap_or("auto");
        let options_raw = mnt.mount.options.as_deref().unwrap_or("defaults");
        let options = options_raw.replace("%%", "%");

        if let Some(mode) = mnt.mount.directory_mode {
            ensure_mount_directory(mount_point, mode);
        }

        if is_mounted(mount_point) {
            kmsg(&format!(
                "{} already mounted at {}, skipping",
                name, mount_point
            ));
            log::info!("{} already mounted at {}", name, mount_point);
            if let Some(state) = self.states.get_mut(name) {
                state.set_running(0);
            }
            return Ok(());
        }
        kmsg(&format!(
            "{} NOT mounted, will mount at {}",
            name, mount_point
        ));

        log::info!(
            "Mounting {} ({}) at {} with options {}",
            name,
            what,
            mount_point,
            options
        );

        let (flags, data_options, graceful_options) = parse_mount_options(&options);

        let result = mount_with_graceful_retry(
            what,
            mount_point,
            fs_type,
            flags,
            &data_options,
            &graceful_options,
            name,
        );

        finalize_mount_result(name, what, mount_point, result, &mut self.states)
    }

    /// Stop a mount unit (execute umount operation)
    pub(super) async fn stop_mount(&mut self, name: &str, mnt: &Mount) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        let mount_point = &mnt.mount.r#where;

        if !is_mounted(mount_point) {
            log::debug!("{} not mounted, marking inactive", name);
            if let Some(state) = self.states.get_mut(name) {
                state.set_stopped(0);
            }
            return Ok(());
        }

        log::info!("Unmounting {}", mount_point);

        use nix::mount::{umount2, MntFlags};

        let mut flags = MntFlags::empty();
        if mnt.mount.lazy_unmount {
            flags |= MntFlags::MNT_DETACH;
        }
        if mnt.mount.force_unmount {
            flags |= MntFlags::MNT_FORCE;
        }

        let result = umount2(mount_point.as_str(), flags);

        finalize_umount_result(name, result, &mut self.states)
    }
}

/// Check if a path is currently mounted (by reading /proc/mounts)
pub(super) fn is_mounted(path: &str) -> bool {
    let Ok(content) = std::fs::read_to_string("/proc/mounts") else {
        return false;
    };

    let normalized = if path == "/" {
        path.to_string()
    } else {
        path.trim_end_matches('/').to_string()
    };

    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let mount_point = parts[1];
            let mount_point = mount_point
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");

            let mount_normalized = if mount_point == "/" {
                mount_point
            } else {
                mount_point.trim_end_matches('/').to_string()
            };

            if mount_normalized == normalized {
                return true;
            }
        }
    }

    false
}
