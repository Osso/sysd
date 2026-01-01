use sysd::manager::Manager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    let mut mgr = Manager::new();
    
    // First load getty.target
    mgr.load("getty.target").await?;
    
    // Check what's in its wants_dir
    if let Some(unit) = mgr.get_unit("getty.target") {
        eprintln!("getty.target wants_dir: {:?}", unit.wants_dir());
    }
    
    // Now try loading getty@tty1.service
    match mgr.load("getty@tty1.service").await {
        Ok(name) => eprintln!("Loaded getty@tty1 as: {}", name),
        Err(e) => eprintln!("Error loading getty@tty1: {:?}", e),
    }
    
    // List what's loaded
    eprintln!("\nLoaded units:");
    for (name, _, _) in mgr.list_units() {
        if name.contains("getty") {
            eprintln!("  {}", name);
        }
    }
    
    Ok(())
}
