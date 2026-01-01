use sysd::manager::Manager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("error")).init();
    let mut mgr = Manager::new();

    let target = mgr.get_default_target()?;

    // Load local-fs.target first so fstab mounts can be added to it
    let _ = mgr.load("local-fs.target").await;

    // Load fstab-generated mounts (like systemd-fstab-generator)
    if let Err(e) = mgr.load_fstab() {
        eprintln!("Warning: failed to load fstab: {}", e);
    }
    let plan = mgr.get_boot_plan(&target).await?;
    
    // Check what dbus units are loaded
    eprintln!("Loaded dbus units:");
    for (name, unit, _) in mgr.list_units() {
        if name.contains("dbus") {
            eprintln!("  key={} name={}", name, unit.name());
        }
    }
    eprintln!();
    
    for unit in &plan {
        println!("{}", unit);
    }
    Ok(())
}
