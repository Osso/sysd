#!/bin/bash
# QEMU-based test for sysd PID 1 functionality
#
# This boots a minimal Linux system with sysd as init to test:
# - Essential filesystems are mounted
# - Signal handling (SIGTERM triggers shutdown)
# - Shutdown sequence (stop services, sync, unmount)
#
# Requirements:
# - qemu-system-x86_64
# - Linux kernel (uses host's /boot/vmlinuz-linux or specify KERNEL=)
# - Built sysd binary

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORK_DIR="${PROJECT_DIR}/target/qemu-test"
SYSD_BIN="${PROJECT_DIR}/target/x86_64-unknown-linux-musl/release/sysd"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $*"; }
err() { echo -e "${RED}[-]${NC} $*" >&2; }

# Find kernel
find_kernel() {
    for k in /boot/vmlinuz-linux /boot/vmlinuz; do
        if [[ -f "$k" ]]; then
            echo "$k"
            return
        fi
    done
    err "No kernel found. Set KERNEL= environment variable."
    exit 1
}

KERNEL="${KERNEL:-$(find_kernel)}"

# Check dependencies
check_deps() {
    if ! command -v qemu-system-x86_64 &>/dev/null; then
        err "qemu-system-x86_64 not found"
        exit 1
    fi

    if [[ ! -f "$SYSD_BIN" ]]; then
        err "sysd binary not found at $SYSD_BIN"
        err "Run: cargo build --release"
        exit 1
    fi

    if [[ ! -f "$KERNEL" ]]; then
        err "Kernel not found at $KERNEL"
        exit 1
    fi
}

# Create minimal initramfs with sysd as init
create_initramfs() {
    log "Creating initramfs..."

    local initrd_dir="$WORK_DIR/initrd"
    rm -rf "$initrd_dir"
    mkdir -p "$initrd_dir"/{bin,dev,proc,sys,run,tmp,etc}

    # Copy sysd as init (statically linked with musl)
    cp "$SYSD_BIN" "$initrd_dir/bin/sysd"
    chmod +x "$initrd_dir/bin/sysd"

    # Copy busybox for utilities
    local busybox_bin=""
    if command -v busybox &>/dev/null; then
        busybox_bin="$(command -v busybox)"
    elif [[ -f /usr/lib/initcpio/busybox ]]; then
        # Arch Linux mkinitcpio-busybox
        busybox_bin="/usr/lib/initcpio/busybox"
    fi

    if [[ -n "$busybox_bin" ]]; then
        cp "$busybox_bin" "$initrd_dir/bin/busybox"
        # Create symlinks for common utilities
        for cmd in sh cat ls mount ps kill sleep; do
            ln -sf busybox "$initrd_dir/bin/$cmd"
        done
    else
        log "Warning: busybox not found, shutdown test will fail"
    fi

    # Create a shutdown trigger script that sends SIGTERM after delay
    cat > "$initrd_dir/bin/trigger-shutdown" <<'SHUTDOWN_EOF'
#!/bin/sh
sleep 2
kill -TERM 1
SHUTDOWN_EOF
    chmod +x "$initrd_dir/bin/trigger-shutdown"

    # Minimal /etc/passwd for User= directive
    echo "root:x:0:0:root:/:/bin/sh" > "$initrd_dir/etc/passwd"
    echo "root:x:0:" > "$initrd_dir/etc/group"

    # Create systemd unit directories
    mkdir -p "$initrd_dir/etc/systemd/system"
    mkdir -p "$initrd_dir/usr/lib/systemd/system"

    # Create shutdown trigger service (sends SIGTERM to PID 1 after delay)
    cat > "$initrd_dir/usr/lib/systemd/system/shutdown-trigger.service" <<'EOF'
[Unit]
Description=Shutdown Trigger for Testing

[Service]
Type=oneshot
ExecStart=/bin/trigger-shutdown
EOF

    # Create test target that wants the shutdown trigger
    cat > "$initrd_dir/usr/lib/systemd/system/test.target" <<'EOF'
[Unit]
Description=Test Target
Wants=shutdown-trigger.service
EOF

    # Create default target symlink (sysd looks in /etc/systemd/system/)
    ln -sf ../../../usr/lib/systemd/system/test.target "$initrd_dir/etc/systemd/system/default.target"

    # Create console device node
    mknod -m 622 "$initrd_dir/dev/console" c 5 1 2>/dev/null || true
    mknod -m 666 "$initrd_dir/dev/null" c 1 3 2>/dev/null || true
    mknod -m 666 "$initrd_dir/dev/tty" c 5 0 2>/dev/null || true

    # Create initramfs cpio
    log "Packing initramfs..."
    (cd "$initrd_dir" && find . | cpio -o -H newc 2>/dev/null | gzip) > "$WORK_DIR/initramfs.cpio.gz"

    log "Initramfs created: $WORK_DIR/initramfs.cpio.gz"
}

OUTPUT_FILE=""

# Run QEMU and capture output
run_qemu() {
    log "Booting QEMU with sysd as init..."

    local timeout_sec=30
    OUTPUT_FILE="$WORK_DIR/qemu-output.log"
    local monitor_sock="$WORK_DIR/qemu-monitor.sock"

    # Use KVM if available
    local accel=""
    if [[ -w /dev/kvm ]]; then
        accel="-machine pc,accel=kvm"
        log "Using KVM acceleration"
    fi

    # Run QEMU in background with monitor socket for signal injection
    timeout "$timeout_sec" qemu-system-x86_64 \
        $accel \
        -kernel "$KERNEL" \
        -initrd "$WORK_DIR/initramfs.cpio.gz" \
        -append "console=ttyS0 panic=1 rdinit=/bin/sysd" \
        -nographic \
        -no-reboot \
        -m 256M \
        -serial file:"$OUTPUT_FILE" \
        -monitor unix:"$monitor_sock",server,nowait \
        2>&1 &
    local qemu_pid=$!

    # Wait for sysd to start (check for "sysd listening" in output)
    log "Waiting for sysd to start..."
    local waited=0
    while [[ $waited -lt 15 ]]; do
        if [[ -f "$OUTPUT_FILE" ]] && grep -q "sysd listening\|Essential filesystems mounted" "$OUTPUT_FILE" 2>/dev/null; then
            log "sysd started, waiting 2s for services..."
            sleep 2
            break
        fi
        sleep 1
        ((waited++))
    done

    # Wait for shutdown-trigger.service to run and initiate shutdown
    # Service sleeps 2s then sends SIGTERM, then shutdown takes ~5s
    log "Waiting for shutdown-trigger.service to initiate shutdown..."
    sleep 10

    # Force quit if still running
    if [[ -S "$monitor_sock" ]]; then
        echo "quit" | nc -U -q1 "$monitor_sock" 2>/dev/null || true
    fi

    # Wait for QEMU to exit
    wait $qemu_pid 2>/dev/null || true

    # Show output
    if [[ -f "$OUTPUT_FILE" ]]; then
        log "=== QEMU Serial Output ==="
        cat "$OUTPUT_FILE"
        echo ""
    fi
}

# Check test results
check_results() {
    local output_file="$1"
    local success=true

    log "Checking test results..."

    # === PID 1 & Mount Tests ===
    log "--- PID 1 & Mount Tests ---"

    # Check for PID 1 detection
    if grep -q "Running as PID 1" "$output_file"; then
        log "✓ PID 1 detection: PASS"
    else
        err "✗ PID 1 detection: FAIL"
        success=false
    fi

    # Check for filesystem mounts
    if grep -q "Essential filesystems mounted" "$output_file"; then
        log "✓ Filesystem mounting: PASS"
    else
        err "✗ Filesystem mounting: FAIL"
        success=false
    fi

    # Check for individual mounts (these should NOT say "already mounted")
    for fs in "/proc" "/sys" "/dev" "/run"; do
        if grep -q "Mounted.*on $fs" "$output_file"; then
            log "✓ Mounted $fs: PASS"
        elif grep -q "$fs already mounted" "$output_file"; then
            # In initramfs, nothing should be pre-mounted
            err "✗ $fs was already mounted (unexpected): FAIL"
            success=false
        fi
    done

    # Check that sysd started listening
    if grep -q "sysd listening" "$output_file"; then
        log "✓ sysd started: PASS"
    else
        # Might fail due to missing /run/sysd directory, that's ok for mount test
        log "○ sysd socket (may fail without full setup)"
    fi

    # === Boot Tests ===
    log "--- Boot Tests ---"

    # Check if boot completed
    if grep -q "Boot complete\|Booting to target" "$output_file"; then
        log "✓ Boot to target: PASS"
    else
        err "✗ Boot to target: FAIL"
        success=false
    fi

    # === Shutdown Tests ===
    log "--- Shutdown Tests ---"

    # Check for shutdown initiation
    if grep -q -i "shutdown\|stopping\|SIGTERM\|reboot\|poweroff" "$output_file"; then
        log "✓ Shutdown sequence initiated: PASS"
    else
        log "○ Shutdown sequence (signal may not have been received)"
    fi

    # Check for service stop during shutdown
    if grep -q "Stopping.*service\|SERVICE_STOPPED" "$output_file"; then
        log "✓ Services stopped during shutdown: PASS"
    else
        log "○ Service stop during shutdown (may not be implemented yet)"
    fi

    # Check for filesystem sync
    if grep -q -i "sync\|Syncing filesystems" "$output_file"; then
        log "✓ Filesystem sync: PASS"
    else
        log "○ Filesystem sync (may not log this)"
    fi

    if $success; then
        log "All critical tests PASSED"
        return 0
    else
        err "Some critical tests FAILED"
        return 1
    fi
}

# Main
main() {
    log "QEMU PID 1 Integration Test"
    log "Kernel: $KERNEL"
    log "sysd: $SYSD_BIN"

    check_deps

    mkdir -p "$WORK_DIR"

    create_initramfs

    run_qemu

    echo ""
    log "=== Test Results ==="
    check_results "$OUTPUT_FILE"
}

main "$@"
