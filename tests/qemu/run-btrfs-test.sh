#!/bin/bash
# QEMU-based test for sysd with btrfs root filesystem
#
# This tests:
# - Root filesystem (btrfs with subvolumes) already mounted by initramfs
# - fstab-generated mount units detecting already-mounted filesystems
# - Additional subvolume mounts from fstab
#
# Requirements:
# - qemu-system-x86_64
# - Linux kernel with btrfs support
# - mkfs.btrfs
# - Built sysd binary

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORK_DIR="${PROJECT_DIR}/target/qemu-btrfs-test"
SYSD_BIN="${PROJECT_DIR}/target/x86_64-unknown-linux-musl/release/sysd"
DISK_IMG="${WORK_DIR}/root.img"
DISK_SIZE="256M"

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
    local missing=()

    command -v qemu-system-x86_64 &>/dev/null || missing+=("qemu-system-x86_64")
    command -v mkfs.btrfs &>/dev/null || missing+=("mkfs.btrfs (btrfs-progs)")

    if [[ ! -f "$SYSD_BIN" ]]; then
        err "sysd binary not found at $SYSD_BIN"
        err "Run: cargo build --release"
        exit 1
    fi

    if [[ ! -f "$KERNEL" ]]; then
        err "Kernel not found at $KERNEL"
        exit 1
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        err "Missing dependencies: ${missing[*]}"
        exit 1
    fi
}

# Create btrfs disk image with subvolumes
create_disk_image() {
    log "Creating btrfs disk image..."

    rm -rf "$WORK_DIR"
    mkdir -p "$WORK_DIR"

    # Create sparse disk image
    truncate -s "$DISK_SIZE" "$DISK_IMG"

    # Format as btrfs
    mkfs.btrfs -q "$DISK_IMG"

    # Mount and create subvolumes
    local mnt="$WORK_DIR/mnt"
    mkdir -p "$mnt"

    sudo mount -o loop "$DISK_IMG" "$mnt"

    # Create subvolumes like Arch Linux setup
    sudo btrfs subvolume create "$mnt/@root"
    sudo btrfs subvolume create "$mnt/@home"

    # Create directory structure in @root
    sudo mkdir -p "$mnt/@root"/{bin,dev,proc,sys,run,tmp,etc,home,usr/lib/systemd/system}
    sudo mkdir -p "$mnt/@root/etc/systemd/system"

    # Copy sysd
    sudo cp "$SYSD_BIN" "$mnt/@root/bin/sysd"
    sudo chmod +x "$mnt/@root/bin/sysd"

    # Copy busybox for utilities
    local busybox_bin=""
    if command -v busybox &>/dev/null; then
        busybox_bin="$(command -v busybox)"
    elif [[ -f /usr/lib/initcpio/busybox ]]; then
        busybox_bin="/usr/lib/initcpio/busybox"
    fi

    if [[ -n "$busybox_bin" ]]; then
        sudo cp "$busybox_bin" "$mnt/@root/bin/busybox"
        for cmd in sh cat ls mount umount ps kill sleep grep; do
            sudo ln -sf busybox "$mnt/@root/bin/$cmd"
        done
    fi

    # Create /etc/fstab with btrfs subvolumes (like user's real system)
    # Use /dev/vda since that's what QEMU virtio uses
    sudo tee "$mnt/@root/etc/fstab" > /dev/null <<'EOF'
# Root filesystem (should already be mounted by initramfs)
/dev/vda    /        btrfs   subvol=@root,compress=zstd,noatime  0 0

# Home subvolume
/dev/vda    /home    btrfs   subvol=@home,compress=zstd,noatime  0 0

# tmpfs
tmpfs       /tmp     tmpfs   defaults,noatime,mode=1777          0 0
EOF

    # Create /etc/passwd and /etc/group
    echo "root:x:0:0:root:/root:/bin/sh" | sudo tee "$mnt/@root/etc/passwd" > /dev/null
    echo "root:x:0:" | sudo tee "$mnt/@root/etc/group" > /dev/null

    # Create local-fs.target to pull in mount units
    sudo tee "$mnt/@root/usr/lib/systemd/system/local-fs.target" > /dev/null <<'EOF'
[Unit]
Description=Local File Systems
DefaultDependencies=no
EOF

    # Create test.target that requires local-fs.target
    sudo tee "$mnt/@root/usr/lib/systemd/system/test.target" > /dev/null <<'EOF'
[Unit]
Description=Test Target
Requires=local-fs.target
After=local-fs.target
EOF

    # Create default target symlink
    sudo ln -sf ../../../usr/lib/systemd/system/test.target "$mnt/@root/etc/systemd/system/default.target"

    # Create static mount units (normally generated from fstab)
    sudo tee "$mnt/@root/usr/lib/systemd/system/-.mount" > /dev/null <<'EOF'
[Unit]
Description=Root Filesystem
DefaultDependencies=no
Before=local-fs.target

[Mount]
What=/dev/vda
Where=/
Type=btrfs
Options=subvol=@root,compress=zstd,noatime
EOF

    sudo tee "$mnt/@root/usr/lib/systemd/system/home.mount" > /dev/null <<'EOF'
[Unit]
Description=Home Directory
DefaultDependencies=no
Before=local-fs.target

[Mount]
What=/dev/vda
Where=/home
Type=btrfs
Options=subvol=@home,compress=zstd,noatime
EOF

    sudo tee "$mnt/@root/usr/lib/systemd/system/tmp.mount" > /dev/null <<'EOF'
[Unit]
Description=Temporary Directory
DefaultDependencies=no
Before=local-fs.target

[Mount]
What=tmpfs
Where=/tmp
Type=tmpfs
Options=defaults,noatime,mode=1777
EOF

    # Make local-fs.target want the mount units
    sudo mkdir -p "$mnt/@root/etc/systemd/system/local-fs.target.wants"
    sudo ln -sf /usr/lib/systemd/system/-.mount "$mnt/@root/etc/systemd/system/local-fs.target.wants/"
    sudo ln -sf /usr/lib/systemd/system/home.mount "$mnt/@root/etc/systemd/system/local-fs.target.wants/"
    sudo ln -sf /usr/lib/systemd/system/tmp.mount "$mnt/@root/etc/systemd/system/local-fs.target.wants/"

    # Create a test script that runs after boot and prints mount info
    sudo tee "$mnt/@root/bin/boot-test" > /dev/null <<'SCRIPT'
#!/bin/sh
echo "=== BOOT TEST ==="
echo "PID: $$"
echo ""
echo "=== /proc/mounts ==="
cat /proc/mounts
echo ""
echo "=== Mount points ==="
mount
echo ""
echo "=== Test complete ==="
# Keep system alive briefly for log capture
sleep 3
# Signal init to shutdown
kill -TERM 1
SCRIPT
    sudo chmod +x "$mnt/@root/bin/boot-test"

    # Create a service to run the test
    sudo tee "$mnt/@root/usr/lib/systemd/system/boot-test.service" > /dev/null <<'EOF'
[Unit]
Description=Boot Test Service
After=local-fs.target

[Service]
Type=oneshot
ExecStart=/bin/boot-test
StandardOutput=tty
StandardError=tty
EOF

    # Make test.target want the test service
    sudo mkdir -p "$mnt/@root/etc/systemd/system/test.target.wants"
    sudo ln -sf ../../../usr/lib/systemd/system/boot-test.service "$mnt/@root/etc/systemd/system/test.target.wants/"

    sudo umount "$mnt"

    log "Disk image created: $DISK_IMG"
}

# Create initramfs that mounts btrfs root
create_initramfs() {
    log "Creating initramfs with btrfs support..."

    local initrd_dir="$WORK_DIR/initrd"
    rm -rf "$initrd_dir"
    mkdir -p "$initrd_dir"/{bin,dev,proc,sys,mnt/root,lib/modules}

    # Copy busybox
    local busybox_bin=""
    if command -v busybox &>/dev/null; then
        busybox_bin="$(command -v busybox)"
    elif [[ -f /usr/lib/initcpio/busybox ]]; then
        busybox_bin="/usr/lib/initcpio/busybox"
    fi

    if [[ -z "$busybox_bin" ]]; then
        err "busybox not found"
        exit 1
    fi

    cp "$busybox_bin" "$initrd_dir/bin/busybox"
    for cmd in sh cat ls mount umount switch_root sleep echo mkdir mknod; do
        ln -sf busybox "$initrd_dir/bin/$cmd"
    done

    # Create device nodes
    mknod -m 622 "$initrd_dir/dev/console" c 5 1 2>/dev/null || true
    mknod -m 666 "$initrd_dir/dev/null" c 1 3 2>/dev/null || true
    mknod -m 666 "$initrd_dir/dev/tty" c 5 0 2>/dev/null || true

    # Create init script that mounts btrfs root and switches to it
    cat > "$initrd_dir/init" <<'INIT'
#!/bin/sh
echo "initramfs: starting..."

# Mount essential filesystems
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev

echo "initramfs: mounting btrfs root..."

# Wait for disk to appear
sleep 1

# Mount btrfs root subvolume
mount -t btrfs -o subvol=@root /dev/vda /mnt/root

if [ $? -ne 0 ]; then
    echo "initramfs: FAILED to mount root!"
    echo "Available devices:"
    ls -la /dev/vda* 2>/dev/null || echo "No vda devices"
    exec /bin/sh
fi

echo "initramfs: root mounted, switching..."

# Clean up
umount /proc
umount /sys

# Switch to real root
exec switch_root /mnt/root /bin/sysd
INIT
    chmod +x "$initrd_dir/init"

    # Pack initramfs
    (cd "$initrd_dir" && find . | cpio -o -H newc 2>/dev/null | gzip) > "$WORK_DIR/initramfs.cpio.gz"

    log "Initramfs created"
}

OUTPUT_FILE=""

# Run QEMU
run_qemu() {
    log "Booting QEMU with btrfs root..."

    OUTPUT_FILE="$WORK_DIR/qemu-output.log"
    local timeout_sec=45

    # Use KVM if available
    local accel=""
    if [[ -w /dev/kvm ]]; then
        accel="-machine pc,accel=kvm"
        log "Using KVM acceleration"
    fi

    # Run QEMU with virtio disk
    # -nographic redirects serial to stdio, so don't use -serial stdio
    timeout "$timeout_sec" qemu-system-x86_64 \
        $accel \
        -kernel "$KERNEL" \
        -initrd "$WORK_DIR/initramfs.cpio.gz" \
        -drive file="$DISK_IMG",format=raw,if=virtio \
        -append "console=ttyS0 panic=1 root=/dev/vda rootflags=subvol=@root rootfstype=btrfs rw" \
        -nographic \
        -no-reboot \
        -m 512M \
        2>&1 | tee "$OUTPUT_FILE" || true

    log "QEMU finished"
}

# Check results
check_results() {
    log "Checking results..."

    local success=true

    # Check for sysd starting
    if grep -q "Running as PID 1\|sysd listening\|Essential filesystems mounted" "$OUTPUT_FILE"; then
        log "✓ sysd started as PID 1"
    else
        err "✗ sysd did not start properly"
        success=false
    fi

    # Check for root already mounted detection
    if grep -q "already mounted at /\|-.mount already mounted" "$OUTPUT_FILE"; then
        log "✓ Root filesystem correctly detected as already mounted"
    else
        err "✗ Root filesystem not detected as already mounted"
        # Check if it tried to mount
        if grep -q "NOT mounted, will mount at /" "$OUTPUT_FILE"; then
            err "  sysd tried to mount root again (should skip)"
        fi
        success=false
    fi

    # Check for /home mount
    if grep -q "Mounted.*home\|home.mount.*mounted\|/home" "$OUTPUT_FILE"; then
        log "✓ /home subvolume handling"
    else
        log "○ /home subvolume (may not be in boot plan)"
    fi

    # Check for /tmp mount
    if grep -q "Mounted.*tmp\|tmp.mount" "$OUTPUT_FILE"; then
        log "✓ /tmp tmpfs mounted"
    else
        log "○ /tmp tmpfs (may not be in boot plan)"
    fi

    # Check for mount failures
    if grep -q "MOUNT FAILED" "$OUTPUT_FILE"; then
        err "✗ Mount failures detected:"
        grep "MOUNT FAILED" "$OUTPUT_FILE" | head -5
        success=false
    else
        log "✓ No mount failures"
    fi

    # Check for boot completion
    if grep -q "Boot complete\|BOOT TEST" "$OUTPUT_FILE"; then
        log "✓ Boot completed"
    else
        err "✗ Boot did not complete"
        success=false
    fi

    if $success; then
        log "All tests PASSED"
        return 0
    else
        err "Some tests FAILED"
        return 1
    fi
}

# Cleanup
cleanup() {
    # Remove loop devices if any are still attached
    sudo losetup -D 2>/dev/null || true
}

trap cleanup EXIT

# Main
main() {
    log "QEMU btrfs Integration Test"
    log "Kernel: $KERNEL"
    log "sysd: $SYSD_BIN"

    check_deps
    create_disk_image
    create_initramfs
    run_qemu

    echo ""
    log "=== Output Log ==="
    cat "$OUTPUT_FILE"
    echo ""

    log "=== Test Results ==="
    check_results
}

main "$@"
