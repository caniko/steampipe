#!/usr/bin/env bash
# Idempotently bring up the fixture: bridge → VM → workload running.
# Emits a one-line status to stdout. Exit 0 OK, non-zero on any failure.
#
# Inputs (env):
#   FIXTURE_DIR     — absolute path to tests/fixture
#   RUN_DIR         — absolute path to _run-<ts> output dir
#   WORKLOAD        — workload name (default: foot-banner)
#   CLUSTER_NAME    — fixture cluster name for the selected flavor
#   VM_IP           — optional VM IP override (otherwise derived from leased_vm.id)
#   VM_USER         — SSH user (default: fixture)
#   CLUSTER_CTL     — path to cluster-ctl binary
#   SKIP_NET_UP     — set to 1 to skip net-up (assume bridge already up)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/fixture/diagnose/steps/lib.sh
. "$SCRIPT_DIR/lib.sh"

FIXTURE_DIR="${FIXTURE_DIR:?FIXTURE_DIR required}"
RUN_DIR="${RUN_DIR:?RUN_DIR required}"
WORKLOAD="${WORKLOAD:-foot-banner}"
CLUSTER_NAME="${CLUSTER_NAME:?CLUSTER_NAME required}"
VM_IP="${VM_IP:-}"
VM_USER="${VM_USER:-fixture}"
CLUSTER_CTL="${CLUSTER_CTL:?CLUSTER_CTL required}"
SSH_KEY="$FIXTURE_DIR/cluster_key"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -i $SSH_KEY"
LEASED_VM_ID_FILE="$RUN_DIR/leased_vm.id"
LEASED_VM_NAME_FILE="$RUN_DIR/leased_vm.name"
LEASED_VM_IP_FILE="$RUN_DIR/leased_vm.ip"

log() { printf '[01-boot] %s\n' "$*" >&2; }
ok()  { echo "OK: $*"; exit 0; }
fail(){ echo "FAIL: $*"; exit 1; }

record_leased_vm() {
    local vm_id="$1"
    VM_IP="10.0.100.$vm_id"
    printf '%s\n' "$vm_id" >"$LEASED_VM_ID_FILE"
    printf 'vm-%s\n' "$vm_id" >"$LEASED_VM_NAME_FILE"
    printf '%s\n' "$VM_IP" >"$LEASED_VM_IP_FILE"
    log "leased VM is vm-$vm_id ($VM_IP)"
}

discover_leased_vm_from_status() {
    "$CLUSTER_CTL" --project-root "$FIXTURE_DIR" --cluster "$CLUSTER_NAME" --vm-count 1 status \
        2>/dev/null | awk '/^vm-[0-9]+/ { sub(/^vm-/, "", $1); print $1; exit }' || true
}

discover_leased_vm_from_boot_log() {
    parse_reserved_vm_id_from_log "$RUN_DIR/01-boot.cluster-up.log" || true
}

mkdir -p "$RUN_DIR"
exec > >(tee -a "$RUN_DIR/01-boot.log") 2>&1

if [ -n "$VM_IP" ]; then
    record_leased_vm "${VM_IP##*.}"
fi
if [ -z "$VM_IP" ]; then
    vm_id="$(discover_leased_vm_from_status)"
    if [ -n "$vm_id" ]; then
        record_leased_vm "$vm_id"
    fi
fi

# 1) bridge: must already exist. On atlas this is provided declaratively
#    by canix (services.steampipe-cluster wires br-cluster + tap-vm-1..8
#    owned by uid 1000). If it isn't present, the host config has not
#    been deployed.
if ! ip link show br-cluster >/dev/null 2>&1; then
    fail "br-cluster missing — deploy canix host config: (cd /data/nvme0/can/Projects/canix && canix deploy switch atlas)"
fi
log "bridge br-cluster present"

# 2) VM: if SSH already reachable, skip up; else boot.
if [ -n "$VM_IP" ] && ssh $SSH_OPTS "$VM_USER@$VM_IP" 'true' >/dev/null 2>&1; then
    log "VM already up at $VM_IP"
else
    FLAVOR="${FLAVOR:-default}"
    if [ "$FLAVOR" = "default" ]; then
        UP_APP="cluster-1v1-up"
    else
        UP_APP="cluster-1v1-${FLAVOR}-up"
    fi
    # Pre-build the runner so the SSH-wait window only covers the actual
    # boot, not nix derivation builds (which can take minutes cold).
    log "pre-building $UP_APP runner"
    if ! nix build "$FIXTURE_DIR#$UP_APP.program" --no-link \
            >"$RUN_DIR/01-boot.build.log" 2>&1; then
        fail "nix build $UP_APP failed — see 01-boot.build.log"
    fi
    log "booting VM via $UP_APP (background)"
    nix run "$FIXTURE_DIR#$UP_APP" >"$RUN_DIR/01-boot.cluster-up.log" 2>&1 &
    BOOT_PID=$!
    # wait up to 90s for SSH
    for i in $(seq 1 90); do
        if [ -z "$VM_IP" ]; then
            vm_id="$(discover_leased_vm_from_boot_log)"
            if [ -n "$vm_id" ]; then
                record_leased_vm "$vm_id"
            fi
        fi
        if [ -n "$VM_IP" ] && ssh $SSH_OPTS "$VM_USER@$VM_IP" 'true' >/dev/null 2>&1; then
            log "SSH reachable after ${i}s"
            break
        fi
        sleep 1
    done
    if [ -z "$VM_IP" ] || ! ssh $SSH_OPTS "$VM_USER@$VM_IP" 'true' >/dev/null 2>&1; then
        kill "$BOOT_PID" 2>/dev/null || true
        fail "SSH never reached $VM_IP within 90s — see $RUN_DIR/01-boot.cluster-up.log"
    fi
fi

if [ ! -f "$LEASED_VM_ID_FILE" ]; then
    vm_id="$(discover_leased_vm_from_boot_log)"
    if [ -z "$vm_id" ]; then vm_id="$(discover_leased_vm_from_status)"; fi
    if [ -z "$vm_id" ]; then
        fail "could not determine leased VM from up output or cluster status"
    fi
    record_leased_vm "$vm_id"
fi

# 3) Start the requested workload. workload-stop is best-effort.
log "launching workload: $WORKLOAD"
ssh $SSH_OPTS "$VM_USER@$VM_IP" "workload-stop || true; workload-run $WORKLOAD" \
    >"$RUN_DIR/01-boot.workload.log" 2>&1 || {
        fail "workload-run $WORKLOAD failed — see 01-boot.workload.log"
    }

# 4) Wait for the workload window to actually appear in sway tree.
log "waiting for workload window to appear in sway tree"
for i in $(seq 1 15); do
    if ssh $SSH_OPTS "$VM_USER@$VM_IP" \
        "export XDG_RUNTIME_DIR=/tmp/runtime-$VM_USER; export SWAYSOCK=\$(find \"\$XDG_RUNTIME_DIR\" -maxdepth 1 -type s -name 'sway-ipc.*.sock' | head -n1); swaymsg -t get_tree 2>/dev/null" \
        | grep -qi "workload-$WORKLOAD\|workload-${WORKLOAD%-*}"
    then
        log "workload window visible after ${i}s"
        ok "boot+workload ready"
    fi
    sleep 1
done

# Workload did not appear — still treat boot itself as OK; 02-compositor will
# classify this case.
ok "boot ok, workload window not yet visible (02-compositor will detect)"
