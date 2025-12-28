//! Show unit dependencies

use sysd::manager::Manager;

pub async fn deps(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut manager = Manager::new();

    // Load the unit
    manager.load(name).await?;

    let unit = manager.get_unit(name)
        .ok_or("Unit not found after loading")?;

    let section = unit.unit_section();

    println!("Dependencies for {}:", unit.name());
    println!();

    if !section.after.is_empty() {
        println!("After:");
        for dep in &section.after {
            println!("  {}", dep);
        }
    }

    if !section.before.is_empty() {
        println!("Before:");
        for dep in &section.before {
            println!("  {}", dep);
        }
    }

    if !section.requires.is_empty() {
        println!("Requires:");
        for dep in &section.requires {
            println!("  {}", dep);
        }
    }

    if !section.wants.is_empty() {
        println!("Wants:");
        for dep in &section.wants {
            println!("  {}", dep);
        }
    }

    // Show .wants directory entries for targets
    let wants_dir = unit.wants_dir();
    if !wants_dir.is_empty() {
        println!("Wants (from .wants/):");
        for dep in wants_dir {
            println!("  {}", dep);
        }
    }

    Ok(())
}
