//! Shared low-level sandbox helpers used by manager and executor.

/// NoNewPrivileges=yes - prevents privilege escalation via execve().
pub fn apply_no_new_privileges() -> Result<(), String> {
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err("Failed to set PR_SET_NO_NEW_PRIVS".to_string());
        }
    }
    Ok(())
}

/// PrivateNetwork=yes - create an isolated network namespace.
pub fn apply_private_network() -> Result<(), String> {
    unsafe {
        if libc::unshare(libc::CLONE_NEWNET) != 0 {
            return Err("Failed to create network namespace".to_string());
        }
    }
    Ok(())
}
