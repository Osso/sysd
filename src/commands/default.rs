//! Show or set default target

use sysd::manager::Manager;

pub async fn default_target() -> Result<(), Box<dyn std::error::Error>> {
    let manager = Manager::new();
    let target = manager.get_default_target()?;
    println!("{}", target);
    Ok(())
}
