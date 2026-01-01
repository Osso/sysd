#!/bin/bash
# Docker-based integration tests for sysd
# Tests real Arch Linux systemd units

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

PASSED=0
FAILED=0
SKIPPED=0

log() { echo -e "${GREEN}[+]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err() { echo -e "${RED}[-]${NC} $*"; }

pass() {
    echo -e "${GREEN}✓${NC} $1"
    PASSED=$((PASSED + 1))
}

fail() {
    echo -e "${RED}✗${NC} $1"
    FAILED=$((FAILED + 1))
}

skip() {
    echo -e "${YELLOW}○${NC} $1 (skipped)"
    SKIPPED=$((SKIPPED + 1))
}

# Start sysd in background
start_sysd() {
    log "Starting sysd..."
    /usr/local/bin/sysd --foreground &
    SYSD_PID=$!

    # Wait for socket
    local waited=0
    while [[ ! -S /run/sysd.sock ]] && [[ $waited -lt 10 ]]; do
        sleep 0.5
        waited=$((waited + 1))
    done

    if [[ -S /run/sysd.sock ]]; then
        log "sysd started (PID $SYSD_PID)"
        return 0
    else
        err "sysd failed to start"
        return 1
    fi
}

stop_sysd() {
    if [[ -n "${SYSD_PID:-}" ]]; then
        kill "$SYSD_PID" 2>/dev/null || true
        wait "$SYSD_PID" 2>/dev/null || true
    fi
}

trap stop_sysd EXIT

# Test: sysdctl can connect
test_connection() {
    if /usr/local/bin/sysdctl ping 2>/dev/null; then
        pass "sysdctl ping"
    else
        fail "sysdctl ping"
    fi
}

# Discover units by starting a target (units are loaded on demand)
discover_units() {
    log "Discovering units via multi-user.target..."
    # Start multi-user.target to trigger unit discovery through dependency resolution
    /usr/local/bin/sysdctl start multi-user.target 2>&1 || true
    sleep 1
}

# Test: List units
test_list_units() {
    local output
    output=$(/usr/local/bin/sysdctl list 2>&1)

    if [[ $? -eq 0 ]] && [[ -n "$output" ]] && [[ "$output" != "No units loaded" ]]; then
        local count
        count=$(echo "$output" | wc -l)
        pass "sysdctl list ($count units loaded)"
    else
        fail "sysdctl list (no units loaded)"
    fi
}

# Test: List specific unit types
test_list_by_type() {
    local unit_type="$1"
    local output
    output=$(/usr/local/bin/sysdctl list -t "$unit_type" 2>&1)

    if [[ $? -eq 0 ]]; then
        local count
        count=$(echo "$output" | grep -c "\\.$unit_type" 2>/dev/null) || count=0
        pass "sysdctl list -t $unit_type ($count units)"
    else
        fail "sysdctl list -t $unit_type"
    fi
}

# Test: Start and stop a service
test_service_lifecycle() {
    local service="$1"

    # Try to start the service (this will load it if not already loaded)
    local start_output
    start_output=$(/usr/local/bin/sysdctl start "$service" 2>&1)
    local start_rc=$?

    if [[ $start_rc -eq 0 ]]; then
        sleep 1
        pass "start $service"

        # Check if service is still running before trying to stop
        local status_output state_line
        status_output=$(/usr/local/bin/sysdctl status "$service" 2>&1) || true
        state_line=$(echo "$status_output" | grep "State:" || echo "")
        # Service is running if state is not Inactive or Failed
        if [[ -n "$state_line" ]] && ! echo "$state_line" | grep -qiE "inactive|failed"; then
            # Service is running, try to stop it
            if /usr/local/bin/sysdctl stop "$service" 2>/dev/null; then
                sleep 1
                pass "stop $service"
            else
                fail "stop $service"
            fi
        else
            # Service already exited (e.g., failed to start properly)
            skip "stop $service (service already exited)"
        fi
    else
        # Check if it's a known error we can skip
        if echo "$start_output" | grep -q "not found\|No such file"; then
            skip "$service (unit file not found)"
        else
            fail "start $service: $start_output"
        fi
    fi
}

# Test: Service dependencies
test_deps() {
    local service="$1"
    local output
    output=$(/usr/local/bin/sysdctl deps "$service" 2>&1)

    if [[ $? -eq 0 ]]; then
        pass "deps $service"
    else
        fail "deps $service"
    fi
}

# Test: Get default boot target
test_boot_target() {
    local output
    output=$(/usr/local/bin/sysdctl get-boot-target 2>&1)

    if [[ $? -eq 0 ]] && [[ -n "$output" ]]; then
        pass "get-boot-target: $output"
    else
        fail "get-boot-target"
    fi
}

# Test: Timer units
test_timer_units() {
    local output
    output=$(/usr/local/bin/sysdctl list -t timer 2>&1)

    if [[ $? -eq 0 ]]; then
        local count
        count=$(echo "$output" | grep -c "\\.timer" 2>/dev/null) || count=0
        if [[ $count -gt 0 ]]; then
            pass "timer units ($count found)"
        else
            skip "timer units (none found)"
        fi
    else
        fail "timer units"
    fi
}

# Test: Socket units
test_socket_units() {
    local output
    output=$(/usr/local/bin/sysdctl list -t socket 2>&1)

    if [[ $? -eq 0 ]]; then
        local count
        count=$(echo "$output" | grep -c "\\.socket" 2>/dev/null) || count=0
        if [[ $count -gt 0 ]]; then
            pass "socket units ($count found)"
        else
            skip "socket units (none found)"
        fi
    else
        fail "socket units"
    fi
}

# Test: Mount units
test_mount_units() {
    local output
    output=$(/usr/local/bin/sysdctl list -t mount 2>&1)

    if [[ $? -eq 0 ]]; then
        local count
        count=$(echo "$output" | grep -c "\\.mount" 2>/dev/null) || count=0
        pass "mount units ($count found)"
    else
        fail "mount units"
    fi
}

# Test: Target units
test_target_units() {
    local output
    output=$(/usr/local/bin/sysdctl list -t target 2>&1)

    if [[ $? -eq 0 ]]; then
        local count
        count=$(echo "$output" | grep -c "\\.target" 2>/dev/null) || count=0
        if [[ $count -gt 0 ]]; then
            pass "target units ($count found)"
        else
            fail "target units (expected some)"
        fi
    else
        fail "target units"
    fi
}

# Main test sequence
main() {
    log "=== sysd Docker Integration Tests ==="
    log "Testing with Arch Linux systemd units"
    echo ""

    start_sysd || exit 1
    echo ""

    log "--- Connection Tests ---"
    test_connection
    echo ""

    log "--- Unit Discovery ---"
    discover_units
    echo ""

    log "--- Unit Listing Tests ---"
    test_list_units
    test_list_by_type "service"
    test_list_by_type "target"
    test_list_by_type "socket"
    test_list_by_type "timer"
    test_list_by_type "mount"
    echo ""

    log "--- Unit Type Tests ---"
    test_target_units
    test_socket_units
    test_timer_units
    test_mount_units
    echo ""

    log "--- Service Lifecycle Tests ---"
    # Test nginx (simple service)
    test_service_lifecycle "nginx.service"

    # Test valkey (redis replacement)
    test_service_lifecycle "valkey.service"

    # Test sshd (socket-activated possible)
    test_service_lifecycle "sshd.service"
    echo ""

    log "--- Dependency Tests ---"
    test_deps "multi-user.target"
    echo ""

    log "--- Boot Target Tests ---"
    test_boot_target
    echo ""

    log "=== Test Results ==="
    echo -e "Passed: ${GREEN}$PASSED${NC}"
    echo -e "Failed: ${RED}$FAILED${NC}"
    echo -e "Skipped: ${YELLOW}$SKIPPED${NC}"
    echo ""

    if [[ $FAILED -gt 0 ]]; then
        err "Some tests failed"
        exit 1
    else
        log "All tests passed!"
        exit 0
    fi
}

main "$@"
