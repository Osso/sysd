//! Orderly shutdown sequence for PID 1
//!
//! Implements proper system shutdown/reboot:
//! 1. Stop all services in reverse dependency order
//! 2. Send SIGTERM to remaining processes
//! 3. Wait briefly for graceful exit
//! 4. Send SIGKILL to stragglers
//! 5. Sync filesystems
//! 6. Unmount filesystems
//! 7. Call reboot() syscall

use nix::sys::reboot::{reboot, RebootMode};
use nix::sys::signal::{kill, Signal};
use nix::unistd::{sync, Pid};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tokio::time::sleep;

/// Type of shutdown to perform
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownType {
    /// Power off the system
    Poweroff,
    /// Reboot the system
    Reboot,
    /// Halt (stop, don't power off)
    Halt,
}

impl ShutdownType {
    fn to_reboot_mode(self) -> RebootMode {
        match self {
            ShutdownType::Poweroff => RebootMode::RB_POWER_OFF,
            ShutdownType::Reboot => RebootMode::RB_AUTOBOOT,
            ShutdownType::Halt => RebootMode::RB_HALT_SYSTEM,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_type_maps_to_expected_reboot_modes() {
        assert_eq!(
            ShutdownType::Poweroff.to_reboot_mode(),
            RebootMode::RB_POWER_OFF
        );
        assert_eq!(
            ShutdownType::Reboot.to_reboot_mode(),
            RebootMode::RB_AUTOBOOT
        );
        assert_eq!(
            ShutdownType::Halt.to_reboot_mode(),
            RebootMode::RB_HALT_SYSTEM
        );
    }
}

/// Execute shutdown sequence
pub async fn shutdown(shutdown_type: ShutdownType) -> ! {
    log::info!("Initiating {:?} sequence", shutdown_type);

    // Send SIGTERM to all processes (except ourselves)
    terminate_all_processes().await;

    // Sync filesystems
    log::info!("Syncing filesystems");
    sync();

    // Unmount filesystems (in reverse order)
    unmount_filesystems();

    // Final sync
    sync();

    log::info!("Executing {:?}", shutdown_type);

    // Execute reboot syscall
    let Err(e) = reboot(shutdown_type.to_reboot_mode());
    log::error!("reboot() failed: {}", e);
    // If reboot fails, loop forever (we're PID 1, can't exit)
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Send SIGTERM then SIGKILL to all processes
async fn terminate_all_processes() {
    log::info!("Sending SIGTERM to all processes");

    // Send SIGTERM to all processes (signal -1 means all)
    let _ = kill(Pid::from_raw(-1), Signal::SIGTERM);

    // Wait for processes to exit gracefully
    sleep(Duration::from_secs(5)).await;

    log::info!("Sending SIGKILL to remaining processes");

    // Send SIGKILL to stragglers
    let _ = kill(Pid::from_raw(-1), Signal::SIGKILL);

    // Brief pause for kernel cleanup
    sleep(Duration::from_millis(100)).await;
}

/// Unmount all filesystems (except root)
fn unmount_filesystems() {
    log::info!("Unmounting filesystems");

    // Read current mounts
    let mounts = match fs::read_to_string("/proc/mounts") {
        Ok(m) => m,
        Err(e) => {
            log::error!("Cannot read /proc/mounts: {}", e);
            return;
        }
    };

    let mount_points = mount_points_for_unmount(&mounts);

    for mount_point in mount_points {
        if should_skip_mount(&mount_point) {
            continue;
        }

        // Try to unmount
        let path = Path::new(&mount_point);
        log::debug!("Unmounting {}", mount_point);

        if let Err(e) = nix::mount::umount(path) {
            // Try lazy unmount if normal fails
            log::debug!(
                "Normal unmount failed for {}: {}, trying lazy",
                mount_point,
                e
            );
            if let Err(e) = nix::mount::umount2(path, nix::mount::MntFlags::MNT_DETACH) {
                log::warn!("Failed to unmount {}: {}", mount_point, e);
            }
        }
    }
}

fn mount_points_for_unmount(mounts: &str) -> Vec<String> {
    let mut mount_points: Vec<String> = mounts
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                Some(parts[1].to_string())
            } else {
                None
            }
        })
        .collect();
    mount_points.reverse();
    mount_points
}

fn should_skip_mount(mount_point: &str) -> bool {
    ["/", "/proc", "/sys", "/dev"].contains(&mount_point)
}

#[cfg(test)]
mod more_tests {
    use super::*;

    #[test]
    fn mount_points_for_unmount_parses_and_reverses_proc_mount_lines() {
        let mounts = "\
rootfs / rootfs rw 0 0
proc /proc proc rw 0 0
tmpfs /run tmpfs rw 0 0
malformed
/dev/sda1 /var ext4 rw 0 0
";

        assert_eq!(
            mount_points_for_unmount(mounts),
            ["/var", "/run", "/proc", "/"]
        );
    }

    #[test]
    fn should_skip_mount_only_skips_critical_mount_points() {
        for mount in ["/", "/proc", "/sys", "/dev"] {
            assert!(should_skip_mount(mount));
        }
        assert!(!should_skip_mount("/run"));
        assert!(!should_skip_mount("/var"));
    }
}
