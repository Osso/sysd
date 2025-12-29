//! Integration tests for PID 1 mode using Docker
//!
//! These tests require Docker to be running and will spin up containers
//! to test PID 1 functionality (zombie reaping, signal handling, shutdown).
//!
//! Run with: cargo test --test pid1_integration -- --ignored --test-threads=1

use std::process::{Command, Stdio};
use std::time::Duration;
use std::thread;
use std::sync::Mutex;

// Ensure only one test runs at a time (they share the same container)
static DOCKER_LOCK: Mutex<()> = Mutex::new(());

/// Check if Docker is available
fn docker_available() -> bool {
    Command::new("docker")
        .args(["info"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check if the sysd container image exists
fn image_exists() -> bool {
    Command::new("docker")
        .args(["image", "inspect", "sysd-sysd"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run docker compose command and return output
fn docker_compose(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("docker")
        .arg("compose")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
}

/// Get container logs
fn get_logs() -> String {
    docker_compose(&["logs", "--no-color"])
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Start the sysd container
fn start_container() -> bool {
    docker_compose(&["up", "-d"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Stop the sysd container
fn stop_container() -> bool {
    docker_compose(&["down"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Execute a command in the running container
fn exec_in_container(cmd: &[&str]) -> std::io::Result<std::process::Output> {
    let mut args = vec!["compose", "exec", "-T", "sysd"];
    args.extend(cmd);
    Command::new("docker")
        .args(&args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
}

/// Send a signal to PID 1 in the container
fn send_signal(signal: &str) -> bool {
    exec_in_container(&["kill", &format!("-{}", signal), "1"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
#[ignore] // Requires Docker, run with --test-threads=1
fn test_pid1_detection() {
    let _lock = DOCKER_LOCK.lock().unwrap();

    if !docker_available() {
        eprintln!("Docker not available, skipping test");
        return;
    }
    if !image_exists() {
        eprintln!("sysd-sysd image not found, run 'docker compose build' first");
        return;
    }

    // Ensure clean state
    stop_container();

    // Start container
    assert!(start_container(), "Failed to start container");
    thread::sleep(Duration::from_secs(2));

    // Check logs for PID 1 detection
    let logs = get_logs();

    // Cleanup
    stop_container();

    assert!(
        logs.contains("Running as PID 1"),
        "sysd did not detect PID 1 mode. Logs:\n{}", logs
    );
}

#[test]
#[ignore] // Requires Docker, run with --test-threads=1
fn test_essential_filesystems_mounted() {
    let _lock = DOCKER_LOCK.lock().unwrap();

    if !docker_available() || !image_exists() {
        eprintln!("Docker or image not available, skipping test");
        return;
    }

    stop_container();
    assert!(start_container(), "Failed to start container");
    thread::sleep(Duration::from_secs(2));

    let logs = get_logs();
    stop_container();

    assert!(
        logs.contains("Essential filesystems mounted"),
        "Essential filesystems not mounted. Logs:\n{}", logs
    );
}

#[test]
#[ignore] // Requires Docker, run with --test-threads=1
fn test_zombie_reaping() {
    let _lock = DOCKER_LOCK.lock().unwrap();

    if !docker_available() || !image_exists() {
        eprintln!("Docker or image not available, skipping test");
        return;
    }

    stop_container();
    assert!(start_container(), "Failed to start container");
    thread::sleep(Duration::from_secs(2));

    // Spawn an orphan process that exits quickly
    exec_in_container(&["sh", "-c", "(sleep 0.1 &)"]).ok();
    thread::sleep(Duration::from_secs(1));

    // Check for zombies - there should be none
    let ps_output = exec_in_container(&["ps", "aux"])
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    stop_container();

    // Count zombie processes (state 'Z')
    let zombie_count = ps_output
        .lines()
        .filter(|line| line.contains(" Z ") || line.contains(" Z+ "))
        .count();

    assert_eq!(zombie_count, 0, "Found zombie processes:\n{}", ps_output);
}

#[test]
#[ignore] // Requires Docker, run with --test-threads=1
fn test_sigusr1_state_dump() {
    let _lock = DOCKER_LOCK.lock().unwrap();

    if !docker_available() || !image_exists() {
        eprintln!("Docker or image not available, skipping test");
        return;
    }

    stop_container();
    assert!(start_container(), "Failed to start container");
    thread::sleep(Duration::from_secs(2));

    // Send SIGUSR1
    assert!(send_signal("USR1"), "Failed to send SIGUSR1");
    thread::sleep(Duration::from_secs(1));

    let logs = get_logs();
    stop_container();

    assert!(
        logs.contains("Received SIGUSR1, dumping state"),
        "SIGUSR1 not handled. Logs:\n{}", logs
    );
}

#[test]
#[ignore] // Requires Docker, run with --test-threads=1
fn test_sigterm_shutdown_sequence() {
    let _lock = DOCKER_LOCK.lock().unwrap();

    if !docker_available() || !image_exists() {
        eprintln!("Docker or image not available, skipping test");
        return;
    }

    stop_container();
    assert!(start_container(), "Failed to start container");
    thread::sleep(Duration::from_secs(2));

    // Use docker compose stop which follows logs until container exits
    let _ = Command::new("docker")
        .args(["compose", "stop"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Failed to run docker compose stop");

    // Get logs before down (container still exists, just stopped)
    let logs = get_logs();

    // Now clean up
    docker_compose(&["down"]).ok();

    // Verify shutdown sequence
    assert!(
        logs.contains("Received SIGTERM, initiating poweroff"),
        "SIGTERM not received. Logs:\n{}", logs
    );
    assert!(
        logs.contains("Initiating Poweroff sequence"),
        "Shutdown sequence not initiated. Logs:\n{}", logs
    );
    assert!(
        logs.contains("Sending SIGTERM to all processes"),
        "SIGTERM not sent to processes. Logs:\n{}", logs
    );
    assert!(
        logs.contains("Syncing filesystems"),
        "Filesystems not synced. Logs:\n{}", logs
    );
    assert!(
        logs.contains("Unmounting filesystems"),
        "Filesystems not unmounted. Logs:\n{}", logs
    );
    assert!(
        logs.contains("Executing Poweroff"),
        "Poweroff not executed. Logs:\n{}", logs
    );
}

#[test]
#[ignore] // Requires Docker, run with --test-threads=1
fn test_sysd_is_pid1() {
    let _lock = DOCKER_LOCK.lock().unwrap();

    if !docker_available() || !image_exists() {
        eprintln!("Docker or image not available, skipping test");
        return;
    }

    stop_container();
    assert!(start_container(), "Failed to start container");
    thread::sleep(Duration::from_secs(2));

    // Verify sysd is PID 1
    let ps_output = exec_in_container(&["ps", "-p", "1", "-o", "comm="])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    stop_container();

    assert_eq!(ps_output, "sysd", "sysd is not PID 1, got: {}", ps_output);
}
