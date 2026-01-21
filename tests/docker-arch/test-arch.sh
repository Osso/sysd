#!/bin/bash
# Test script run inside Docker container with sysd as PID 1

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

PASSED=0
FAILED=0

pass() {
    echo -e "${GREEN}✓${NC} $1"
    PASSED=$((PASSED + 1))
}

fail() {
    echo -e "${RED}✗${NC} $1"
    FAILED=$((FAILED + 1))
}

log() { echo -e "${GREEN}[+]${NC} $*"; }
err() { echo -e "${RED}[-]${NC} $*" >&2; }

# Test: sysd is running as PID 1
test_pid1() {
    local init_name
    init_name=$(cat /proc/1/comm)
    if [[ "$init_name" == "sysd" ]]; then
        pass "sysd running as PID 1"
    else
        fail "sysd not PID 1 (found: $init_name)"
    fi
}

# Test: sysdctl can connect
test_connection() {
    if timeout 3 /usr/bin/sysdctl ping 2>/dev/null; then
        pass "sysdctl ping"
    else
        fail "sysdctl ping"
    fi
}

# Test: D-Bus socket exists
test_dbus_socket() {
    if [[ -S /run/dbus/system_bus_socket ]]; then
        pass "D-Bus socket exists"
    else
        fail "D-Bus socket not found"
    fi
}

# Test: D-Bus is functional and sysd is registered
test_dbus_systemd1() {
    local output
    local attempts=0
    local max_attempts=10

    while [[ $attempts -lt $max_attempts ]]; do
        output=$(timeout 3 dbus-send --system --dest=org.freedesktop.DBus --print-reply \
            /org/freedesktop/DBus org.freedesktop.DBus.ListNames 2>&1)
        if echo "$output" | grep -q "org.freedesktop.systemd1"; then
            pass "D-Bus org.freedesktop.systemd1 registered"
            return
        fi
        attempts=$((attempts + 1))
        sleep 1
    done
    fail "D-Bus org.freedesktop.systemd1 not registered after ${max_attempts}s"
}

# Test: Unit discovery via target
test_unit_discovery() {
    # Start multi-user.target to trigger dependency resolution
    timeout 5 /usr/bin/sysdctl start multi-user.target 2>&1 || true
    sleep 1

    local output
    output=$(timeout 3 /usr/bin/sysdctl list 2>&1)
    if [[ $? -eq 0 ]] && [[ -n "$output" ]] && [[ "$output" != "No units loaded" ]]; then
        local count
        count=$(echo "$output" | wc -l)
        pass "unit discovery ($count units loaded)"
    else
        fail "unit discovery"
    fi
}

# Test: Start a service
test_service_start() {
    local service="$1"
    if timeout 5 /usr/bin/sysdctl start "$service" 2>&1; then
        pass "start $service"
        return 0
    else
        fail "start $service"
        return 1
    fi
}

# Test: Stop a service
test_service_stop() {
    local service="$1"
    if timeout 5 /usr/bin/sysdctl stop "$service" 2>/dev/null; then
        pass "stop $service"
    else
        fail "stop $service"
    fi
}

# Test: Service status
test_service_status() {
    local service="$1"
    if timeout 3 /usr/bin/sysdctl status "$service" &>/dev/null; then
        pass "status $service"
    else
        fail "status $service"
    fi
}

# Test: Target units
test_targets() {
    local output
    output=$(timeout 3 /usr/bin/sysdctl list -t target 2>&1)
    if [[ $? -eq 0 ]]; then
        local count
        count=$(echo "$output" | grep -c "\\.target" 2>/dev/null) || count=0
        if [[ $count -gt 0 ]]; then
            pass "target units ($count loaded)"
        else
            fail "target units (none loaded)"
        fi
    else
        fail "target units query"
    fi
}

# Test: Socket units
test_sockets() {
    local output
    output=$(timeout 3 /usr/bin/sysdctl list -t socket 2>&1)
    if [[ $? -eq 0 ]]; then
        local count
        count=$(echo "$output" | grep -c "\\.socket" 2>/dev/null) || count=0
        if [[ $count -gt 0 ]]; then
            pass "socket units ($count loaded)"
        else
            fail "socket units (none loaded)"
        fi
    else
        fail "socket units query"
    fi
}

# Test: Boot target resolution
test_boot_target() {
    local output
    output=$(timeout 3 /usr/bin/sysdctl get-boot-target 2>&1)
    if [[ $? -eq 0 ]] && [[ -n "$output" ]]; then
        pass "boot target: $output"
    else
        fail "boot target resolution"
    fi
}

# Test: Dependencies resolution
test_deps() {
    local unit="$1"
    local output
    output=$(timeout 3 /usr/bin/sysdctl deps "$unit" 2>&1)
    if [[ $? -eq 0 ]]; then
        pass "deps $unit"
    else
        fail "deps $unit"
    fi
}

# Test: Cgroup exists for sysd
test_cgroups() {
    if [[ -d /sys/fs/cgroup/system.slice ]]; then
        pass "cgroup hierarchy (system.slice exists)"
    else
        fail "cgroup hierarchy (no system.slice)"
    fi
}

# Main test sequence
main() {
    log "=== sysd Docker Arch Full Boot Test ==="
    log "Testing sysd as PID 1 with full Arch Linux"
    echo ""

    log "--- Basic Checks ---"
    test_pid1
    test_connection
    echo ""

    log "--- Boot Target ---"
    test_boot_target
    echo ""

    log "--- Unit Discovery ---"
    test_unit_discovery
    echo ""

    log "--- Unit Types ---"
    test_targets
    test_sockets
    echo ""

    log "--- D-Bus ---"
    test_dbus_socket
    test_dbus_systemd1
    echo ""

    log "--- Cgroups ---"
    test_cgroups
    echo ""

    log "--- Service Lifecycle (dbus-broker) ---"
    # Test dbus-broker - should already be started via socket activation
    test_service_status "dbus-broker.service"
    echo ""

    log "--- Dependency Resolution ---"
    test_deps "multi-user.target"
    test_deps "dbus.socket"
    echo ""

    log "=== Test Results ==="
    echo -e "Passed: ${GREEN}$PASSED${NC}"
    echo -e "Failed: ${RED}$FAILED${NC}"
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
