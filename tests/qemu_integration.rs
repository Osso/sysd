//! QEMU-based integration tests for PID 1 mount functionality
//!
//! These tests boot a minimal Linux system with sysd as init to verify
//! that essential filesystems are actually mounted (not skipped like in Docker).
//!
//! Run with: cargo test --test qemu_integration -- --ignored
//!
//! Requirements:
//! - qemu-system-x86_64
//! - Linux kernel at /boot/vmlinuz-linux (or set KERNEL env var)
//! - cargo build --release

use std::process::Command;
use std::path::Path;
use std::fs;

/// Check if QEMU is available
fn qemu_available() -> bool {
    Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if kernel is available
fn kernel_available() -> bool {
    std::env::var("KERNEL")
        .map(|k| Path::new(&k).exists())
        .unwrap_or_else(|_| {
            Path::new("/boot/vmlinuz-linux").exists() || Path::new("/boot/vmlinuz").exists()
        })
}

/// Check if sysd release binary exists
fn sysd_binary_exists() -> bool {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .join("target/release/sysd")
        .exists()
}

/// Run the QEMU test script and return output
fn run_qemu_test() -> (bool, String) {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let script = Path::new(manifest_dir).join("tests/qemu/run-qemu-test.sh");

    let output = Command::new("bash")
        .arg(&script)
        .current_dir(manifest_dir)
        .output()
        .expect("Failed to run QEMU test script");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{}\n{}", stdout, stderr);

    (output.status.success(), combined)
}

#[test]
#[ignore] // Requires QEMU and kernel
fn test_qemu_pid1_mounts() {
    if !qemu_available() {
        eprintln!("QEMU not available, skipping test");
        return;
    }

    if !kernel_available() {
        eprintln!("Linux kernel not found, skipping test");
        eprintln!("Set KERNEL=/path/to/vmlinuz or ensure /boot/vmlinuz-linux exists");
        return;
    }

    if !sysd_binary_exists() {
        eprintln!("sysd release binary not found, run 'cargo build --release' first");
        return;
    }

    let (success, output) = run_qemu_test();

    // Print output for debugging
    println!("=== QEMU Test Output ===\n{}", output);

    assert!(success, "QEMU mount test failed");

    // Additional assertions on output
    assert!(
        output.contains("PID 1 detection: PASS"),
        "PID 1 detection failed"
    );
    assert!(
        output.contains("Filesystem mounting: PASS"),
        "Filesystem mounting failed"
    );
}

#[test]
#[ignore]
fn test_qemu_prerequisites() {
    // Just check prerequisites without running full test
    println!("QEMU available: {}", qemu_available());
    println!("Kernel available: {}", kernel_available());
    println!("sysd binary exists: {}", sysd_binary_exists());

    if !qemu_available() {
        println!("Install with: pacman -S qemu-system-x86");
    }
    if !sysd_binary_exists() {
        println!("Build with: cargo build --release");
    }
}
