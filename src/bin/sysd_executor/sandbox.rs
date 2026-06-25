use std::ffi::CString;

use sysd::executor::{
    DevicePolicyConfig, ProtectHomeConfig, ProtectProcConfig, ProtectSystemConfig, SandboxConfig,
};
use sysd::sandbox_prctl::{apply_no_new_privileges, apply_private_network};

const CAPABILITY_TABLE: &[(&str, u32)] = &[
    ("CHOWN", 0),
    ("DAC_OVERRIDE", 1),
    ("DAC_READ_SEARCH", 2),
    ("FOWNER", 3),
    ("FSETID", 4),
    ("KILL", 5),
    ("SETGID", 6),
    ("SETUID", 7),
    ("SETPCAP", 8),
    ("LINUX_IMMUTABLE", 9),
    ("NET_BIND_SERVICE", 10),
    ("NET_BROADCAST", 11),
    ("NET_ADMIN", 12),
    ("NET_RAW", 13),
    ("IPC_LOCK", 14),
    ("IPC_OWNER", 15),
    ("SYS_MODULE", 16),
    ("SYS_RAWIO", 17),
    ("SYS_CHROOT", 18),
    ("SYS_PTRACE", 19),
    ("SYS_PACCT", 20),
    ("SYS_ADMIN", 21),
    ("SYS_BOOT", 22),
    ("SYS_NICE", 23),
    ("SYS_RESOURCE", 24),
    ("SYS_TIME", 25),
    ("SYS_TTY_CONFIG", 26),
    ("MKNOD", 27),
    ("LEASE", 28),
    ("AUDIT_WRITE", 29),
    ("AUDIT_CONTROL", 30),
    ("SETFCAP", 31),
    ("MAC_OVERRIDE", 32),
    ("MAC_ADMIN", 33),
    ("SYSLOG", 34),
    ("WAKE_ALARM", 35),
    ("BLOCK_SUSPEND", 36),
    ("AUDIT_READ", 37),
    ("PERFMON", 38),
    ("BPF", 39),
    ("CHECKPOINT_RESTORE", 40),
];

pub(super) fn apply_sandbox_phase1(sandbox: &SandboxConfig) -> Result<(), String> {
    if sandbox.protect_kernel_modules {
        drop_capability(16)?;
    }
    apply_capability_bounding_set(&sandbox.capability_bounding_set)?;
    if sandbox.private_network {
        apply_private_network()?;
    }
    if sandbox.memory_deny_write_execute {
        apply_memory_deny_write_execute()?;
    }
    if sandbox.ignore_sigpipe {
        apply_ignore_sigpipe()?;
    }
    if needs_mount_namespace(sandbox) {
        apply_mount_namespace_settings(sandbox)?;
    }
    Ok(())
}

fn needs_mount_namespace(sandbox: &SandboxConfig) -> bool {
    !matches!(sandbox.protect_system, ProtectSystemConfig::No)
        || !matches!(sandbox.protect_home, ProtectHomeConfig::No)
        || sandbox.private_tmp
        || sandbox.private_devices
        || !matches!(sandbox.device_policy, DevicePolicyConfig::Auto)
        || !matches!(sandbox.protect_proc, ProtectProcConfig::Default)
        || !sandbox.read_only_paths.is_empty()
        || !sandbox.read_write_paths.is_empty()
        || !sandbox.inaccessible_paths.is_empty()
        || sandbox.protect_control_groups
        || sandbox.protect_kernel_tunables
        || sandbox.protect_kernel_logs
}

fn apply_mount_namespace_settings(sandbox: &SandboxConfig) -> Result<(), String> {
    create_mount_namespace()?;
    apply_protect_system(&sandbox.protect_system)?;
    apply_protect_home(&sandbox.protect_home)?;
    if sandbox.private_tmp {
        apply_private_tmp()?;
    }
    apply_device_isolation(sandbox)?;
    apply_protect_proc(&sandbox.protect_proc)?;
    apply_kernel_protections(sandbox)?;
    apply_path_restrictions(
        &sandbox.read_write_paths,
        &sandbox.read_only_paths,
        &sandbox.inaccessible_paths,
    )?;
    Ok(())
}

fn apply_device_isolation(sandbox: &SandboxConfig) -> Result<(), String> {
    if !matches!(sandbox.device_policy, DevicePolicyConfig::Auto) {
        return apply_device_policy(&sandbox.device_policy, &sandbox.device_allow);
    }
    if sandbox.private_devices {
        return apply_private_devices();
    }
    Ok(())
}

fn apply_kernel_protections(sandbox: &SandboxConfig) -> Result<(), String> {
    if sandbox.protect_control_groups {
        apply_protect_control_groups()?;
    }
    if sandbox.protect_kernel_tunables {
        apply_protect_kernel_tunables()?;
    }
    if sandbox.protect_kernel_logs {
        apply_protect_kernel_logs()?;
    }
    Ok(())
}

pub(super) fn apply_sandbox_phase2(sandbox: &SandboxConfig) -> Result<(), String> {
    apply_ambient_capabilities(&sandbox.ambient_capabilities)?;
    if sandbox.no_new_privileges {
        apply_no_new_privileges()?;
    }
    let has_seccomp = sandbox.restrict_namespaces.is_some()
        || !sandbox.system_call_filter.is_empty()
        || sandbox.protect_clock
        || sandbox.protect_hostname
        || sandbox.restrict_suid_sgid
        || sandbox.restrict_address_families.is_some()
        || !sandbox.system_call_architectures.is_empty()
        || sandbox.restrict_realtime
        || sandbox.lock_personality;
    if has_seccomp {
        apply_seccomp(sandbox)?;
    }
    Ok(())
}

fn drop_capability(cap: u32) -> Result<(), String> {
    unsafe {
        if libc::prctl(libc::PR_CAPBSET_DROP, cap as libc::c_ulong, 0, 0, 0) != 0 {
            return Err(format!("Failed to drop capability {}", cap));
        }
    }
    Ok(())
}

fn apply_capability_bounding_set(caps: &[String]) -> Result<(), String> {
    if caps.is_empty() {
        return Ok(());
    }
    for cap_str in caps {
        if let Some(name) = cap_str.strip_prefix('~') {
            if let Some(cap_num) = capability_name_to_num(name) {
                drop_capability(cap_num)?;
            }
        }
    }
    Ok(())
}

fn capability_name_to_num(name: &str) -> Option<u32> {
    let normalized = name.strip_prefix("CAP_").unwrap_or(name).to_uppercase();
    CAPABILITY_TABLE
        .iter()
        .find(|(cap, _)| *cap == normalized)
        .map(|(_, num)| *num)
}

const PR_CAP_AMBIENT: libc::c_int = 47;
const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;
const _LINUX_CAPABILITY_VERSION_3: u32 = 0x20080522;

#[repr(C)]
struct CapUserHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
struct CapUserData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn apply_ambient_capabilities(caps: &[String]) -> Result<(), String> {
    if caps.is_empty() {
        return Ok(());
    }
    let (header, mut data) = current_cap_data();
    add_inheritable_caps(caps, &mut data);
    set_cap_data(&header, &data);
    raise_ambient_caps(caps);
    Ok(())
}

fn current_cap_data() -> (CapUserHeader, [CapUserData; 2]) {
    let mut header = CapUserHeader {
        version: _LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [
        CapUserData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
        CapUserData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    unsafe {
        if libc::syscall(
            libc::SYS_capget,
            &mut header as *mut CapUserHeader,
            data.as_mut_ptr(),
        ) != 0
        {
            eprintln!(
                "sysd-executor: capget failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    (header, data)
}

fn add_inheritable_caps(caps: &[String], data: &mut [CapUserData; 2]) {
    for cap_str in caps {
        let Some(cap_num) = capability_name_to_num(cap_str) else {
            continue;
        };
        let cap_idx = (cap_num / 32) as usize;
        let cap_bit = 1u32 << (cap_num % 32);
        if cap_idx < 2 {
            data[cap_idx].inheritable |= cap_bit;
        }
    }
}

fn set_cap_data(header: &CapUserHeader, data: &[CapUserData; 2]) {
    unsafe {
        if libc::syscall(
            libc::SYS_capset,
            header as *const CapUserHeader,
            data.as_ptr(),
        ) != 0
        {
            eprintln!(
                "sysd-executor: capset failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

fn raise_ambient_caps(caps: &[String]) {
    for cap_str in caps {
        let Some(cap_num) = capability_name_to_num(cap_str) else {
            continue;
        };
        unsafe {
            let ret = libc::prctl(
                PR_CAP_AMBIENT,
                PR_CAP_AMBIENT_RAISE,
                cap_num as libc::c_ulong,
                0,
                0,
            );
            if ret != 0 {
                eprintln!(
                    "sysd-executor: failed to raise ambient cap {}: {}",
                    cap_str,
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

const PR_SET_MDWE: libc::c_int = 65;
const PR_MDWE_REFUSE_EXEC_GAIN: libc::c_ulong = 1;

fn apply_memory_deny_write_execute() -> Result<(), String> {
    unsafe {
        if libc::prctl(PR_SET_MDWE, PR_MDWE_REFUSE_EXEC_GAIN, 0, 0, 0) != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINVAL) {
                return Err(format!("Failed to set PR_SET_MDWE: {}", err));
            }
        }
    }
    Ok(())
}

fn apply_ignore_sigpipe() -> Result<(), String> {
    unsafe {
        if libc::signal(libc::SIGPIPE, libc::SIG_IGN) == libc::SIG_ERR {
            return Err("Failed to ignore SIGPIPE".to_string());
        }
    }
    Ok(())
}

fn create_mount_namespace() -> Result<(), String> {
    unsafe {
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            return Err("Failed to create mount namespace".to_string());
        }
        let root = CString::new("/").unwrap();
        let none = CString::new("none").unwrap();
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            none.as_ptr(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        );
    }
    Ok(())
}

fn apply_protect_system(mode: &ProtectSystemConfig) -> Result<(), String> {
    match mode {
        ProtectSystemConfig::No => {}
        ProtectSystemConfig::Yes => {
            bind_mount_ro("/usr")?;
            bind_mount_ro("/boot")?;
        }
        ProtectSystemConfig::Full => {
            bind_mount_ro("/usr")?;
            bind_mount_ro("/boot")?;
            bind_mount_ro("/etc")?;
        }
        ProtectSystemConfig::Strict => {
            bind_mount_ro("/")?;
            for path in &["/dev", "/proc", "/sys", "/run", "/tmp", "/var"] {
                if std::path::Path::new(path).exists() {
                    let _ = remount_rw(path);
                }
            }
        }
    }
    Ok(())
}

fn apply_protect_home(mode: &ProtectHomeConfig) -> Result<(), String> {
    match mode {
        ProtectHomeConfig::No => {}
        ProtectHomeConfig::Yes => {
            for_existing_home_dir(make_inaccessible)?;
        }
        ProtectHomeConfig::ReadOnly => {
            for_existing_home_dir(bind_mount_ro)?;
        }
        ProtectHomeConfig::Tmpfs => {
            for_existing_home_dir(mount_tmpfs)?;
        }
    }
    Ok(())
}

fn for_existing_home_dir(mut action: impl FnMut(&str) -> Result<(), String>) -> Result<(), String> {
    for dir in ["/home", "/root", "/run/user"] {
        if std::path::Path::new(dir).exists() {
            action(dir)?;
        }
    }
    Ok(())
}

fn apply_private_tmp() -> Result<(), String> {
    mount_tmpfs("/tmp")?;
    mount_tmpfs("/var/tmp")?;
    Ok(())
}

fn apply_private_devices() -> Result<(), String> {
    mount_tmpfs("/dev")?;
    create_pseudo_devices()?;
    let _ = std::fs::create_dir("/dev/pts");
    let _ = std::fs::create_dir("/dev/shm");
    Ok(())
}

fn apply_device_policy(policy: &DevicePolicyConfig, device_allow: &[String]) -> Result<(), String> {
    mount_tmpfs("/dev")?;
    let _ = std::fs::create_dir("/dev/pts");
    let _ = std::fs::create_dir("/dev/shm");
    match policy {
        DevicePolicyConfig::Auto => {}
        DevicePolicyConfig::Closed => {
            create_pseudo_devices()?;
        }
        DevicePolicyConfig::Strict => {}
    }
    for entry in device_allow {
        let _ = add_device_allow_entry(entry);
    }
    Ok(())
}

fn create_pseudo_devices() -> Result<(), String> {
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
            libc::mknod(path_c.as_ptr(), libc::S_IFCHR | 0o666, dev);
        }
    }
    Ok(())
}

fn add_device_allow_entry(entry: &str) -> Result<(), String> {
    let parts: Vec<&str> = entry.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(());
    }
    let device = parts[0];
    if device.starts_with("/dev/") && std::path::Path::new(device).exists() {
        let device_c = CString::new(device).unwrap();
        let none = CString::new("none").unwrap();
        unsafe {
            libc::mount(
                device_c.as_ptr(),
                device_c.as_ptr(),
                none.as_ptr(),
                libc::MS_BIND,
                std::ptr::null(),
            );
        }
    }
    Ok(())
}

fn apply_protect_proc(mode: &ProtectProcConfig) -> Result<(), String> {
    match mode {
        ProtectProcConfig::Default => {}
        ProtectProcConfig::Invisible => {
            remount_proc("hidepid=2")?;
        }
        ProtectProcConfig::Ptraceable => {
            remount_proc("hidepid=1")?;
        }
        ProtectProcConfig::NoAccess => {
            make_inaccessible("/proc")?;
        }
    }
    Ok(())
}

fn apply_protect_control_groups() -> Result<(), String> {
    if std::path::Path::new("/sys/fs/cgroup").exists() {
        bind_mount_ro("/sys/fs/cgroup")?;
    }
    Ok(())
}

fn apply_protect_kernel_tunables() -> Result<(), String> {
    if std::path::Path::new("/proc/sys").exists() {
        bind_mount_ro("/proc/sys")?;
    }
    if std::path::Path::new("/sys").exists() {
        bind_mount_ro("/sys")?;
    }
    Ok(())
}

fn apply_protect_kernel_logs() -> Result<(), String> {
    if std::path::Path::new("/dev/kmsg").exists() {
        make_inaccessible("/dev/kmsg")?;
    }
    if std::path::Path::new("/proc/kmsg").exists() {
        make_inaccessible("/proc/kmsg")?;
    }
    Ok(())
}

fn apply_path_restrictions(
    read_write: &[std::path::PathBuf],
    read_only: &[std::path::PathBuf],
    inaccessible: &[std::path::PathBuf],
) -> Result<(), String> {
    for path in inaccessible {
        if path.exists() {
            make_inaccessible(path.to_str().unwrap_or(""))?;
        }
    }
    for path in read_only {
        if path.exists() {
            bind_mount_ro(path.to_str().unwrap_or(""))?;
        }
    }
    for path in read_write {
        if path.exists() {
            let _ = remount_rw(path.to_str().unwrap_or(""));
        }
    }
    Ok(())
}

fn bind_mount_ro(path: &str) -> Result<(), String> {
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let none = CString::new("none").unwrap();
    unsafe {
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
        libc::mount(
            std::ptr::null(),
            path_c.as_ptr(),
            none.as_ptr(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC,
            std::ptr::null(),
        );
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
    let path_c = CString::new(path).map_err(|e| e.to_string())?;
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) => return Err(format!("Failed to stat {}: {}", path, e)),
    };
    if metadata.is_dir() {
        return make_directory_inaccessible(path, &path_c);
    }
    make_file_inaccessible(path, &path_c)
}

fn make_directory_inaccessible(path: &str, path_c: &CString) -> Result<(), String> {
    let fstype = CString::new("tmpfs").unwrap();
    let source = CString::new("tmpfs").unwrap();
    unsafe {
        if libc::mount(
            source.as_ptr(),
            path_c.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        ) == 0
        {
            return Ok(());
        }
    }
    let errno = unsafe { *libc::__errno_location() };
    Err(format!(
        "Failed to make {} inaccessible (tmpfs mount): errno {}",
        path, errno
    ))
}

fn make_file_inaccessible(path: &str, path_c: &CString) -> Result<(), String> {
    let dev_null = CString::new("/dev/null").unwrap();
    unsafe {
        if libc::mount(
            dev_null.as_ptr(),
            path_c.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        ) == 0
        {
            return Ok(());
        }
    }
    let errno = unsafe { *libc::__errno_location() };
    Err(format!(
        "Failed to make {} inaccessible (bind /dev/null): errno {}",
        path, errno
    ))
}

fn remount_proc(options: &str) -> Result<(), String> {
    let path = CString::new("/proc").unwrap();
    let fstype = CString::new("proc").unwrap();
    let source = CString::new("proc").unwrap();
    let opts = CString::new(options).map_err(|e| e.to_string())?;
    unsafe {
        libc::umount2(path.as_ptr(), libc::MNT_DETACH);
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

fn apply_seccomp(sandbox: &SandboxConfig) -> Result<(), String> {
    if sandbox.restrict_namespaces.is_some() {}
    if !sandbox.system_call_filter.is_empty() {}
    if sandbox.protect_clock {}
    if sandbox.protect_hostname {}
    Ok(())
}
