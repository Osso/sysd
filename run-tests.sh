#!/bin/bash
# Run all sysd tests
#
# Usage: ./run-tests.sh [options]
#   --unit         Run unit tests only (fast, no deps)
#   --docker       Run Docker integration tests
#   --qemu         Run QEMU integration tests
#   --btrfs        Run QEMU btrfs mount test
#   --arch         Run full Arch Linux boot test (QEMU, requires root)
#   --arch-docker  Run full Arch Linux boot test (Docker, no root needed)
#   --all          Run all tests (default, excludes --arch variants)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $*"; }
err() { echo -e "${RED}[-]${NC} $*" >&2; }

build_release() {
    log "Building release binary..."
    cargo build --release
}

run_unit_tests() {
    log "Running unit tests..."
    cargo test --lib
}

run_docker_tests() {
    log "Running Docker integration tests..."
    build_release
    # Use legacy builder to avoid buildx activity tracking issues
    DOCKER_BUILDKIT=0 tests/docker/run-docker-test.sh
}

run_qemu_tests() {
    log "Running QEMU integration tests..."
    build_release
    tests/qemu/run-qemu-test.sh
}

run_btrfs_tests() {
    log "Running QEMU btrfs mount tests..."
    build_release
    tests/qemu/run-btrfs-test.sh
}

run_arch_tests() {
    log "Running full Arch Linux boot tests (QEMU)..."
    build_release
    # This test requires root for pacstrap
    if [[ $EUID -ne 0 ]]; then
        err "Arch test requires root. Run with: sudo $0 --arch"
        exit 1
    fi
    tests/qemu/run-arch-test.sh
}

run_arch_docker_tests() {
    log "Running full Arch Linux boot tests (Docker)..."
    build_release
    tests/docker-arch/run-arch-docker-test.sh
}

# Parse args
if [[ $# -eq 0 ]] || [[ "$1" == "--all" ]]; then
    run_unit_tests
    echo ""
    run_docker_tests
    echo ""
    run_qemu_tests
    echo ""
    run_btrfs_tests
elif [[ "$1" == "--unit" ]]; then
    run_unit_tests
elif [[ "$1" == "--docker" ]]; then
    run_docker_tests
elif [[ "$1" == "--qemu" ]]; then
    run_qemu_tests
elif [[ "$1" == "--btrfs" ]]; then
    run_btrfs_tests
elif [[ "$1" == "--arch" ]]; then
    run_arch_tests
elif [[ "$1" == "--arch-docker" ]]; then
    run_arch_docker_tests
else
    err "Unknown option: $1"
    echo "Usage: $0 [--unit|--docker|--qemu|--btrfs|--arch|--arch-docker|--all]"
    exit 1
fi

log "Tests completed!"
