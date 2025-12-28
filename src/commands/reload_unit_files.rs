//! Reload unit files from disk
//!
//! Equivalent to systemd's daemon-reload.
//! Currently a no-op since sysd isn't a persistent daemon yet.

pub async fn reload_unit_files() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: When sysd becomes a daemon, this will signal it to rescan unit files
    // For now, just acknowledge the request
    println!("Unit files reloaded");
    Ok(())
}
