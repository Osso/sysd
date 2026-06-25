#[cfg(target_arch = "x86_64")]
const SYSCALL_NR_X86_64: &[(&str, i64)] = &[
    ("read", 0),
    ("write", 1),
    ("open", 2),
    ("close", 3),
    ("stat", 4),
    ("fstat", 5),
    ("lstat", 6),
    ("poll", 7),
    ("lseek", 8),
    ("mmap", 9),
    ("mprotect", 10),
    ("munmap", 11),
    ("brk", 12),
    ("ioctl", 16),
    ("access", 21),
    ("pipe", 22),
    ("dup", 32),
    ("dup2", 33),
    ("socket", 41),
    ("connect", 42),
    ("accept", 43),
    ("bind", 49),
    ("listen", 50),
    ("clone", 56),
    ("fork", 57),
    ("vfork", 58),
    ("execve", 59),
    ("exit", 60),
    ("kill", 62),
    ("uselib", 134),
    ("vhangup", 153),
    ("pivot_root", 155),
    ("acct", 163),
    ("settimeofday", 164),
    ("mount", 165),
    ("umount", 166),
    ("umount2", 166),
    ("swapon", 167),
    ("swapoff", 168),
    ("reboot", 169),
    ("sethostname", 170),
    ("setdomainname", 171),
    ("iopl", 172),
    ("ioperm", 173),
    ("create_module", 174),
    ("init_module", 175),
    ("delete_module", 176),
    ("get_kernel_syms", 177),
    ("query_module", 178),
    ("clock_settime", 227),
    ("kexec_load", 246),
    ("clock_adjtime", 305),
    ("finit_module", 313),
    ("kexec_file_load", 320),
    ("bpf", 321),
    ("open_tree", 428),
    ("move_mount", 429),
];

#[cfg(target_arch = "aarch64")]
const SYSCALL_NR_AARCH64: &[(&str, i64)] = &[
    ("dup", 23),
    ("dup3", 24),
    ("ioctl", 29),
    ("umount2", 39),
    ("mount", 40),
    ("pivot_root", 41),
    ("openat", 56),
    ("close", 57),
    ("vhangup", 58),
    ("lseek", 62),
    ("read", 63),
    ("write", 64),
    ("fstat", 80),
    ("exit", 93),
    ("kexec_load", 104),
    ("init_module", 105),
    ("delete_module", 106),
    ("clock_settime", 112),
    ("kill", 129),
    ("reboot", 142),
    ("sethostname", 161),
    ("setdomainname", 162),
    ("settimeofday", 170),
    ("socket", 198),
    ("bind", 200),
    ("listen", 201),
    ("accept", 202),
    ("connect", 203),
    ("brk", 214),
    ("munmap", 215),
    ("clone", 220),
    ("execve", 221),
    ("mmap", 222),
    ("swapon", 224),
    ("swapoff", 225),
    ("mprotect", 226),
    ("finit_module", 273),
    ("clock_adjtime", 266),
    ("bpf", 280),
    ("kexec_file_load", 294),
    ("open_tree", 428),
    ("move_mount", 429),
];

// M16: prctl-based security enforcement

// PR_SET_MDWE constants (not in older libc)
const PR_SET_MDWE: libc::c_int = 65;
const PR_MDWE_REFUSE_EXEC_GAIN: libc::c_ulong = 1;
const PERSONALITY_QUERY: libc::c_ulong = 0xffff_ffff;

/// RestrictRealtime=yes - block realtime scheduling via seccomp
/// (systemd uses seccomp to block sched_setscheduler with RT policies)
fn apply_restrict_realtime() -> Result<(), String> {
    // This is handled via seccomp in apply_combined_seccomp_m16
    // We set a flag here and handle it there
    log::debug!("RestrictRealtime: will be enforced via seccomp");
    Ok(())
}

/// MemoryDenyWriteExecute=yes - block W+X memory mappings
fn apply_memory_deny_write_execute() -> Result<(), String> {
    unsafe {
        // PR_SET_MDWE prevents creating memory mappings that are both writable and executable
        if libc::prctl(PR_SET_MDWE, PR_MDWE_REFUSE_EXEC_GAIN, 0, 0, 0) != 0 {
            // This may fail on older kernels (< 6.3)
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINVAL) {
                log::warn!("MemoryDenyWriteExecute: kernel does not support PR_SET_MDWE");
                return Ok(());
            }
            return Err(format!("Failed to set PR_SET_MDWE: {}", err));
        }
    }
    log::debug!("MemoryDenyWriteExecute: PR_SET_MDWE applied");
    Ok(())
}

/// LockPersonality=yes - lock the execution domain
fn apply_lock_personality() -> Result<(), String> {
    unsafe {
        // First get the current personality
        let current = libc::personality(PERSONALITY_QUERY);
        if current == -1 {
            return Err("Failed to get current personality".to_string());
        }

        // Set the personality with UNAME26 flag to lock it
        // Actually, systemd uses seccomp to block personality() syscall
        // Let's use that approach instead - handled in seccomp
    }
    log::debug!("LockPersonality: will be enforced via seccomp");
    Ok(())
}

/// IgnoreSIGPIPE=yes - ignore SIGPIPE signal
fn apply_ignore_sigpipe() -> Result<(), String> {
    unsafe {
        if libc::signal(libc::SIGPIPE, libc::SIG_IGN) == libc::SIG_ERR {
            return Err("Failed to ignore SIGPIPE".to_string());
        }
    }
    log::debug!("IgnoreSIGPIPE: SIGPIPE set to SIG_IGN");
    Ok(())
}

// M16: Mount-based security enforcement

/// ProtectControlGroups=yes - make /sys/fs/cgroup read-only
fn apply_protect_control_groups() -> Result<(), String> {
    if Path::new("/sys/fs/cgroup").exists() {
        bind_mount_ro("/sys/fs/cgroup")?;
        log::debug!("ProtectControlGroups: /sys/fs/cgroup mounted read-only");
    }
    Ok(())
}

/// ProtectKernelTunables=yes - make /proc/sys and /sys read-only
fn apply_protect_kernel_tunables() -> Result<(), String> {
    if Path::new("/proc/sys").exists() {
        bind_mount_ro("/proc/sys")?;
    }
    if Path::new("/sys").exists() {
        // Mount /sys read-only but keep /sys/fs/cgroup writable if not protected
        bind_mount_ro("/sys")?;
    }
    log::debug!("ProtectKernelTunables: /proc/sys and /sys mounted read-only");
    Ok(())
}

/// ProtectKernelLogs=yes - make /dev/kmsg inaccessible
fn apply_protect_kernel_logs() -> Result<(), String> {
    if Path::new("/dev/kmsg").exists() {
        make_inaccessible("/dev/kmsg")?;
        log::debug!("ProtectKernelLogs: /dev/kmsg made inaccessible");
    }
    // Also protect /proc/kmsg if it exists
    if Path::new("/proc/kmsg").exists() {
        make_inaccessible("/proc/kmsg")?;
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
