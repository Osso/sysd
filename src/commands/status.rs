//! Show service status

use sysd::manager::Manager;

pub async fn status(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut manager = Manager::new();

    // Load the service
    manager.load(name).await?;

    let service = manager.get_service(name)
        .ok_or("Service not found")?;

    let state = manager.status(name)
        .ok_or("Service state not found")?;

    // Header
    let status_symbol = match state.active {
        sysd::manager::ActiveState::Active => "●",
        sysd::manager::ActiveState::Inactive => "○",
        sysd::manager::ActiveState::Failed => "×",
        _ => "◐",
    };

    println!(
        "{} {} - {}",
        status_symbol,
        service.name,
        service.unit.description.as_deref().unwrap_or("(no description)")
    );

    // State
    println!(
        "     Loaded: loaded ({:?})",
        service.service.service_type
    );
    println!(
        "     Active: {} ({})",
        state.active.as_str(),
        state.sub.as_str()
    );

    // PID if running
    if let Some(pid) = state.main_pid {
        println!("   Main PID: {}", pid);
    }

    // ExecStart
    if !service.service.exec_start.is_empty() {
        println!("   ExecStart: {}", service.service.exec_start[0]);
    }

    // Restart policy
    println!("    Restart: {:?}", service.service.restart);

    Ok(())
}
