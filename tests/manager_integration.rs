//! Integration tests for the Manager

use std::path::PathBuf;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use sysd::manager::Manager;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique_test_dir() -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = PathBuf::from(format!("/tmp/sysd-test-{}-{}", std::process::id(), id));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_test_service(dir: &PathBuf, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

#[tokio::test]
async fn test_manager_load_unit() {
    let dir = unique_test_dir();
    let path = write_test_service(&dir, "test-load.service", r#"
[Unit]
Description=Test service for loading

[Service]
Type=simple
ExecStart=/bin/true
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&path).await.unwrap();

    assert!(manager.get_unit("test-load.service").is_some());
    let unit = manager.get_unit("test-load.service").unwrap();
    assert_eq!(unit.name(), "test-load.service");
}

#[tokio::test]
async fn test_manager_unit_not_found() {
    let mut manager = Manager::new();
    let result = manager.load("nonexistent-unit-12345.service").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_manager_start_simple_service() {
    let dir = unique_test_dir();
    let path = write_test_service(&dir, "test-start.service", r#"
[Unit]
Description=Test service for starting

[Service]
Type=simple
ExecStart=/bin/sleep 60
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&path).await.unwrap();

    // Start the service
    manager.start("test-start.service").await.unwrap();

    // Check state
    let state = manager.status("test-start.service").unwrap();
    assert!(state.is_active());
    assert!(state.main_pid.is_some());

    // Stop the service
    manager.stop("test-start.service").await.unwrap();

    // Check state after stop
    let state = manager.status("test-start.service").unwrap();
    assert!(!state.is_active());
}

#[tokio::test]
async fn test_manager_restart_service() {
    let dir = unique_test_dir();
    let path = write_test_service(&dir, "test-restart.service", r#"
[Unit]
Description=Test service for restarting

[Service]
Type=simple
ExecStart=/bin/sleep 60
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&path).await.unwrap();

    // Start the service
    manager.start("test-restart.service").await.unwrap();
    let pid1 = manager.status("test-restart.service").unwrap().main_pid;

    // Restart
    manager.restart("test-restart.service").await.unwrap();
    let pid2 = manager.status("test-restart.service").unwrap().main_pid;

    // PID should be different after restart
    assert_ne!(pid1, pid2);

    // Cleanup
    manager.stop("test-restart.service").await.unwrap();
}

#[tokio::test]
async fn test_manager_already_active() {
    let dir = unique_test_dir();
    let path = write_test_service(&dir, "test-active.service", r#"
[Unit]
Description=Test already active

[Service]
Type=simple
ExecStart=/bin/sleep 60
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&path).await.unwrap();

    manager.start("test-active.service").await.unwrap();

    // Starting again should fail
    let result = manager.start("test-active.service").await;
    assert!(result.is_err());

    manager.stop("test-active.service").await.unwrap();
}

#[tokio::test]
async fn test_manager_stop_not_active() {
    let dir = unique_test_dir();
    let path = write_test_service(&dir, "test-stop-inactive.service", r#"
[Unit]
Description=Test stop inactive

[Service]
Type=simple
ExecStart=/bin/sleep 60
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&path).await.unwrap();

    // Stopping a not-started service should fail
    let result = manager.stop("test-stop-inactive.service").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_manager_normalize_name() {
    let dir = unique_test_dir();
    let path = write_test_service(&dir, "test-normalize.service", r#"
[Unit]
Description=Test name normalization

[Service]
Type=simple
ExecStart=/bin/true
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&path).await.unwrap();

    // Should work with or without .service suffix
    assert!(manager.get_unit("test-normalize").is_some());
    assert!(manager.get_unit("test-normalize.service").is_some());
}

#[tokio::test]
async fn test_manager_list_units() {
    let dir = unique_test_dir();
    write_test_service(&dir, "test-list1.service", r#"
[Unit]
Description=Test list 1

[Service]
ExecStart=/bin/true
"#);
    write_test_service(&dir, "test-list2.service", r#"
[Unit]
Description=Test list 2

[Service]
ExecStart=/bin/true
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&dir.join("test-list1.service")).await.unwrap();
    manager.load_from_path(&dir.join("test-list2.service")).await.unwrap();

    let units: Vec<_> = manager.list().collect();
    assert_eq!(units.len(), 2);
}

#[tokio::test]
async fn test_manager_get_default_target() {
    // This test depends on the system having default.target
    let manager = Manager::new();
    let result = manager.get_default_target();
    // Just check it doesn't panic - actual result depends on system config
    let _ = result;
}

#[tokio::test]
async fn test_manager_service_description() {
    let dir = unique_test_dir();
    let path = write_test_service(&dir, "test-desc.service", r#"
[Unit]
Description=My test service description

[Service]
ExecStart=/bin/true
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&path).await.unwrap();

    let unit = manager.get_unit("test-desc.service").unwrap();
    assert_eq!(
        unit.unit_section().description,
        Some("My test service description".to_string())
    );
}

#[tokio::test]
async fn test_manager_service_dependencies() {
    let dir = unique_test_dir();
    let path = write_test_service(&dir, "test-deps.service", r#"
[Unit]
Description=Test deps
After=network.target
Requires=dbus.service
Wants=syslog.service

[Service]
ExecStart=/bin/true
"#);

    let mut manager = Manager::new();
    manager.load_from_path(&path).await.unwrap();

    let unit = manager.get_unit("test-deps.service").unwrap();
    let section = unit.unit_section();
    assert!(section.after.contains(&"network.target".to_string()));
    assert!(section.requires.contains(&"dbus.service".to_string()));
    assert!(section.wants.contains(&"syslog.service".to_string()));
}
