#!/bin/bash
# Build and run Docker-based integration tests for sysd
#
# This tests sysd with real Arch Linux systemd units including:
# - nginx.service, redis.service, sshd.service
# - Various .target, .socket, .timer, .mount units
# - Boot plan generation
# - Service lifecycle (start/stop)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $*"; }
err() { echo -e "${RED}[-]${NC} $*" >&2; }

# Check dependencies
check_deps() {
    if ! command -v docker &>/dev/null; then
        err "docker not found"
        exit 1
    fi
}

# Build sysd binaries
build_binaries() {
    log "Building sysd binaries..."

    # Build for musl target (statically linked, works in any container)
    # The project .cargo/config.toml sets musl as default target
    cargo build --release --manifest-path "$PROJECT_DIR/Cargo.toml" 2>&1 | tail -5

    # Musl binaries are in target/x86_64-unknown-linux-musl/release/
    local target_dir="$PROJECT_DIR/target/x86_64-unknown-linux-musl/release"

    if [[ ! -f "$target_dir/sysd" ]]; then
        err "sysd binary not found at $target_dir/sysd"
        exit 1
    fi

    if [[ ! -f "$target_dir/sysdctl" ]]; then
        err "sysdctl binary not found at $target_dir/sysdctl"
        exit 1
    fi

    log "Binaries built successfully"
}

# Build Docker image
build_image() {
    log "Building Docker image..."

    local target_dir="$PROJECT_DIR/target/x86_64-unknown-linux-musl/release"

    # Copy binaries to docker context
    cp "$target_dir/sysd" "$SCRIPT_DIR/sysd"
    cp "$target_dir/sysdctl" "$SCRIPT_DIR/sysdctl"

    docker build -t sysd-test:latest "$SCRIPT_DIR"

    # Clean up copied binaries
    rm -f "$SCRIPT_DIR/sysd" "$SCRIPT_DIR/sysdctl"

    log "Docker image built"
}

# Run tests
run_tests() {
    log "Running Docker tests..."
    echo ""

    # Run with --privileged for cgroups access
    # Use --cgroupns=host to access host cgroups (needed for cgroup operations)
    docker run --rm \
        --privileged \
        --cgroupns=host \
        -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
        sysd-test:latest

    local exit_code=$?

    echo ""
    if [[ $exit_code -eq 0 ]]; then
        log "Docker tests completed successfully"
    else
        err "Docker tests failed (exit code: $exit_code)"
    fi

    return $exit_code
}

# Cleanup
cleanup() {
    rm -f "$SCRIPT_DIR/sysd" "$SCRIPT_DIR/sysdctl"
}

trap cleanup EXIT

main() {
    log "=== sysd Docker Integration Test ==="
    echo ""

    check_deps
    build_binaries
    build_image
    run_tests
}

main "$@"
