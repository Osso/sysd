#!/bin/bash
# QEMU-based test for sysd PID 1 mount functionality
#
# This boots a minimal Linux system with sysd as init to test
# that essential filesystems are actually mounted (not skipped).
#
# Requirements:
# - qemu-system-x86_64
# - Linux kernel (uses host's /boot/vmlinuz-linux or specify KERNEL=)
# - Built sysd binary

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORK_DIR="${PROJECT_DIR}/target/qemu-test"
SYSD_BIN="${PROJECT_DIR}/target/release/sysd"

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
    mkdir -p "$initrd_dir"/{bin,dev,proc,sys,run,tmp,etc,lib,lib64,usr/lib}

    # Copy sysd as init
    cp "$SYSD_BIN" "$initrd_dir/init"
    chmod +x "$initrd_dir/init"

    # Copy required shared libraries for sysd
    log "Copying shared libraries..."
    cp /usr/lib/libgcc_s.so.1 "$initrd_dir/usr/lib/"
    cp /usr/lib/libm.so.6 "$initrd_dir/usr/lib/"
    cp /usr/lib/libc.so.6 "$initrd_dir/usr/lib/"
    cp /usr/lib64/ld-linux-x86-64.so.2 "$initrd_dir/lib64/"
    # Create symlinks for library path resolution
    ln -sf ../usr/lib/libgcc_s.so.1 "$initrd_dir/lib/"
    ln -sf ../usr/lib/libm.so.6 "$initrd_dir/lib/"
    ln -sf ../usr/lib/libc.so.6 "$initrd_dir/lib/"

    # Copy busybox for utilities (if available)
    if command -v busybox &>/dev/null; then
        cp "$(command -v busybox)" "$initrd_dir/bin/busybox"
        # Create symlinks for common utilities
        for cmd in sh cat ls mount ps; do
            ln -sf busybox "$initrd_dir/bin/$cmd"
        done
    fi

    # Minimal /etc/passwd for User= directive
    echo "root:x:0:0:root:/:/bin/sh" > "$initrd_dir/etc/passwd"
    echo "root:x:0:" > "$initrd_dir/etc/group"

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

    # Use KVM if available
    local accel=""
    if [[ -w /dev/kvm ]]; then
        accel="-machine pc,accel=kvm"
        log "Using KVM acceleration"
    fi

    # Run QEMU with timeout, capture output to file
    timeout "$timeout_sec" qemu-system-x86_64 \
        $accel \
        -kernel "$KERNEL" \
        -initrd "$WORK_DIR/initramfs.cpio.gz" \
        -append "console=ttyS0 panic=1 init=/init" \
        -nographic \
        -no-reboot \
        -m 256M \
        -serial file:"$OUTPUT_FILE" \
        2>&1 || true

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

    if $success; then
        log "All mount tests PASSED"
        return 0
    else
        err "Some tests FAILED"
        return 1
    fi
}

# Main
main() {
    log "QEMU PID 1 Mount Test"
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
