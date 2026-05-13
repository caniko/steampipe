#!/usr/bin/env bash
# Query sway IPC: are outputs active, is the workload window mapped?
# Writes raw JSON to $RUN_DIR/02-compositor.json
# Exit codes: 0 OK, 2 NO_OUTPUTS, 3 NO_WORKLOAD_WINDOW, 4 SWAY_UNREACHABLE
set -euo pipefail

FIXTURE_DIR="${FIXTURE_DIR:?}"
RUN_DIR="${RUN_DIR:?}"
WORKLOAD="${WORKLOAD:-foot-banner}"
VM_IP="${VM_IP:-}"
VM_USER="${VM_USER:-fixture}"
SSH_KEY="$FIXTURE_DIR/cluster_key"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -i $SSH_KEY"

if [ -z "$VM_IP" ]; then
    VM_ID_FILE="$RUN_DIR/leased_vm.id"
    [ -f "$VM_ID_FILE" ] || { echo "FAIL: missing leased_vm.id" >&2; exit 4; }
    VM_IP="10.0.100.$(tr -d '\n' <"$VM_ID_FILE")"
fi

OUT="$RUN_DIR/02-compositor.json"
LOG="$RUN_DIR/02-compositor.log"

ssh $SSH_OPTS "$VM_USER@$VM_IP" \
    "export XDG_RUNTIME_DIR=/tmp/runtime-$VM_USER; \
     export SWAYSOCK=\$(find \"\$XDG_RUNTIME_DIR\" -maxdepth 1 -type s -name 'sway-ipc.*.sock' | head -n1); \
     echo '{\"outputs\":'; swaymsg -t get_outputs 2>/dev/null || echo null; \
     echo ',\"tree\":'; swaymsg -t get_tree 2>/dev/null || echo null; \
     echo '}'" >"$OUT" 2>"$LOG" || {
    echo "FAIL: SWAY_UNREACHABLE — see 02-compositor.log" >&2
    exit 4
}

# Validate JSON
if ! python3 -c "import json,sys; json.load(open('$OUT'))" 2>>"$LOG"; then
    echo "FAIL: SWAY_UNREACHABLE — invalid JSON in 02-compositor.json" >&2
    exit 4
fi

# Active outputs?
ACTIVE=$(python3 -c "
import json
d = json.load(open('$OUT'))
outs = d.get('outputs') or []
print(sum(1 for o in outs if o.get('active')))
")
if [ "$ACTIVE" -lt 1 ]; then
    echo "FAIL: NO_OUTPUTS — sway reports no active output"
    exit 2
fi

# Workload window present?
FOUND=$(python3 -c "
import json
d = json.load(open('$OUT'))
def walk(n):
    if not n: return False
    title = (n.get('name') or '').lower()
    app   = (n.get('app_id') or '').lower()
    if 'workload-$WORKLOAD' in title or 'workload-$WORKLOAD' in app:
        return True
    for k in ('nodes','floating_nodes'):
        for c in (n.get(k) or []):
            if walk(c): return True
    return False
print('YES' if walk(d.get('tree')) else 'NO')
")
if [ "$FOUND" != "YES" ]; then
    echo "FAIL: NO_WORKLOAD_WINDOW — workload-$WORKLOAD not visible in sway tree"
    exit 3
fi

echo "OK: $ACTIVE output(s) active, workload-$WORKLOAD mapped"
exit 0
