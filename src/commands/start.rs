//! Start a service

use std::path::Path;
use sysd::manager::Manager;

pub async fn start(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut manager = Manager::new();

    // If name looks like a path, load from path
    if name.contains('/') {
        manager.load_from_path(Path::new(name)).await?;
        let service_name = Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(name);
        manager.start(service_name).await?;

        if let Some(state) = manager.status(service_name) {
            println!(
                "● {} - started (PID {})",
                service_name,
                state.main_pid.unwrap_or(0)
            );
        }
    } else {
        manager.start(name).await?;

        if let Some(state) = manager.status(name) {
            println!(
                "● {} - started (PID {})",
                name,
                state.main_pid.unwrap_or(0)
            );
        }
    }

    Ok(())
}
