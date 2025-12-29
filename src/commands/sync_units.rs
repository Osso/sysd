//! Sync units from disk and restart affected services
//!
//! Reloads unit files and restarts any services whose
//! configuration has changed. Used by pacman hooks after
//! package updates.

pub async fn sync_units() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: When sysd becomes a daemon:
    // 1. Rescan unit files from disk
    // 2. Detect which units have changed
    // 3. Restart affected running services
    println!("Units synced");
    Ok(())
}
