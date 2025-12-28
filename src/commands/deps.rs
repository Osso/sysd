//! Show service dependencies

use sysd::manager::Manager;

pub async fn deps(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut manager = Manager::new();

    // Load the service
    manager.load(name).await?;

    let svc = manager.get_service(name)
        .ok_or("Service not found after loading")?;

    println!("Dependencies for {}:", svc.name);
    println!();

    if !svc.unit.after.is_empty() {
        println!("After:");
        for dep in &svc.unit.after {
            println!("  {}", dep);
        }
    }

    if !svc.unit.before.is_empty() {
        println!("Before:");
        for dep in &svc.unit.before {
            println!("  {}", dep);
        }
    }

    if !svc.unit.requires.is_empty() {
        println!("Requires:");
        for dep in &svc.unit.requires {
            println!("  {}", dep);
        }
    }

    if !svc.unit.wants.is_empty() {
        println!("Wants:");
        for dep in &svc.unit.wants {
            println!("  {}", dep);
        }
    }

    Ok(())
}
