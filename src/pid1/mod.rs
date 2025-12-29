//! PID 1 functionality
//!
//! Handles responsibilities specific to running as init (PID 1):
//! - Mounting essential filesystems
//! - Zombie process reaping
//! - Signal handling
//! - Orderly shutdown

mod mount;
mod reaper;
mod shutdown;
mod signals;

pub use mount::{mount_essential_filesystems, MountError};
pub use reaper::ZombieReaper;
pub use shutdown::{shutdown, ShutdownType};
pub use signals::{SignalHandler, SysdSignal};

use std::process;

/// Check if we are running as PID 1
pub fn is_pid1() -> bool {
    process::id() == 1
}

/// Initialize PID 1 environment
///
/// This should be called early in startup when running as init.
/// Sets up essential filesystems and signal handlers.
pub fn init() -> Result<(), Pid1Error> {
    if !is_pid1() {
        log::debug!("Not PID 1 (pid={}), skipping init setup", process::id());
        return Ok(());
    }

    log::info!("Running as PID 1, initializing init environment");

    // Mount essential filesystems
    mount::mount_essential_filesystems()?;

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Pid1Error {
    #[error("Mount failed: {0}")]
    Mount(#[from] MountError),

    #[error("Signal setup failed: {0}")]
    Signal(String),
}
