#!/usr/bin/env bash
# Tail wayvnc.log and search for known error signatures.
# Exit: 0 OK, 30 ENCODER_FAIL, 31 SCREENCOPY_FAIL, 32 DMABUF_FAIL, 33 LOG_MISSING
set -euo pipefail

FIXTURE_DIR="${FIXTURE_DIR:?}"
RUN_DIR="${RUN_DIR:?}"
VM_IP="${VM_IP:-}"
VM_USER="${VM_USER:-fixture}"
SSH_KEY="$FIXTURE_DIR/cluster_key"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -i $SSH_KEY"

if [ -z "$VM_IP" ]; then
    VM_ID_FILE="$RUN_DIR/leased_vm.id"
    [ -f "$VM_ID_FILE" ] || { echo "WARN: missing leased_vm.id" >&2; exit 33; }
    VM_IP="10.0.100.$(tr -d '\n' <"$VM_ID_FILE")"
fi

OUT="$RUN_DIR/06-wayvnc.log"

ssh $SSH_OPTS "$VM_USER@$VM_IP" '
LOG="/tmp/runtime-fixture/wayvnc.log"
if [ -f "$LOG" ]; then
    tail -n 200 "$LOG"
else
    echo "LOG_MISSING: $LOG not found"
fi
' >"$OUT" 2>&1

if grep -q "^LOG_MISSING" "$OUT"; then
    echo "WARN: LOG_MISSING — wayvnc.log not present (wayvnc never started?)"
    exit 33
fi

# Known error signatures
if grep -qi "failed to start encoder\|encoder error" "$OUT"; then
    echo "FAIL: ENCODER_FAIL"
    exit 30
fi
if grep -qi "screencopy.*fail\|no.*screencopy\|screencopy not supported" "$OUT"; then
    echo "FAIL: SCREENCOPY_FAIL"
    exit 31
fi
if grep -qi "dmabuf.*fail\|no.*dmabuf" "$OUT"; then
    echo "FAIL: DMABUF_FAIL"
    exit 32
fi

echo "OK: no known error signatures in wayvnc.log"
exit 0
