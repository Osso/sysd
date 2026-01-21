#!/bin/bash
# Build and run Docker-based full Arch Linux test with sysd as PID 1
#
# This tests sysd running as PID 1 in a full Arch Linux environment:
# - D-Bus socket activation
# - Service lifecycle (start/stop)
# - Target and dependency resolution
# - Cgroup integration
#
# Unlike the QEMU test, this doesn't require root or pacstrap, but
# sysd won't have true PID 1 kernel semantics (signal handling, etc.)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
CONTAINER_NAME="sysd-arch-test-$$"
IMAGE_NAME="sysd-arch-test:latest"
STARTUP_TIMEOUT=30

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[+]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
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
    cargo build --release --manifest-path "$PROJECT_DIR/Cargo.toml" 2>&1 | tail -5

    local target_dir="$PROJECT_DIR/target/release"
    if [[ ! -f "$target_dir/sysd" ]] || [[ ! -f "$target_dir/sysdctl" ]] || [[ ! -f "$target_dir/sysd-executor" ]]; then
        err "sysd/sysdctl/sysd-executor binaries not found"
        exit 1
    fi
    log "Binaries built successfully"
}

# Build Docker image
build_image() {
    log "Building Docker image..."

    local target_dir="$PROJECT_DIR/target/release"

    # Copy binaries to docker context
    cp "$target_dir/sysd" "$SCRIPT_DIR/sysd"
    cp "$target_dir/sysdctl" "$SCRIPT_DIR/sysdctl"
    cp "$target_dir/sysd-executor" "$SCRIPT_DIR/sysd-executor"

    DOCKER_BUILDKIT=0 docker build -t "$IMAGE_NAME" "$SCRIPT_DIR"

    # Clean up copied binaries
    rm -f "$SCRIPT_DIR/sysd" "$SCRIPT_DIR/sysdctl" "$SCRIPT_DIR/sysd-executor"

    log "Docker image built"
}

# Cleanup on exit
cleanup() {
    if docker ps -q -f "name=$CONTAINER_NAME" | grep -q .; then
        log "Stopping container..."
        docker stop "$CONTAINER_NAME" &>/dev/null || true
    fi
    if docker ps -aq -f "name=$CONTAINER_NAME" | grep -q .; then
        docker rm "$CONTAINER_NAME" &>/dev/null || true
    fi
    rm -f "$SCRIPT_DIR/sysd" "$SCRIPT_DIR/sysdctl" "$SCRIPT_DIR/sysd-executor"
}

trap cleanup EXIT

# Start container with sysd as PID 1
start_container() {
    log "Starting container with sysd as PID 1..."

    docker run -d \
        --name "$CONTAINER_NAME" \
        --privileged \
        --cgroupns=host \
        -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
        --tmpfs /run:exec \
        --tmpfs /tmp \
        "$IMAGE_NAME"

    log "Container started: $CONTAINER_NAME"
}

# Wait for sysd to be ready
wait_for_sysd() {
    log "Waiting for sysd to start (timeout: ${STARTUP_TIMEOUT}s)..."

    local waited=0
    while [[ $waited -lt $STARTUP_TIMEOUT ]]; do
        # Check if container is still running
        if ! docker ps -q -f "name=$CONTAINER_NAME" | grep -q .; then
            err "Container stopped unexpectedly"
            echo "--- Container logs ---"
            docker logs "$CONTAINER_NAME" 2>&1 | tail -50
            return 1
        fi

        # Check if sysd socket is ready
        if docker exec "$CONTAINER_NAME" test -S /run/sysd.sock 2>/dev/null; then
            log "sysd is ready!"
            return 0
        fi

        sleep 1
        waited=$((waited + 1))
        echo -ne "\r  Elapsed: ${waited}s / ${STARTUP_TIMEOUT}s"
    done

    echo ""
    err "sysd did not start within ${STARTUP_TIMEOUT}s"
    echo "--- Container logs ---"
    docker logs "$CONTAINER_NAME" 2>&1 | tail -50
    return 1
}

# Run tests inside container
run_tests() {
    log "Running tests..."
    echo ""

    docker exec "$CONTAINER_NAME" /test-arch.sh
    return $?
}

# Show container logs
show_logs() {
    log "Container logs:"
    docker logs "$CONTAINER_NAME" 2>&1 | tail -30
}

main() {
    log "=== sysd Docker Arch Full Boot Test ==="
    echo ""

    check_deps
    build_binaries
    build_image

    start_container

    if ! wait_for_sysd; then
        exit 1
    fi

    echo ""
    local test_result=0
    run_tests || test_result=$?

    if [[ $test_result -ne 0 ]]; then
        echo ""
        show_logs
    fi

    exit $test_result
}

main "$@"
