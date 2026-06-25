// Mount unit operations
//
// Handles mounting and unmounting filesystems.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use crate::units::Mount;

use super::{Manager, ManagerError};

/// Write to kernel log (/dev/kmsg) - survives better than filesystem logs during early boot
fn mount_kmsg(msg: &str) {
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
        match parse_mount_option(opt.trim()) {
            ParsedMountOption::Flag(flag) => flags |= flag,
            ParsedMountOption::Data(option) => data_options.push(option),
            ParsedMountOption::Graceful(option) => {
                graceful_options.push(option);
                data_options.push(option);
            }
            ParsedMountOption::Ignore => {}
        }
    }

    (flags, data_options, graceful_options)
}

enum ParsedMountOption<'a> {
    Flag(nix::mount::MsFlags),
    Data(&'a str),
    Graceful(&'a str),
    Ignore,
}

fn parse_mount_option(option: &str) -> ParsedMountOption<'_> {
    use nix::mount::MsFlags;
    match option {
        "ro" | "read-only" => ParsedMountOption::Flag(MsFlags::MS_RDONLY),
        "rw" | "defaults" => ParsedMountOption::Ignore,
        "nosuid" => ParsedMountOption::Flag(MsFlags::MS_NOSUID),
        "nodev" => ParsedMountOption::Flag(MsFlags::MS_NODEV),
        "noexec" => ParsedMountOption::Flag(MsFlags::MS_NOEXEC),
        "noatime" => ParsedMountOption::Flag(MsFlags::MS_NOATIME),
        "nodiratime" => ParsedMountOption::Flag(MsFlags::MS_NODIRATIME),
        "relatime" => ParsedMountOption::Flag(MsFlags::MS_RELATIME),
        "strictatime" => ParsedMountOption::Flag(MsFlags::MS_STRICTATIME),
        "sync" => ParsedMountOption::Flag(MsFlags::MS_SYNCHRONOUS),
        "dirsync" => ParsedMountOption::Flag(MsFlags::MS_DIRSYNC),
        "silent" => ParsedMountOption::Flag(MsFlags::MS_SILENT),
        "bind" => ParsedMountOption::Flag(MsFlags::MS_BIND),
        "move" => ParsedMountOption::Flag(MsFlags::MS_MOVE),
        "remount" => ParsedMountOption::Flag(MsFlags::MS_REMOUNT),
        _ => parse_nonstandard_mount_option(option),
    }
}

fn parse_nonstandard_mount_option(option: &str) -> ParsedMountOption<'_> {
    if let Some(graceful_option) = option.strip_prefix("x-systemd.graceful-option=") {
        return ParsedMountOption::Graceful(graceful_option);
    }
    if should_ignore_mount_option(option) {
        return ParsedMountOption::Ignore;
    }
    ParsedMountOption::Data(option)
}

fn should_ignore_mount_option(option: &str) -> bool {
    option.starts_with("x-systemd.")
        || option.starts_with("x-")
        || option == "_netdev"
        || option == "nofail"
        || option.starts_with("comment=")
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
    let result = run_mount(what, mount_point, fs_type, flags, data_options);
    if should_retry_without_graceful(&result, graceful_options) {
        log::info!(
            "{}: mount failed, retrying without graceful options: {:?}",
            name,
            graceful_options
        );
        let filtered = filter_out_graceful_options(data_options, graceful_options);
        return run_mount(what, mount_point, fs_type, flags, &filtered);
    }
    result
}

fn run_mount(
    what: &str,
    mount_point: &str,
    fs_type: &str,
    flags: nix::mount::MsFlags,
    data_options: &[&str],
) -> nix::Result<()> {
    use nix::mount::mount;
    let data = mount_data_string(data_options);
    mount(
        Some(what),
        mount_point,
        Some(fs_type),
        flags,
        data.as_deref(),
    )
}

fn mount_data_string(options: &[&str]) -> Option<String> {
    if options.is_empty() {
        None
    } else {
        Some(options.join(","))
    }
}

fn should_retry_without_graceful(result: &nix::Result<()>, graceful_options: &[&str]) -> bool {
    matches!(result, Err(nix::errno::Errno::EINVAL)) && !graceful_options.is_empty()
}

fn filter_out_graceful_options<'a>(
    data_options: &'a [&'a str],
    graceful_options: &[&str],
) -> Vec<&'a str> {
    data_options
        .iter()
        .filter(|option| !graceful_options.contains(option))
        .copied()
        .collect()
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
            mount_kmsg(&format!("MOUNT FAILED: {}", msg));
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
            mount_kmsg(&format!(
                "{} already mounted at {}, skipping",
                name, mount_point
            ));
            log::info!("{} already mounted at {}", name, mount_point);
            if let Some(state) = self.states.get_mut(name) {
                state.set_running(0);
            }
            return Ok(());
        }
        mount_kmsg(&format!(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{ActiveState, ServiceState, SubState};
    use crate::units::Mount;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(label: &str) -> TempDir {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("sysd-mount-{label}-{id}"));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn mount_unit(name: &str, mount_point: &str) -> Mount {
        let mut mount = Mount::new(name.to_string());
        mount.mount.what = "tmpfs".to_string();
        mount.mount.r#where = mount_point.to_string();
        mount.mount.fs_type = Some("tmpfs".to_string());
        mount
    }

    fn states_with_mount(name: &str) -> HashMap<String, ServiceState> {
        let mut states = HashMap::new();
        states.insert(name.to_string(), ServiceState::new());
        states
    }

    #[test]
    fn parse_mount_options_separates_flags_data_graceful_and_ignored_values() {
        let (flags, data, graceful) = parse_mount_options(
            "ro,nosuid,nodev,noexec,noatime,nodiratime,relatime,strictatime,sync,dirsync,silent,bind,move,remount,size=64m,x-systemd.graceful-option=nosymfollow,nofail,x-comment,_netdev,comment=demo",
        );

        assert!(flags.contains(nix::mount::MsFlags::MS_RDONLY));
        assert!(flags.contains(nix::mount::MsFlags::MS_NOSUID));
        assert!(flags.contains(nix::mount::MsFlags::MS_NODEV));
        assert!(flags.contains(nix::mount::MsFlags::MS_NOEXEC));
        assert!(flags.contains(nix::mount::MsFlags::MS_NOATIME));
        assert!(flags.contains(nix::mount::MsFlags::MS_NODIRATIME));
        assert!(flags.contains(nix::mount::MsFlags::MS_RELATIME));
        assert!(flags.contains(nix::mount::MsFlags::MS_STRICTATIME));
        assert!(flags.contains(nix::mount::MsFlags::MS_SYNCHRONOUS));
        assert!(flags.contains(nix::mount::MsFlags::MS_DIRSYNC));
        assert!(flags.contains(nix::mount::MsFlags::MS_SILENT));
        assert!(flags.contains(nix::mount::MsFlags::MS_BIND));
        assert!(flags.contains(nix::mount::MsFlags::MS_MOVE));
        assert!(flags.contains(nix::mount::MsFlags::MS_REMOUNT));
        assert_eq!(data, ["size=64m", "nosymfollow"]);
        assert_eq!(graceful, ["nosymfollow"]);
    }

    #[test]
    fn parse_single_mount_option_classifies_known_and_nonstandard_values() {
        assert!(matches!(
            parse_mount_option("defaults"),
            ParsedMountOption::Ignore
        ));
        assert!(matches!(
            parse_mount_option("nofail"),
            ParsedMountOption::Ignore
        ));
        assert!(matches!(
            parse_mount_option("x-systemd.requires=network.target"),
            ParsedMountOption::Ignore
        ));
        assert!(matches!(
            parse_mount_option("x-systemd.graceful-option=nosymfollow"),
            ParsedMountOption::Graceful("nosymfollow")
        ));
        assert!(matches!(
            parse_mount_option("mode=0755"),
            ParsedMountOption::Data("mode=0755")
        ));
    }

    #[test]
    fn mount_data_and_graceful_helpers_filter_retry_options() {
        let result: nix::Result<()> = Err(nix::errno::Errno::EINVAL);
        let other_error: nix::Result<()> = Err(nix::errno::Errno::EPERM);

        assert_eq!(mount_data_string(&[]), None);
        assert_eq!(mount_data_string(&["size=1m", "mode=0755"]), Some("size=1m,mode=0755".to_string()));
        assert!(should_retry_without_graceful(&result, &["nosymfollow"]));
        assert!(!should_retry_without_graceful(&other_error, &["nosymfollow"]));
        assert!(!should_retry_without_graceful(&result, &[]));
        assert_eq!(
            filter_out_graceful_options(&["size=1m", "nosymfollow", "mode=0755"], &["nosymfollow"]),
            ["size=1m", "mode=0755"]
        );
    }

    #[test]
    fn finalize_mount_and_umount_results_update_state_and_report_errors() {
        let mut states = states_with_mount("demo.mount");

        assert!(
            finalize_mount_result("demo.mount", "tmpfs", "/tmp/demo", Ok(()), &mut states)
                .is_ok()
        );
        let state = states.get("demo.mount").unwrap();
        assert_eq!(state.active, ActiveState::Active);
        assert_eq!(state.sub, SubState::Running);

        let error = finalize_mount_result(
            "demo.mount",
            "tmpfs",
            "/tmp/demo",
            Err(nix::errno::Errno::EINVAL),
            &mut states,
        )
        .unwrap_err();
        assert!(matches!(error, ManagerError::Io(message) if message.contains("failed")));
        assert_eq!(states.get("demo.mount").unwrap().active, ActiveState::Failed);

        assert!(finalize_umount_result("demo.mount", Ok(()), &mut states).is_ok());
        let state = states.get("demo.mount").unwrap();
        assert_eq!(state.active, ActiveState::Inactive);
        assert_eq!(state.sub, SubState::Exited);

        let error =
            finalize_umount_result("demo.mount", Err(nix::errno::Errno::EINVAL), &mut states)
                .unwrap_err();
        assert!(matches!(error, ManagerError::Io(message) if message.contains("umount failed")));
    }

    #[test]
    fn ensure_mount_directory_creates_missing_directory_with_mode() {
        let root = temp_dir("ensure-dir");
        let mount_point = root.0.join("nested/mount");

        ensure_mount_directory(mount_point.to_str().unwrap(), 0o750);

        let mode = std::fs::metadata(&mount_point)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[tokio::test]
    async fn start_mount_marks_already_mounted_root_active_without_mount_call() {
        let mut manager = Manager::new_user();
        manager
            .states
            .insert("root.mount".to_string(), ServiceState::new());
        let mount = mount_unit("root.mount", "/");

        assert!(manager.start_mount("root.mount", &mount).await.is_ok());

        let state = manager.states.get("root.mount").unwrap();
        assert_eq!(state.active, ActiveState::Active);
        assert_eq!(state.sub, SubState::Running);
    }

    #[tokio::test]
    async fn mount_start_stop_validate_state_before_privileged_operations() {
        let mut manager = Manager::new_user();
        let mount = mount_unit("missing.mount", "/definitely/not/mounted/sysd-test");

        assert!(matches!(
            manager.start_mount("missing.mount", &mount).await,
            Err(ManagerError::NotFound(name)) if name == "missing.mount"
        ));

        manager
            .states
            .insert("inactive.mount".to_string(), ServiceState::new());
        assert!(matches!(
            manager.stop_mount("inactive.mount", &mount).await,
            Err(ManagerError::NotActive(name)) if name == "inactive.mount"
        ));

        manager
            .states
            .get_mut("inactive.mount")
            .unwrap()
            .set_running(0);
        assert!(manager.stop_mount("inactive.mount", &mount).await.is_ok());
        assert_eq!(
            manager.states.get("inactive.mount").unwrap().active,
            ActiveState::Inactive
        );
    }

    #[test]
    fn is_mounted_handles_root_and_missing_paths() {
        assert!(is_mounted("/"));
        assert!(!is_mounted("/definitely/not/mounted/sysd-test"));
    }
}
