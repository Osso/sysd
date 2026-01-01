use sysd::manager::Manager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mgr = Manager::new();
    mgr.load("getty@tty1.service").await?;
    
    for (key, unit, _) in mgr.list_units() {
        eprintln!("Key: {}, unit.name(): {}", key, unit.name());
    }
    Ok(())
}
