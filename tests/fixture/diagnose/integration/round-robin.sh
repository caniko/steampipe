#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIAG_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LOCK_DIR="${XDG_RUNTIME_DIR:-/tmp}/steampipe"
mkdir -p "$LOCK_DIR"

WORK_DIR="$(mktemp -d)"
CLAIMS=()

cleanup() {
    set +e
    for claim in "${CLAIMS[@]:-}"; do
        [ -n "$claim" ] || continue
        if grep -q "\"pid\":$$" "$claim" 2>/dev/null; then
            rm -f "$claim"
        fi
    done
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

leased_ids=()

for iter in 1 2 3; do
    log="$WORK_DIR/run-$iter.log"
    set +e
    nix develop -c bash "$DIAG_DIR/run.sh" --flavor uifull --no-vnc >"$log" 2>&1
    rc=$?
    set -e

    run_dir="$(grep -oE '\[run\.sh\] RUN_DIR=.*' "$log" | tail -n1 | sed 's/.*RUN_DIR=//')"
    if [ -z "$run_dir" ] || [ ! -f "$run_dir/leased_vm.id" ]; then
        cat "$log" >&2
        echo "round-robin iteration $iter did not produce leased_vm.id" >&2
        exit 1
    fi

    vm_id="$(tr -d '\n' <"$run_dir/leased_vm.id")"
    if [[ ! "$vm_id" =~ ^[0-9]+$ ]]; then
        cat "$log" >&2
        echo "round-robin iteration $iter produced invalid leased_vm.id: '$vm_id'" >&2
        exit 1
    fi

    leased_ids+=("$vm_id")
    printf '{"cluster":"round-robin-hold","pid":%d,"since":"%s"}' \
        "$$" "$(date -Iseconds)" >"$LOCK_DIR/vm-${vm_id}.claim"
    CLAIMS+=("$LOCK_DIR/vm-${vm_id}.claim")

    echo "iteration $iter: leased vm-$vm_id (diagnostic exit $rc)"
done

unique_count="$(printf '%s\n' "${leased_ids[@]}" | sort -u | wc -l | tr -d ' ')"
if [ "$unique_count" -ne 3 ]; then
    echo "expected three distinct leased VMs, got: ${leased_ids[*]}" >&2
    exit 1
fi

echo "leased VMs across runs: ${leased_ids[*]}"
