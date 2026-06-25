// Security sandboxing implementation.
//
// Implements systemd's security features:
// - NoNewPrivileges (prctl)
// - ProtectSystem/ProtectHome/PrivateTmp (mount namespaces)
// - PrivateDevices/PrivateNetwork (namespaces)
// - Capabilities (prctl)
// - RestrictNamespaces (seccomp)
// - SystemCallFilter (seccomp)

use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::Path;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use crate::sandbox_prctl::{apply_no_new_privileges, apply_private_network};
use crate::units::{DevicePolicy, ProtectHome, ProtectProc, ProtectSystem, ServiceSection};

/// Apply all sandbox settings for a service.
/// Must be called after fork() but before exec().
/// Returns Ok(()) on success, Err with description on failure.
pub fn apply_sandbox(service: &ServiceSection) -> Result<(), String> {
    apply_basic_sandbox_settings(service)?;
    if needs_mount_namespace(service) {
        apply_mount_namespace_settings(service)?;
    }
    if has_seccomp_settings(service) {
        apply_combined_seccomp_m16(service)?;
    }
    Ok(())
}

fn apply_basic_sandbox_settings(service: &ServiceSection) -> Result<(), String> {
    if service.no_new_privileges {
        apply_no_new_privileges()?;
    }
    if service.protect_kernel_modules {
        drop_capability(Capability::SysModule)?;
    }
    apply_capability_bounding_set(&service.capability_bounding_set)?;
    apply_ambient_capabilities(&service.ambient_capabilities)?;
    if service.private_network {
        apply_private_network()?;
    }
    apply_prctl_settings(service)
}

fn apply_prctl_settings(service: &ServiceSection) -> Result<(), String> {
    if service.restrict_realtime {
        apply_restrict_realtime()?;
    }
    if service.memory_deny_write_execute {
        apply_memory_deny_write_execute()?;
    }
    if service.lock_personality {
        apply_lock_personality()?;
    }
    if service.ignore_sigpipe {
        apply_ignore_sigpipe()?;
    }
    Ok(())
}

fn needs_mount_namespace(service: &ServiceSection) -> bool {
    let requires_namespace = [
        !matches!(service.protect_system, ProtectSystem::No),
        !matches!(service.protect_home, ProtectHome::No),
        service.private_tmp,
        service.private_devices,
        !matches!(service.device_policy, DevicePolicy::Auto),
        !matches!(service.protect_proc, ProtectProc::Default),
        !service.read_only_paths.is_empty(),
        !service.read_write_paths.is_empty(),
        !service.inaccessible_paths.is_empty(),
        service.protect_control_groups,
        service.protect_kernel_tunables,
        service.protect_kernel_logs,
    ];
    requires_namespace.into_iter().any(|enabled| enabled)
}

fn apply_mount_namespace_settings(service: &ServiceSection) -> Result<(), String> {
    create_mount_namespace()?;
    apply_protect_system(&service.protect_system)?;
    apply_protect_home(&service.protect_home)?;
    if service.private_tmp {
        apply_private_tmp()?;
    }
    apply_device_namespace_policy(service)?;
    apply_protect_proc(&service.protect_proc)?;
    apply_mount_protections(service)?;
    apply_path_restrictions(
        &service.read_write_paths,
        &service.read_only_paths,
        &service.inaccessible_paths,
    )?;
    Ok(())
}

fn apply_device_namespace_policy(service: &ServiceSection) -> Result<(), String> {
    if !matches!(service.device_policy, DevicePolicy::Auto) {
        return apply_device_policy(&service.device_policy, &service.device_allow);
    }
    if service.private_devices {
        return apply_private_devices();
    }
    Ok(())
}

fn apply_mount_protections(service: &ServiceSection) -> Result<(), String> {
    if service.protect_control_groups {
        apply_protect_control_groups()?;
    }
    if service.protect_kernel_tunables {
        apply_protect_kernel_tunables()?;
    }
    if service.protect_kernel_logs {
        apply_protect_kernel_logs()?;
    }
    Ok(())
}

fn has_seccomp_settings(service: &ServiceSection) -> bool {
    service.restrict_namespaces.is_some()
        || !service.system_call_filter.is_empty()
        || service.protect_clock
        || service.protect_hostname
        || service.restrict_suid_sgid
        || service.restrict_address_families.is_some()
        || !service.system_call_architectures.is_empty()
}

/// Linux capabilities
#[derive(Debug, Clone, Copy)]
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

const CAPABILITY_BY_NAME: &[(&str, Capability)] = &[
    ("CHOWN", Capability::Chown),
    ("DAC_OVERRIDE", Capability::DacOverride),
    ("DAC_READ_SEARCH", Capability::DacReadSearch),
    ("FOWNER", Capability::Fowner),
    ("FSETID", Capability::Fsetid),
    ("KILL", Capability::Kill),
    ("SETGID", Capability::Setgid),
    ("SETUID", Capability::Setuid),
    ("SETPCAP", Capability::Setpcap),
    ("LINUX_IMMUTABLE", Capability::LinuxImmutable),
    ("NET_BIND_SERVICE", Capability::NetBindService),
    ("NET_BROADCAST", Capability::NetBroadcast),
    ("NET_ADMIN", Capability::NetAdmin),
    ("NET_RAW", Capability::NetRaw),
    ("IPC_LOCK", Capability::IpcLock),
    ("IPC_OWNER", Capability::IpcOwner),
    ("SYS_MODULE", Capability::SysModule),
    ("SYS_RAWIO", Capability::SysRawio),
    ("SYS_CHROOT", Capability::SysChroot),
    ("SYS_PTRACE", Capability::SysPtrace),
    ("SYS_PACCT", Capability::SysPacct),
    ("SYS_ADMIN", Capability::SysAdmin),
    ("SYS_BOOT", Capability::SysBoot),
    ("SYS_NICE", Capability::SysNice),
    ("SYS_RESOURCE", Capability::SysResource),
    ("SYS_TIME", Capability::SysTime),
    ("SYS_TTY_CONFIG", Capability::SysTtyConfig),
    ("MKNOD", Capability::Mknod),
    ("LEASE", Capability::Lease),
    ("AUDIT_WRITE", Capability::AuditWrite),
    ("AUDIT_CONTROL", Capability::AuditControl),
    ("SETFCAP", Capability::Setfcap),
    ("MAC_OVERRIDE", Capability::MacOverride),
    ("MAC_ADMIN", Capability::MacAdmin),
    ("SYSLOG", Capability::Syslog),
    ("WAKE_ALARM", Capability::WakeAlarm),
    ("BLOCK_SUSPEND", Capability::BlockSuspend),
    ("AUDIT_READ", Capability::AuditRead),
];

impl Capability {
    fn from_name(name: &str) -> Option<Self> {
        let normalized = name.strip_prefix("CAP_").unwrap_or(name).to_uppercase();
        CAPABILITY_BY_NAME.iter().find_map(|(cap_name, capability)| {
            (*cap_name == normalized.as_str()).then_some(*capability)
        })
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

    let dropped_caps: Vec<&str> = caps.iter().filter_map(|cap| cap.strip_prefix('~')).collect();
    if dropped_caps.is_empty() {
        log::debug!("CapabilityBoundingSet keep-list not fully implemented");
        return Ok(());
    }

    for cap_name in dropped_caps {
        if let Some(capability) = Capability::from_name(cap_name) {
            drop_capability(capability)?;
        }
    }
    Ok(())
}

// PR_CAP_AMBIENT constants (not in libc crate)
const PR_CAP_AMBIENT: libc::c_int = 47;
const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;

/// Apply ambient capabilities
fn apply_ambient_capabilities(caps: &[String]) -> Result<(), String> {
    for cap_str in caps {
        let Some(capability) = Capability::from_name(cap_str) else {
            continue;
        };
        raise_ambient_capability(capability);
    }
    Ok(())
}

fn raise_ambient_capability(capability: Capability) {
    let result = unsafe {
        libc::prctl(
            PR_CAP_AMBIENT,
            PR_CAP_AMBIENT_RAISE,
            capability as libc::c_ulong,
            0,
            0,
        )
    };
    if result != 0 {
        log::warn!("Failed to raise ambient capability {:?}", capability);
    }
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
        ProtectHome::No => Ok(()),
        ProtectHome::Yes => apply_to_home_dirs(&home_dirs, make_inaccessible),
        ProtectHome::ReadOnly => apply_to_home_dirs(&home_dirs, bind_mount_ro),
        ProtectHome::Tmpfs => apply_to_home_dirs(&home_dirs, mount_tmpfs),
    }
}

fn apply_to_home_dirs(
    home_dirs: &[&str],
    operation: fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    for dir in home_dirs {
        if Path::new(dir).exists() {
            operation(dir)?;
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
    create_pseudo_devices()?;

    // Create /dev/pts and /dev/shm as directories
    let _ = std::fs::create_dir("/dev/pts");
    let _ = std::fs::create_dir("/dev/shm");

    Ok(())
}

/// DevicePolicy= - restrict device access via mount namespace
fn apply_device_policy(policy: &DevicePolicy, device_allow: &[String]) -> Result<(), String> {
    // Mount a new tmpfs on /dev
    mount_tmpfs("/dev")?;

    // Create /dev/pts and /dev/shm directories first
    let _ = std::fs::create_dir("/dev/pts");
    let _ = std::fs::create_dir("/dev/shm");

    match policy {
        DevicePolicy::Auto => {
            // Should not reach here - Auto means no restrictions
            return Ok(());
        }
        DevicePolicy::Closed => {
            // Create pseudo devices (null, zero, random, urandom, full, tty, console)
            create_pseudo_devices()?;
        }
        DevicePolicy::Strict => {
            // No devices by default - only DeviceAllow entries
        }
    }

    // Add DeviceAllow entries
    for entry in device_allow {
        add_device_allow_entry(entry)?;
    }

    log::debug!(
        "DevicePolicy={:?}: created /dev with {} allowed devices",
        policy,
        device_allow.len()
    );
    Ok(())
}

/// Create pseudo devices (used by PrivateDevices and DevicePolicy=closed)
fn create_pseudo_devices() -> Result<(), String> {
    // Essential pseudo devices
    let devices = [
        ("/dev/null", 1, 3),
        ("/dev/zero", 1, 5),
        ("/dev/full", 1, 7),
        ("/dev/random", 1, 8),
        ("/dev/urandom", 1, 9),
        ("/dev/tty", 5, 0),
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
    Ok(())
}

/// Parse and add a DeviceAllow entry
/// Format: "/dev/sda rw" or "char-pts r" or "/dev/null"
fn add_device_allow_entry(entry: &str) -> Result<(), String> {
    let parts: Vec<&str> = entry.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(());
    }

    let device = parts[0];
    let perms = parts.get(1).copied().unwrap_or("rw");
    let read_only = !perms.contains('w');

    // Handle device classes like "char-pts"
    if device.starts_with("char-") || device.starts_with("block-") {
        let class = device.split('-').nth(1).unwrap_or("");
        return add_device_class(class, device.starts_with("block-"), read_only);
    }

    // Handle explicit device paths
    if device.starts_with("/dev/") {
        return add_device_path(device, read_only);
    }

    log::warn!("DeviceAllow: unrecognized device spec: {}", device);
    Ok(())
}

/// Add a device class (e.g., char-pts, block-*)
fn add_device_class(class: &str, _is_block: bool, read_only: bool) -> Result<(), String> {
    match class {
        "pts" => {
            // Mount devpts on /dev/pts
            let path = CString::new("/dev/pts").unwrap();
            let fstype = CString::new("devpts").unwrap();
            let source = CString::new("devpts").unwrap();
            let opts = CString::new("gid=5,mode=620,ptmxmode=000").unwrap();

            let mut flags = libc::MS_NOSUID | libc::MS_NOEXEC;
            if read_only {
                flags |= libc::MS_RDONLY;
            }

            unsafe {
                if libc::mount(
                    source.as_ptr(),
                    path.as_ptr(),
                    fstype.as_ptr(),
                    flags,
                    opts.as_ptr() as *const libc::c_void,
                ) != 0
                {
                    log::warn!("Failed to mount devpts");
                }
            }
            log::debug!("DeviceAllow: added char-pts (read_only={})", read_only);
        }
        _ => {
            log::warn!("DeviceAllow: unsupported device class: {}", class);
        }
    }
    Ok(())
}

/// Add a specific device path
fn add_device_path(device: &str, read_only: bool) -> Result<(), String> {
    let src = Path::new(device);
    if !src.exists() {
        log::warn!("DeviceAllow: source device {} does not exist", device);
        return Ok(());
    }

    if !is_device_node(src)? {
        log::warn!("DeviceAllow: {} is not a device", device);
        return Ok(());
    }

    ensure_device_placeholder(device);
    bind_mount_device(device)?;
    remount_device_read_only(device, read_only);
    log::debug!("DeviceAllow: added {} (read_only={})", device, read_only);
    Ok(())
}

fn is_device_node(path: &Path) -> Result<bool, String> {
    use std::os::unix::fs::FileTypeExt;
    let file_type = std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .file_type();
    Ok(file_type.is_char_device() || file_type.is_block_device())
}

fn ensure_device_placeholder(device: &str) {
    if Path::new(device).exists() {
        return;
    }
    if let Some(parent) = Path::new(device).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::File::create(device);
}

fn bind_mount_device(device: &str) -> Result<(), String> {
    let device_c = CString::new(device).unwrap();
    let none = CString::new("none").unwrap();
    let result = unsafe {
        libc::mount(
            device_c.as_ptr(),
            device_c.as_ptr(),
            none.as_ptr(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if result != 0 {
        log::warn!("Failed to bind mount {}", device);
    }
    Ok(())
}

fn remount_device_read_only(device: &str, read_only: bool) {
    if !read_only {
        return;
    }
    let device_c = CString::new(device).unwrap();
    let none = CString::new("none").unwrap();
    let result = unsafe {
        libc::mount(
            std::ptr::null(),
            device_c.as_ptr(),
            none.as_ptr(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
            std::ptr::null(),
        )
    };
    if result != 0 {
        log::warn!("Failed to remount {} read-only", device);
    }
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
