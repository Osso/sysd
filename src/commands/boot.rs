//! Boot to default target

use sysd::manager::Manager;

pub async fn boot(dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut manager = Manager::new();

    // Get the default target
    let target = manager.get_default_target()?;
    println!("Default target: {}", target);

    if dry_run {
        // Just show what would be started
        manager.load(&target).await?;

        // Show dependencies
        println!("\nWould start:");
        if let Some(unit) = manager.get_unit(&target) {
            let section = unit.unit_section();
            for dep in &section.requires {
                println!("  {} (required)", dep);
            }
            for dep in &section.wants {
                println!("  {} (wanted)", dep);
            }
            for dep in unit.wants_dir() {
                println!("  {}", dep);
            }
        }
    } else {
        // Actually start services
        println!("\nStarting services...");
        let started = manager.start_with_deps(&target).await?;
        println!("\nStarted {} services:", started.len());
        for name in &started {
            println!("  {}", name);
        }
    }

    Ok(())
}
