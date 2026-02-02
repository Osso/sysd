#!/bin/bash
# QEMU graphical test for sysd - boots full Arch Linux to niri
#
# Creates a standalone raw disk image with Arch Linux and boots with sysd as PID 1
# to test the full graphical stack (greetd -> niri)
#
# Requirements:
# - QEMU with virtio-gpu support
# - Host running Arch Linux
# - niri, greetd installed on host
# - KVM access (/dev/kvm)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
# Use musl target if cargo config specifies it, otherwise use default release
if [[ -f "${PROJECT_DIR}/.cargo/config.toml" ]] && grep -q 'target.*=.*musl' "${PROJECT_DIR}/.cargo/config.toml"; then
    TARGET_DIR="${PROJECT_DIR}/target/x86_64-unknown-linux-musl/release"
else
    TARGET_DIR="${PROJECT_DIR}/target/release"
fi
SYSD_BIN="${TARGET_DIR}/sysd"
SYSDCTL_BIN="${TARGET_DIR}/sysdctl"
EXECUTOR_BIN="${TARGET_DIR}/sysd-executor"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err() { echo -e "${RED}[-]${NC} $*" >&2; }

# Configuration
IMAGE_FILE="${IMAGE_FILE:-/tmp/sysd-arch-graphical.raw}"
IMAGE_SIZE="${IMAGE_SIZE:-8G}"
IMAGE_MOUNT="${IMAGE_MOUNT:-/tmp/sysd-arch-mount}"
KERNEL="${KERNEL:-/boot/vmlinuz-linux}"
INITRAMFS="${INITRAMFS:-/boot/initramfs-linux.img}"

# QEMU settings
QEMU_MEM="${QEMU_MEM:-4G}"
QEMU_CPUS="${QEMU_CPUS:-4}"
QEMU_DISPLAY="${QEMU_DISPLAY:-sdl}"  # sdl, gtk, vnc:0, or none

cleanup() {
    local exit_code=$?
    log "Cleaning up..."

    # Unmount if mounted
    if mountpoint -q "$IMAGE_MOUNT" 2>/dev/null; then
        sudo umount "$IMAGE_MOUNT" 2>/dev/null || true
    fi

    # Detach loop device if attached
    if [[ -n "${LOOP_DEV:-}" && -b "$LOOP_DEV" ]]; then
        sudo losetup -d "$LOOP_DEV" 2>/dev/null || true
    fi

    exit $exit_code
}

trap cleanup EXIT

check_deps() {
    log "Checking dependencies..."

    local missing=()

    command -v qemu-system-x86_64 &>/dev/null || missing+=("qemu")
    command -v mkfs.btrfs &>/dev/null || missing+=("btrfs-progs")
    command -v pacstrap &>/dev/null || missing+=("arch-install-scripts")

    if [[ ! -f "$SYSD_BIN" ]]; then
        err "sysd binary not found. Run: cargo build --release"
        exit 1
    fi

    if [[ ! -f "$EXECUTOR_BIN" ]]; then
        err "sysd-executor binary not found. Run: cargo build --release"
        exit 1
    fi

    if [[ ! -w /dev/kvm ]]; then
        warn "KVM not available - QEMU will be slow"
    fi

    if [[ ${#missing[@]} -gt 0 ]]; then
        err "Missing dependencies: ${missing[*]}"
        exit 1
    fi

    # Check display availability
    if [[ "$QEMU_DISPLAY" != "none" && "$QEMU_DISPLAY" != vnc:* ]]; then
        local available_displays
        available_displays=$(qemu-system-x86_64 -display help 2>&1 | grep -v "^Available\|^Some\|^-display\|^For")
        if ! echo "$available_displays" | grep -q "$QEMU_DISPLAY"; then
            err "Display '$QEMU_DISPLAY' not available. Available: $available_displays"
            err "Install qemu-desktop for GTK/SDL support, or use QEMU_DISPLAY=vnc:0"
            exit 1
        fi
    fi
}

create_image() {
    if [[ -f "$IMAGE_FILE" ]]; then
        log "Image already exists: $IMAGE_FILE"
        log "Use 'rebuild' command to recreate it"
        return 0
    fi

    log "Creating raw disk image: $IMAGE_FILE ($IMAGE_SIZE)"

    # Create sparse raw image
    truncate -s "$IMAGE_SIZE" "$IMAGE_FILE"

    # Set up loop device
    LOOP_DEV=$(sudo losetup --find --show "$IMAGE_FILE")
    log "Loop device: $LOOP_DEV"

    # Create btrfs filesystem
    log "Creating btrfs filesystem..."
    sudo mkfs.btrfs -f "$LOOP_DEV"

    # Mount it
    sudo mkdir -p "$IMAGE_MOUNT"
    sudo mount "$LOOP_DEV" "$IMAGE_MOUNT"

    # Install packages
    install_packages

    # Inject sysd
    inject_sysd

    # Unmount
    sudo umount "$IMAGE_MOUNT"
    sudo losetup -d "$LOOP_DEV"
    unset LOOP_DEV

    log "Image created: $IMAGE_FILE"
}

install_packages() {
    log "Installing minimal Arch system with pacstrap..."

    # Minimal packages for graphical boot
    local packages=(
        # Base system
        base
        linux
        # D-Bus (required for logind/greetd)
        dbus-broker
        dbus-broker-units
        # Seat management (required for niri without systemd PID 1)
        seatd
        # Login manager
        greetd
        # Wayland compositor
        niri
        # Terminal emulator
        foot
        # Graphics (virtio in QEMU)
        mesa
        # Fonts (needed for niri)
        ttf-dejavu
        # Network/SSH for debugging
        openssh
        iproute2
    )

    log "Packages: ${packages[*]}"

    # Use pacstrap to install (uses host's package cache)
    sudo pacstrap -c "$IMAGE_MOUNT" "${packages[@]}"

    # Configure fstab
    log "Configuring fstab..."
    echo "# Minimal fstab for QEMU test" | sudo tee "$IMAGE_MOUNT/etc/fstab" > /dev/null
    echo "/dev/vda  /  btrfs  defaults  0 1" | sudo tee -a "$IMAGE_MOUNT/etc/fstab" > /dev/null

    # Configure greetd to auto-start niri via user sysd
    log "Configuring greetd..."
    sudo mkdir -p "$IMAGE_MOUNT/etc/greetd"
    cat <<'EOF' | sudo tee "$IMAGE_MOUNT/etc/greetd/config.toml" > /dev/null
[terminal]
vt = 1

[default_session]
# Use niri-sysd wrapper which starts sysd --user and then niri.service
command = "niri-sysd"
user = "testuser"
EOF

    # Create niri config for testuser to spawn terminal on startup
    sudo mkdir -p "$IMAGE_MOUNT/home/testuser/.config/niri"
    cat <<'NIRIEOF' | sudo tee "$IMAGE_MOUNT/home/testuser/.config/niri/config.kdl" > /dev/null
spawn-at-startup "foot"

binds {
    Mod+Return { spawn "foot"; }
    Mod+Q { close-window; }
    Mod+Shift+E { quit; }
}
NIRIEOF
    sudo chown -R 1000:1000 "$IMAGE_MOUNT/home/testuser/.config"

    # Also create root niri config for fallback
    sudo mkdir -p "$IMAGE_MOUNT/root/.config/niri"
    cat <<'NIRIEOF' | sudo tee "$IMAGE_MOUNT/root/.config/niri/config.kdl" > /dev/null
spawn-at-startup "foot"

binds {
    Mod+Return { spawn "foot"; }
    Mod+Q { close-window; }
    Mod+Shift+E { quit; }
}
NIRIEOF

    # Create simplified seatd service (avoids complex sandboxing sysd may not support)
    cat <<'SEATEOF' | sudo tee "$IMAGE_MOUNT/etc/systemd/system/seatd.service" > /dev/null
[Unit]
Description=Seat management daemon
Before=greetd.service

[Service]
Type=simple
ExecStart=/usr/bin/seatd -g seat
Restart=always
RestartSec=1

[Install]
WantedBy=multi-user.target
SEATEOF

    # Create seat group and add root and testuser
    sudo arch-chroot "$IMAGE_MOUNT" groupadd -f seat
    sudo arch-chroot "$IMAGE_MOUNT" usermod -aG seat,video root
    sudo arch-chroot "$IMAGE_MOUNT" usermod -aG seat,video testuser

    # Enable greetd and seatd to start on boot
    sudo mkdir -p "$IMAGE_MOUNT/etc/systemd/system/graphical.target.wants"
    sudo mkdir -p "$IMAGE_MOUNT/etc/systemd/system/multi-user.target.wants"
    sudo ln -sf /usr/lib/systemd/system/greetd.service "$IMAGE_MOUNT/etc/systemd/system/graphical.target.wants/greetd.service"
    sudo ln -sf /etc/systemd/system/seatd.service "$IMAGE_MOUNT/etc/systemd/system/multi-user.target.wants/seatd.service"

    # Set default target to graphical
    sudo ln -sf /usr/lib/systemd/system/graphical.target "$IMAGE_MOUNT/etc/systemd/system/default.target"

    # Disable getty@tty1 (conflicts with greetd on VT1)
    sudo rm -f "$IMAGE_MOUNT/etc/systemd/system/getty.target.wants/getty@tty1.service"

    # Set hostname
    echo "sysd-test" | sudo tee "$IMAGE_MOUNT/etc/hostname" > /dev/null

    # Create a test user for login
    sudo arch-chroot "$IMAGE_MOUNT" useradd -m -s /bin/bash testuser 2>/dev/null || true
    echo "testuser:test" | sudo arch-chroot "$IMAGE_MOUNT" chpasswd

    # Set root password for serial console
    echo "root:root" | sudo arch-chroot "$IMAGE_MOUNT" chpasswd

    # Create greetd-greeter PAM config (greetd uses this for greeter sessions)
    cat <<'PAMEOF' | sudo tee "$IMAGE_MOUNT/etc/pam.d/greetd-greeter" > /dev/null
#%PAM-1.0
auth       requisite    pam_nologin.so
auth       include      system-local-login
account    include      system-local-login
session    include      system-local-login
PAMEOF

    # Configure DNS
    echo "nameserver 8.8.8.8" | sudo tee "$IMAGE_MOUNT/etc/resolv.conf" > /dev/null

    # Create network setup service (QEMU user-mode networking, auto-detect interface)
    cat <<'NETEOF' | sudo tee "$IMAGE_MOUNT/etc/systemd/system/network-setup.service" > /dev/null
[Unit]
Description=Setup QEMU network

[Service]
Type=oneshot
ExecStart=/bin/sh -c 'sleep 2; IFACE=$(ls /sys/class/net | grep -v lo | head -1); ip link set $IFACE up; ip addr add 10.0.2.15/24 dev $IFACE; ip route add default via 10.0.2.2; echo nameserver 8.8.8.8 > /etc/resolv.conf'
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
NETEOF
    sudo ln -sf /etc/systemd/system/network-setup.service "$IMAGE_MOUNT/etc/systemd/system/multi-user.target.wants/network-setup.service"

    # Enable sshd
    sudo ln -sf /usr/lib/systemd/system/sshd.service "$IMAGE_MOUNT/etc/systemd/system/multi-user.target.wants/sshd.service"

    # Configure sshd to allow root login and disable DNS lookup
    sudo sed -i 's/^#PermitRootLogin.*/PermitRootLogin yes/' "$IMAGE_MOUNT/etc/ssh/sshd_config" 2>/dev/null || \
        echo "PermitRootLogin yes" | sudo tee -a "$IMAGE_MOUNT/etc/ssh/sshd_config" > /dev/null
    echo "UseDNS no" | sudo tee -a "$IMAGE_MOUNT/etc/ssh/sshd_config" > /dev/null

    # Simplify sshd PAM to avoid pam_systemd blocking
    cat <<'SSHPAM' | sudo tee "$IMAGE_MOUNT/etc/pam.d/sshd" > /dev/null
#%PAM-1.0
auth      required  pam_unix.so
account   required  pam_unix.so
password  required  pam_unix.so
session   required  pam_unix.so
SSHPAM

    # Add SSH public key for passwordless access
    sudo mkdir -p "$IMAGE_MOUNT/root/.ssh"
    sudo chmod 700 "$IMAGE_MOUNT/root/.ssh"
    if [[ -f "$HOME/.ssh/id_ed25519.pub" ]]; then
        sudo cp "$HOME/.ssh/id_ed25519.pub" "$IMAGE_MOUNT/root/.ssh/authorized_keys"
        sudo chmod 600 "$IMAGE_MOUNT/root/.ssh/authorized_keys"
    fi

    log "System installed"
}

inject_sysd() {
    log "Injecting sysd binaries..."

    # Copy sysd binaries
    sudo cp "$SYSD_BIN" "$IMAGE_MOUNT/usr/bin/sysd"
    sudo cp "$SYSDCTL_BIN" "$IMAGE_MOUNT/usr/bin/sysdctl"
    sudo cp "$EXECUTOR_BIN" "$IMAGE_MOUNT/usr/bin/sysd-executor"
    # Copy systemctl compat wrapper (allows niri-session to work unmodified)
    sudo cp "${TARGET_DIR}/systemctl" "$IMAGE_MOUNT/usr/bin/systemctl"
    sudo chmod +x "$IMAGE_MOUNT/usr/bin/sysd" "$IMAGE_MOUNT/usr/bin/sysdctl" "$IMAGE_MOUNT/usr/bin/sysd-executor" "$IMAGE_MOUNT/usr/bin/systemctl"

    # Create /sbin/init symlink (initramfs switch_root expects this)
    sudo ln -sf /usr/bin/sysd "$IMAGE_MOUNT/sbin/init"

    # Copy session wrapper scripts
    sudo mkdir -p "$IMAGE_MOUNT/usr/local/bin"
    # Create niri-sysd wrapper that starts sysd --user and then runs real niri-session
    cat <<'NIRISYSD' | sudo tee "$IMAGE_MOUNT/usr/local/bin/niri-sysd" > /dev/null
#!/bin/sh
# niri-sysd - Start sysd --user and launch niri-session
# This wrapper ensures sysd --user is running before niri-session starts.
# niri-session will then use the 'systemctl' compat wrapper which calls sysdctl.
set -e

# Ensure HOME is set
[ -z "$HOME" ] && export HOME=$(getent passwd "$(id -u)" | cut -d: -f6)

# Ensure XDG variables are set
export XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
export XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
export XDG_STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

# Ensure runtime dir exists with proper permissions
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

# Start sysd --user if not running
if ! sysdctl --user ping >/dev/null 2>&1; then
    echo "Starting sysd --user..."
    sysd --user &
    for i in $(seq 1 50); do
        sysdctl --user ping >/dev/null 2>&1 && break
        sleep 0.1
    done
    if ! sysdctl --user ping >/dev/null 2>&1; then
        echo "Failed to start sysd --user"
        exit 1
    fi
fi

# Now run the real niri-session which will use systemctl (our compat wrapper)
exec niri-session "$@"
NIRISYSD
    sudo chmod +x "$IMAGE_MOUNT/usr/local/bin/niri-sysd"

    if [[ -f /usr/local/bin/hyprland-sysd ]]; then
        sudo cp /usr/local/bin/hyprland-sysd "$IMAGE_MOUNT/usr/local/bin/hyprland-sysd"
        sudo chmod +x "$IMAGE_MOUNT/usr/local/bin/hyprland-sysd"
    fi

    log "Binaries injected"
}

update_sysd() {
    log "Updating sysd binaries in existing image..."

    if [[ ! -f "$IMAGE_FILE" ]]; then
        err "Image not found: $IMAGE_FILE"
        err "Run without arguments to create it first"
        exit 1
    fi

    # Set up loop device
    LOOP_DEV=$(sudo losetup --find --show "$IMAGE_FILE")
    log "Loop device: $LOOP_DEV"

    # Mount
    sudo mkdir -p "$IMAGE_MOUNT"
    sudo mount "$LOOP_DEV" "$IMAGE_MOUNT"

    # Update binaries
    inject_sysd

    # Unmount
    sudo umount "$IMAGE_MOUNT"
    sudo losetup -d "$LOOP_DEV"
    unset LOOP_DEV

    log "Binaries updated"
}

run_qemu() {
    if [[ ! -f "$IMAGE_FILE" ]]; then
        err "Image not found: $IMAGE_FILE"
        err "Run without arguments to create it first"
        exit 1
    fi

    log "Starting QEMU..."
    log "  Image: $IMAGE_FILE"
    log "  Memory: $QEMU_MEM"
    log "  CPUs: $QEMU_CPUS"
    log "  Display: $QEMU_DISPLAY"
    log ""
    log "The VM will boot to greetd. Select niri-sysd session."
    log "Press Ctrl+Alt+G to release mouse grab (GTK/SDL display)"
    log ""

    # Build display args
    local display_args=""
    case "$QEMU_DISPLAY" in
        none)
            display_args="-nographic"
            ;;
        vnc:*)
            display_args="-display none -vnc :${QEMU_DISPLAY#vnc:}"
            ;;
        *)
            display_args="-display $QEMU_DISPLAY,gl=on"
            ;;
    esac

    # KVM acceleration
    local accel_args=""
    if [[ -w /dev/kvm ]]; then
        accel_args="-enable-kvm -cpu host"
    fi

    # Run QEMU with the raw image
    sudo -E \
        DISPLAY="${DISPLAY:-}" \
        WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-}" \
        XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-}" \
        XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}" \
        qemu-system-x86_64 \
        $accel_args \
        -m "$QEMU_MEM" \
        -smp "$QEMU_CPUS" \
        -drive file="$IMAGE_FILE",format=raw,if=virtio \
        -kernel "$KERNEL" \
        -initrd "$INITRAMFS" \
        -append "root=/dev/vda rw init=/usr/bin/sysd console=tty0 console=ttyS0 loglevel=4" \
        -device virtio-vga-gl \
        -nic user,hostfwd=tcp::2222-:22 \
        -usb \
        -device usb-tablet \
        -serial stdio \
        $display_args \
        -no-reboot
}

delete_image() {
    if [[ -f "$IMAGE_FILE" ]]; then
        log "Deleting image: $IMAGE_FILE"
        rm -f "$IMAGE_FILE"
        log "Image deleted"
    else
        warn "Image not found: $IMAGE_FILE"
    fi
}

usage() {
    cat <<EOF
Usage: $0 [command]

Commands:
    run         Run QEMU with existing image (default if image exists)
    create      Create the disk image (default if image doesn't exist)
    rebuild     Delete and recreate the disk image
    update      Update sysd binaries in existing image
    cleanup     Delete the disk image
    help        Show this help

Environment variables:
    IMAGE_FILE      Path to disk image (default: /tmp/sysd-arch-graphical.raw)
    IMAGE_SIZE      Size of disk image (default: 20G)
    QEMU_MEM        VM memory (default: 4G)
    QEMU_CPUS       VM CPUs (default: 4)
    QEMU_DISPLAY    Display type: sdl, gtk, vnc:0, none (default: sdl)
    KERNEL          Kernel path (default: /boot/vmlinuz-linux)
    INITRAMFS       Initramfs path (default: /boot/initramfs-linux.img)

Example:
    # First run - creates image and boots
    ./run-qemu-graphical.sh

    # Subsequent runs - just boots existing image
    ./run-qemu-graphical.sh

    # After code changes - update binaries and run
    ./run-qemu-graphical.sh update && ./run-qemu-graphical.sh run

    # Full rebuild
    ./run-qemu-graphical.sh rebuild

    # Run with VNC display
    QEMU_DISPLAY=vnc:0 ./run-qemu-graphical.sh
EOF
}

main() {
    local cmd="${1:-auto}"

    case "$cmd" in
        auto)
            check_deps
            if [[ -f "$IMAGE_FILE" ]]; then
                run_qemu
            else
                create_image
                run_qemu
            fi
            ;;
        run)
            check_deps
            run_qemu
            ;;
        create)
            check_deps
            create_image
            ;;
        rebuild)
            check_deps
            delete_image
            create_image
            run_qemu
            ;;
        update)
            check_deps
            update_sysd
            ;;
        cleanup)
            delete_image
            ;;
        help|--help|-h)
            usage
            ;;
        *)
            err "Unknown command: $cmd"
            usage
            exit 1
            ;;
    esac
}

main "$@"
