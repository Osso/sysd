#!/bin/bash
# Compare sysd boot plan with systemd's active units

set -e

SYSD_PLAN=$(mktemp)
SYSTEMD_PLAN=$(mktemp)
trap 'rm -f "$SYSD_PLAN" "$SYSTEMD_PLAN"' EXIT

# Get sysd boot plan
cargo run --release --example test_symlink 2>/dev/null | sort > "$SYSD_PLAN"

# Get systemd boot plan (dependencies of graphical.target, not active units)
systemctl list-dependencies graphical.target --plain --no-pager 2>/dev/null \
    | grep -v '^$' | sed 's/^[[:space:]]*//' | sort -u > "$SYSTEMD_PLAN"

sysd_count=$(wc -l < "$SYSD_PLAN")
systemd_count=$(wc -l < "$SYSTEMD_PLAN")
common=$(comm -12 "$SYSD_PLAN" "$SYSTEMD_PLAN" | wc -l)

echo "=== Boot Plan Comparison ==="
echo "sysd:    $sysd_count units"
echo "systemd: $systemd_count units"
echo "common:  $common ($(( common * 100 / systemd_count ))%)"
echo

echo "=== Only in sysd ($(comm -23 "$SYSD_PLAN" "$SYSTEMD_PLAN" | wc -l)) ==="
comm -23 "$SYSD_PLAN" "$SYSTEMD_PLAN"
echo

echo "=== Only in systemd ($(comm -13 "$SYSD_PLAN" "$SYSTEMD_PLAN" | wc -l)) ==="
comm -13 "$SYSD_PLAN" "$SYSTEMD_PLAN"
