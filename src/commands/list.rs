//! List units

use std::path::Path;

pub async fn list(user: bool) -> Result<(), Box<dyn std::error::Error>> {
    let paths = if user {
        vec![
            dirs::config_dir()
                .map(|p| p.join("systemd/user"))
                .unwrap_or_default(),
            dirs::home_dir()
                .map(|p| p.join(".config/systemd/user"))
                .unwrap_or_default(),
            Path::new("/usr/lib/systemd/user").to_path_buf(),
        ]
    } else {
        vec![
            Path::new("/etc/systemd/system").to_path_buf(),
            Path::new("/usr/lib/systemd/system").to_path_buf(),
        ]
    };

    let scope = if user { "user" } else { "system" };
    println!("UNIT                                 STATE       DESCRIPTION");

    let mut count = 0;
    for base in &paths {
        if !base.exists() {
            continue;
        }

        let entries = match std::fs::read_dir(base) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Only .service files, skip symlinks for now
            if path.extension().map_or(true, |e| e != "service") {
                continue;
            }
            if path.is_symlink() {
                continue;
            }

            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");

            // Try to load and get description
            match sysd::units::load_service(&path).await {
                Ok(svc) => {
                    let desc = svc.unit.description
                        .as_deref()
                        .unwrap_or("-")
                        .chars()
                        .take(40)
                        .collect::<String>();

                    println!(
                        "{:<36} {:<11} {}",
                        name,
                        "loaded",
                        desc
                    );
                    count += 1;
                }
                Err(_) => {
                    println!("{:<36} {:<11} (parse error)", name, "error");
                }
            }
        }
    }

    println!();
    println!("{} {} units listed", count, scope);

    Ok(())
}
