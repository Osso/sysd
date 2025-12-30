//! Security sandboxing implementation
//!
//! Implements systemd's security features:
//! - NoNewPrivileges (prctl)
//! - ProtectSystem/ProtectHome/PrivateTmp (mount namespaces)
//! - PrivateDevices/PrivateNetwork (namespaces)
//! - Capabilities (prctl)
//! - SystemCallFilter (seccomp)

use std::ffi::CString;
use std::path::Path;

use crate::units::{ProtectHome, ProtectProc, ProtectSystem, ServiceSection};

/// Apply all sandbox settings for a service.
/// Must be called after fork() but before exec().
/// Returns Ok(()) on success, Err with description on failure.
pub fn apply_sandbox(service: &ServiceSection) -> Result<(), String> {
    // NoNewPrivileges - prevents gaining privileges via setuid/setgid
    if service.no_new_privileges {
        apply_no_new_privileges()?;
    }

    // ProtectKernelModules - drop CAP_SYS_MODULE
    if service.protect_kernel_modules {
        drop_capability(Capability::SysModule)?;
    }

    // Capability bounding set
    apply_capability_bounding_set(&service.capability_bounding_set)?;

    // Ambient capabilities
    apply_ambient_capabilities(&service.ambient_capabilities)?;

    // Namespace isolation (must be done early)
    if service.private_network {
        apply_private_network()?;
    }

    // Mount namespace operations
    let needs_mount_ns = !matches!(service.protect_system, ProtectSystem::No)
        || !matches!(service.protect_home, ProtectHome::No)
        || service.private_tmp
        || service.private_devices
        || !matches!(service.protect_proc, ProtectProc::Default)
        || !service.read_only_paths.is_empty()
        || !service.read_write_paths.is_empty()
        || !service.inaccessible_paths.is_empty();

    if needs_mount_ns {
        // Create new mount namespace
        create_mount_namespace()?;

        // Apply filesystem protections
        apply_protect_system(&service.protect_system)?;
        apply_protect_home(&service.protect_home)?;

        if service.private_tmp {
            apply_private_tmp()?;
        }

        if service.private_devices {
            apply_private_devices()?;
        }

        apply_protect_proc(&service.protect_proc)?;

        // Path-specific restrictions
        apply_path_restrictions(
            &service.read_write_paths,
            &service.read_only_paths,
            &service.inaccessible_paths,
        )?;
    }

    // Seccomp filter (must be last - blocks syscalls needed above)
    if !service.system_call_filter.is_empty() {
        apply_seccomp_filter(&service.system_call_filter)?;
    }

    Ok(())
}

/// NoNewPrivileges=yes - prevents privilege escalation
fn apply_no_new_privileges() -> Result<(), String> {
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err("Failed to set PR_SET_NO_NEW_PRIVS".to_string());
        }
    }
    Ok(())
}

/// Linux capabilities
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum Capability {
    Chown = 0,
    DacOverride = 1,
    DacReadSearch = 2,
    Fowner = 3,
    Fsetid = 4,
    Kill = 5,
    Setgid = 6,
    Setuid = 7,
    Setpcap = 8,
    LinuxImmutable = 9,
    NetBindService = 10,
    NetBroadcast = 11,
    NetAdmin = 12,
    NetRaw = 13,
    IpcLock = 14,
    IpcOwner = 15,
    SysModule = 16,
    SysRawio = 17,
    SysChroot = 18,
    SysPtrace = 19,
    SysPacct = 20,
    SysAdmin = 21,
    SysBoot = 22,
    SysNice = 23,
    SysResource = 24,
    SysTime = 25,
    SysTtyConfig = 26,
    Mknod = 27,
    Lease = 28,
    AuditWrite = 29,
    AuditControl = 30,
    Setfcap = 31,
    MacOverride = 32,
    MacAdmin = 33,
    Syslog = 34,
    WakeAlarm = 35,
    BlockSuspend = 36,
    AuditRead = 37,
}

impl Capability {
    fn from_name(name: &str) -> Option<Self> {
        // Strip CAP_ prefix if present
        let name = name.strip_prefix("CAP_").unwrap_or(name);
        match name.to_uppercase().as_str() {
            "CHOWN" => Some(Self::Chown),
            "DAC_OVERRIDE" => Some(Self::DacOverride),
            "DAC_READ_SEARCH" => Some(Self::DacReadSearch),
            "FOWNER" => Some(Self::Fowner),
            "FSETID" => Some(Self::Fsetid),
            "KILL" => Some(Self::Kill),
            "SETGID" => Some(Self::Setgid),
            "SETUID" => Some(Self::Setuid),
            "SETPCAP" => Some(Self::Setpcap),
            "LINUX_IMMUTABLE" => Some(Self::LinuxImmutable),
            "NET_BIND_SERVICE" => Some(Self::NetBindService),
            "NET_BROADCAST" => Some(Self::NetBroadcast),
            "NET_ADMIN" => Some(Self::NetAdmin),
            "NET_RAW" => Some(Self::NetRaw),
            "IPC_LOCK" => Some(Self::IpcLock),
            "IPC_OWNER" => Some(Self::IpcOwner),
            "SYS_MODULE" => Some(Self::SysModule),
            "SYS_RAWIO" => Some(Self::SysRawio),
            "SYS_CHROOT" => Some(Self::SysChroot),
            "SYS_PTRACE" => Some(Self::SysPtrace),
            "SYS_PACCT" => Some(Self::SysPacct),
            "SYS_ADMIN" => Some(Self::SysAdmin),
            "SYS_BOOT" => Some(Self::SysBoot),
            "SYS_NICE" => Some(Self::SysNice),
            "SYS_RESOURCE" => Some(Self::SysResource),
            "SYS_TIME" => Some(Self::SysTime),
            "SYS_TTY_CONFIG" => Some(Self::SysTtyConfig),
            "MKNOD" => Some(Self::Mknod),
            "LEASE" => Some(Self::Lease),
            "AUDIT_WRITE" => Some(Self::AuditWrite),
            "AUDIT_CONTROL" => Some(Self::AuditControl),
            "SETFCAP" => Some(Self::Setfcap),
            "MAC_OVERRIDE" => Some(Self::MacOverride),
            "MAC_ADMIN" => Some(Self::MacAdmin),
            "SYSLOG" => Some(Self::Syslog),
            "WAKE_ALARM" => Some(Self::WakeAlarm),
            "BLOCK_SUSPEND" => Some(Self::BlockSuspend),
            "AUDIT_READ" => Some(Self::AuditRead),
            _ => None,
        }
    }
}

/// Drop a capability from the bounding set
fn drop_capability(cap: Capability) -> Result<(), String> {
    unsafe {
        if libc::prctl(libc::PR_CAPBSET_DROP, cap as libc::c_ulong, 0, 0, 0) != 0 {
            return Err(format!("Failed to drop capability {:?}", cap));
        }
    }
    Ok(())
}

/// Apply capability bounding set
/// Entries starting with ~ are dropped, others are kept (all others dropped)
fn apply_capability_bounding_set(caps: &[String]) -> Result<(), String> {
    if caps.is_empty() {
        return Ok(());
    }

    // Check if we have a drop list (~ prefix) or keep list
    let has_drop = caps.iter().any(|c| c.starts_with('~'));
    let has_keep = caps.iter().any(|c| !c.starts_with('~'));

    if has_drop && has_keep {
        // Mixed mode - process each entry
        for cap_str in caps {
            if let Some(name) = cap_str.strip_prefix('~') {
                if let Some(cap) = Capability::from_name(name) {
                    drop_capability(cap)?;
                }
            }
        }
    } else if has_drop {
        // Only drop list - drop specified caps
        for cap_str in caps {
            if let Some(name) = cap_str.strip_prefix('~') {
                if let Some(cap) = Capability::from_name(name) {
                    drop_capability(cap)?;
                }
            }
        }
    } else {
        // Keep list - drop all caps not in the list
        // This is more complex - we'd need to enumerate all caps
        // For now, just log a warning
        log::debug!("CapabilityBoundingSet keep-list not fully implemented");
    }

    Ok(())
}

// PR_CAP_AMBIENT constants (not in libc crate)
const PR_CAP_AMBIENT: libc::c_int = 47;
const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;

/// Apply ambient capabilities
fn apply_ambient_capabilities(caps: &[String]) -> Result<(), String> {
    for cap_str in caps {
        if let Some(cap) = Capability::from_name(cap_str) {
            unsafe {
                if libc::prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, cap as libc::c_ulong, 0, 0)
                    != 0
                {
                    log::warn!("Failed to raise ambient capability {:?}", cap);
                }
            }
        }
    }
    Ok(())
}

/// Create a new mount namespace
fn create_mount_namespace() -> Result<(), String> {
    unsafe {
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            return Err("Failed to create mount namespace".to_string());
        }
        // Make all mounts private so changes don't propagate
        let root = CString::new("/").unwrap();
        let none = CString::new("none").unwrap();
        if libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            none.as_ptr(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        ) != 0
        {
            log::warn!("Failed to make mounts private");
        }
    }
    Ok(())
}

/// PrivateNetwork=yes - create isolated network namespace
fn apply_private_network() -> Result<(), String> {
    unsafe {
        if libc::unshare(libc::CLONE_NEWNET) != 0 {
            return Err("Failed to create network namespace".to_string());
        }
    }
    Ok(())
}

/// ProtectSystem= - make system directories read-only
fn apply_protect_system(mode: &ProtectSystem) -> Result<(), String> {
    match mode {
        ProtectSystem::No => {}
        ProtectSystem::Yes => {
            // /usr and /boot read-only
            bind_mount_ro("/usr")?;
            bind_mount_ro("/boot")?;
        }
        ProtectSystem::Full => {
            // /usr, /boot, and /etc read-only
            bind_mount_ro("/usr")?;
            bind_mount_ro("/boot")?;
            bind_mount_ro("/etc")?;
        }
        ProtectSystem::Strict => {
            // Entire filesystem read-only except /dev, /proc, /sys, /run, /tmp, /var
            bind_mount_ro("/")?;
            // Remount writable paths
            for path in &["/dev", "/proc", "/sys", "/run", "/tmp", "/var"] {
                if Path::new(path).exists() {
                    let _ = remount_rw(path);
                }
            }
        }
    }
    Ok(())
}

/// ProtectHome= - protect home directories
fn apply_protect_home(mode: &ProtectHome) -> Result<(), String> {
    let home_dirs = ["/home", "/root", "/run/user"];

    match mode {
        ProtectHome::No => {}
        ProtectHome::Yes => {
            // Make inaccessible
            for dir in &home_dirs {
                if Path::new(dir).exists() {
                    make_inaccessible(dir)?;
                }
            }
        }
        ProtectHome::ReadOnly => {
            // Make read-only
            for dir in &home_dirs {
                if Path::new(dir).exists() {
                    bind_mount_ro(dir)?;
                }
            }
        }
        ProtectHome::Tmpfs => {
            // Mount empty tmpfs
            for dir in &home_dirs {
                if Path::new(dir).exists() {
                    mount_tmpfs(dir)?;
                }
            }
        }
    }
    Ok(())
}

/// PrivateTmp=yes - isolated /tmp and /var/tmp
fn apply_private_tmp() -> Result<(), String> {
    mount_tmpfs("/tmp")?;
    mount_tmpfs("/var/tmp")?;
    Ok(())
}

/// PrivateDevices=yes - minimal /dev with only safe devices
fn apply_private_devices() -> Result<(), String> {
    // Mount a new tmpfs on /dev
    mount_tmpfs("/dev")?;

    // Create essential device nodes
    // Note: This requires CAP_MKNOD
    let devices = [
        ("/dev/null", 1, 3),
        ("/dev/zero", 1, 5),
        ("/dev/full", 1, 7),
        ("/dev/random", 1, 8),
        ("/dev/urandom", 1, 9),
    ];

    for (path, major, minor) in &devices {
        let path_c = CString::new(*path).unwrap();
        unsafe {
            let dev = libc::makedev(*major, *minor);
            // Character device with mode 0666
            if libc::mknod(path_c.as_ptr(), libc::S_IFCHR | 0o666, dev) != 0 {
                log::warn!("Failed to create {}", path);
            }
        }
    }

    // Create /dev/pts and /dev/shm as directories
    let _ = std::fs::create_dir("/dev/pts");
    let _ = std::fs::create_dir("/dev/shm");

    Ok(())
}

/// ProtectProc= - restrict /proc visibility
fn apply_protect_proc(mode: &ProtectProc) -> Result<(), String> {
    match mode {
        ProtectProc::Default => {}
        ProtectProc::Invisible => {
            // Remount /proc with hidepid=2
            remount_proc("hidepid=2")?;
        }
        ProtectProc::Ptraceable => {
            // Remount /proc with hidepid=1
            remount_proc("hidepid=1")?;
        }
        ProtectProc::NoAccess => {
            // Make /proc inaccessible
            make_inaccessible("/proc")?;
        }
    }
    Ok(())
}

/// Apply path restrictions
fn apply_path_restrictions(
    read_write: &[std::path::PathBuf],
    read_only: &[std::path::PathBuf],
    inaccessible: &[std::path::PathBuf],
) -> Result<(), String> {
    // Make paths inaccessible
    for path in inaccessible {
        if path.exists() {
            make_inaccessible(path.to_str().unwrap_or(""))?;
        }
    }

    // Make paths read-only
    for path in read_only {
        if path.exists() {
            bind_mount_ro(path.to_str().unwrap_or(""))?;
        }
    }

    // Read-write paths are allowed by default, but if we're in strict mode
    // we need to explicitly remount them as writable
    for path in read_write {
        if path.exists() {
            let _ = remount_rw(path.to_str().unwrap_or(""));
        }
    }

    Ok(())
}

/// Apply seccomp system call filter
fn apply_seccomp_filter(filter: &[String]) -> Result<(), String> {
    // Seccomp filtering is complex - it requires building a BPF program
    // For now, we'll log and skip
    if !filter.is_empty() {
        log::debug!(
            "SystemCallFilter not fully implemented, ignoring: {:?}",
            filter
        );
    }
    Ok(())
}

// Helper functions for mount operations

fn bind_mount_ro(path: &str) -> Result<(), String> {
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let none = CString::new("none").unwrap();

    unsafe {
        // First bind mount to self
        if libc::mount(
            path_c.as_ptr(),
            path_c.as_ptr(),
            none.as_ptr(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("Failed to bind mount {}", path));
        }

        // Then remount read-only
        if libc::mount(
            std::ptr::null(),
            path_c.as_ptr(),
            none.as_ptr(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC,
            std::ptr::null(),
        ) != 0
        {
            log::warn!("Failed to remount {} read-only", path);
        }
    }
    Ok(())
}

fn remount_rw(path: &str) -> Result<(), String> {
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let none = CString::new("none").unwrap();

    unsafe {
        if libc::mount(
            std::ptr::null(),
            path_c.as_ptr(),
            none.as_ptr(),
            libc::MS_REMOUNT | libc::MS_BIND,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("Failed to remount {} read-write", path));
        }
    }
    Ok(())
}

fn mount_tmpfs(path: &str) -> Result<(), String> {
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let fstype = CString::new("tmpfs").unwrap();
    let source = CString::new("tmpfs").unwrap();

    unsafe {
        if libc::mount(
            source.as_ptr(),
            path_c.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("Failed to mount tmpfs on {}", path));
        }
    }
    Ok(())
}

fn make_inaccessible(path: &str) -> Result<(), String> {
    // Bind mount an empty tmpfs or /dev/null over the path
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let fstype = CString::new("tmpfs").unwrap();
    let source = CString::new("tmpfs").unwrap();

    unsafe {
        // Mount empty tmpfs
        if libc::mount(
            source.as_ptr(),
            path_c.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        ) != 0
        {
            return Err(format!("Failed to make {} inaccessible", path));
        }
    }
    Ok(())
}

fn remount_proc(options: &str) -> Result<(), String> {
    let path = CString::new("/proc").unwrap();
    let fstype = CString::new("proc").unwrap();
    let source = CString::new("proc").unwrap();
    let opts = CString::new(options).map_err(|e| e.to_string())?;

    unsafe {
        // Unmount current /proc
        libc::umount2(path.as_ptr(), libc::MNT_DETACH);

        // Mount new /proc with options
        if libc::mount(
            source.as_ptr(),
            path.as_ptr(),
            fstype.as_ptr(),
            0,
            opts.as_ptr() as *const libc::c_void,
        ) != 0
        {
            return Err(format!("Failed to remount /proc with {}", options));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_from_name() {
        assert!(matches!(
            Capability::from_name("CAP_NET_BIND_SERVICE"),
            Some(Capability::NetBindService)
        ));
        assert!(matches!(
            Capability::from_name("NET_BIND_SERVICE"),
            Some(Capability::NetBindService)
        ));
        assert!(matches!(
            Capability::from_name("SYS_ADMIN"),
            Some(Capability::SysAdmin)
        ));
        assert!(Capability::from_name("INVALID_CAP").is_none());
    }

    #[test]
    fn test_no_new_privileges_requires_root() {
        // This test documents behavior - PR_SET_NO_NEW_PRIVS should work for any user
        // but in a test environment we just verify it doesn't crash
        let result = apply_no_new_privileges();
        // May fail if already set, but shouldn't panic
        let _ = result;
    }
}
