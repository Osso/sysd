//! Integration tests for the unit file parser
//!
//! Tests parsing of real systemd service files from the system.

use std::path::Path;

/// Parse all .service files in a directory (non-recursively)
async fn parse_services_in_dir(dir: &Path) -> (usize, Vec<(String, String)>) {
    let mut success = 0;
    let mut failures = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, vec![]);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "service") {
            // Skip symlinks to avoid duplicates
            if path.is_symlink() {
                continue;
            }

            match sysd::units::load_service(&path).await {
                Ok(_) => success += 1,
                Err(e) => {
                    failures.push((path.display().to_string(), e.to_string()));
                }
            }
        }
    }

    (success, failures)
}

/// Recursively find and parse all .service files
async fn parse_all_services(base: &Path) -> (usize, Vec<(String, String)>) {
    let mut total_success = 0;
    let mut all_failures = Vec::new();

    fn visit_dirs(dir: &Path, dirs: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && !path.is_symlink() {
                    dirs.push(path.clone());
                    visit_dirs(&path, dirs);
                }
            }
        }
    }

    let mut dirs = vec![base.to_path_buf()];
    visit_dirs(base, &mut dirs);

    for dir in dirs {
        let (success, failures) = parse_services_in_dir(&dir).await;
        total_success += success;
        all_failures.extend(failures);
    }

    (total_success, all_failures)
}

#[tokio::test]
async fn test_parse_etc_systemd() {
    let (success, failures) = parse_all_services(Path::new("/etc/systemd")).await;

    if !failures.is_empty() {
        eprintln!("\nFailed to parse {} files:", failures.len());
        for (path, err) in &failures {
            eprintln!("  {}: {}", path, err);
        }
    }

    assert!(
        failures.is_empty(),
        "Failed to parse {} out of {} service files in /etc/systemd",
        failures.len(),
        success + failures.len()
    );

    // Ensure we actually tested something
    assert!(success > 0, "No service files found in /etc/systemd");
    eprintln!(
        "Successfully parsed {} service files from /etc/systemd",
        success
    );
}

#[tokio::test]
async fn test_parse_usr_lib_systemd() {
    let (success, failures) = parse_all_services(Path::new("/usr/lib/systemd/system")).await;

    if !failures.is_empty() {
        eprintln!("\nFailed to parse {} files:", failures.len());
        for (path, err) in &failures {
            eprintln!("  {}: {}", path, err);
        }
    }

    assert!(
        failures.is_empty(),
        "Failed to parse {} out of {} service files in /usr/lib/systemd/system",
        failures.len(),
        success + failures.len()
    );

    // Ensure we actually tested something
    assert!(
        success > 0,
        "No service files found in /usr/lib/systemd/system"
    );
    eprintln!(
        "Successfully parsed {} service files from /usr/lib/systemd/system",
        success
    );
}

// Test specific well-known services
#[tokio::test]
async fn test_parse_docker_service() {
    let path = Path::new("/usr/lib/systemd/system/docker.service");
    if !path.exists() {
        eprintln!("Skipping: docker.service not found");
        return;
    }

    let svc = sysd::units::load_service(path).await.unwrap();
    assert_eq!(svc.service.service_type, sysd::ServiceType::Notify);
    assert_eq!(svc.service.restart, sysd::units::RestartPolicy::Always);
    assert!(!svc.service.exec_start.is_empty());
}

#[tokio::test]
async fn test_parse_networkmanager_service() {
    let path = Path::new("/usr/lib/systemd/system/NetworkManager.service");
    if !path.exists() {
        eprintln!("Skipping: NetworkManager.service not found");
        return;
    }

    let svc = sysd::units::load_service(path).await.unwrap();
    assert_eq!(svc.service.service_type, sysd::ServiceType::Dbus);
    assert!(!svc.unit.after.is_empty());
}

#[tokio::test]
async fn test_parse_sshd_service() {
    let path = Path::new("/usr/lib/systemd/system/sshd.service");
    if !path.exists() {
        eprintln!("Skipping: sshd.service not found");
        return;
    }

    let svc = sysd::units::load_service(path).await.unwrap();
    assert!(!svc.service.exec_start.is_empty());
}
