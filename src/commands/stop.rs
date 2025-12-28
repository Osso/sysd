//! Stop a service

use sysd::manager::Manager;

pub async fn stop(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut manager = Manager::new();

    // Load the service first
    manager.load(name).await?;

    // We need to find the running process
    // For now, this is a limitation - we need a daemon to track state
    println!("Note: sysd currently requires a running daemon to track processes.");
    println!("Use 'sysd daemon' to start the service manager daemon.");

    // Try to stop anyway (will fail if not tracked)
    match manager.stop(name).await {
        Ok(()) => println!("● {} - stopped", name),
        Err(e) => println!("● {} - {}", name, e),
    }

    Ok(())
}
