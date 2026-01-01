use sysd::manager::Manager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mgr = Manager::new();
    match mgr.load("getty@tty1.service").await {
        Ok(name) => println!("Loaded as: {}", name),
        Err(e) => println!("Error: {:?}", e),
    }
    Ok(())
}
