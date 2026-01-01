use sysd::manager::Manager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mgr = Manager::new();
    
    // Load getty.target
    mgr.load("getty.target").await?;
    
    // Check what's stored
    if let Some(unit) = mgr.get_unit("getty.target") {
        eprintln!("Stored getty.target wants_dir: {:?}", unit.wants_dir());
        eprintln!("unit_section().wants: {:?}", unit.unit_section().wants);
    }
    
    Ok(())
}
