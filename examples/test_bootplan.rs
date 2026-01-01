use sysd::manager::Manager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let mut mgr = Manager::new();
    let plan = mgr.get_boot_plan("getty.target").await?;
    
    eprintln!("Boot plan for getty.target:");
    for unit in &plan {
        eprintln!("  {}", unit);
    }
    
    Ok(())
}
