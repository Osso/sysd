use sysd::manager::Manager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mgr = Manager::new();
    
    match mgr.load_fstab() {
        Ok(count) => eprintln!("Loaded {} mounts from fstab", count),
        Err(e) => eprintln!("Error: {:?}", e),
    }
    
    // List all mount units
    for (name, _, _) in mgr.list_units() {
        if name.ends_with(".mount") {
            eprintln!("  {}", name);
        }
    }
    
    Ok(())
}
