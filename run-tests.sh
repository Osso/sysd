#!/bin/bash
# Run all sysd tests
#
# Usage: ./run-tests.sh [options]
#   --unit     Run unit tests only (fast, no deps)
#   --docker   Run Docker integration tests
#   --qemu     Run QEMU integration tests
#   --btrfs    Run QEMU btrfs mount test
#   --all      Run all tests (default)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $*"; }
err() { echo -e "${RED}[-]${NC} $*" >&2; }

run_unit_tests() {
    log "Running unit tests..."
    cargo test --lib
}

run_docker_tests() {
    log "Running Docker integration tests..."
    # Use legacy builder to avoid buildx activity tracking issues
    DOCKER_BUILDKIT=0 tests/docker/run-docker-test.sh
}

run_qemu_tests() {
    log "Running QEMU integration tests..."
    tests/qemu/run-qemu-test.sh
}

run_btrfs_tests() {
    log "Running QEMU btrfs mount tests..."
    tests/qemu/run-btrfs-test.sh
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
else
    err "Unknown option: $1"
    echo "Usage: $0 [--unit|--docker|--qemu|--btrfs|--all]"
    exit 1
fi

log "Tests completed!"
