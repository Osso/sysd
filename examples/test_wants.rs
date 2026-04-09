use std::path::Path;
use sysd::units;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new("/usr/lib/systemd/system/getty.target");
    let target = units::load_target(path).await?;
    println!("getty.target wants_dir: {:?}", target.wants_dir);
    Ok(())
}
