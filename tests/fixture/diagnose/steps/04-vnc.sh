#!/usr/bin/env bash
# Capture via cluster-ctl screenshot --screenshot-backend vnc.
# Exit: 0 MATCH, 5 DIFF, 6 NO_GOLDEN, 7 CAPTURE_FAILED, 8 SKIPPED
set -euo pipefail

FIXTURE_DIR="${FIXTURE_DIR:?}"
RUN_DIR="${RUN_DIR:?}"
WORKLOAD="${WORKLOAD:-foot-banner}"
CLUSTER_NAME="${CLUSTER_NAME:?}"
CLUSTER_CTL="${CLUSTER_CTL:?}"
NO_VNC="${NO_VNC:-0}"
VM_ID_FILE="$RUN_DIR/leased_vm.id"

OUT="$RUN_DIR/04-vnc.png"
LOG="$RUN_DIR/04-vnc.log"
GOLDEN="$FIXTURE_DIR/golden/${WORKLOAD}-1280x720.png"

if [ "$NO_VNC" = "1" ]; then
    echo "SKIPPED: NO_VNC=1"
    exit 8
fi

TMP=$(mktemp -d -t vnc-shot-XXXX)
trap 'rm -rf "$TMP"' EXIT

[ -f "$VM_ID_FILE" ] || { echo "FAIL: CAPTURE_FAILED — missing leased_vm.id"; exit 7; }
VM_ID="$(tr -d '\n' <"$VM_ID_FILE")"
VM_NAME="vm-$VM_ID"

"$CLUSTER_CTL" --project-root "$FIXTURE_DIR" --cluster "$CLUSTER_NAME" --vm-count 1 \
    screenshot --screenshot-backend vnc -o "$TMP" >"$LOG" 2>&1 || {
    echo "FAIL: CAPTURE_FAILED — see 04-vnc.log"
    exit 7
}

if [ ! -f "$TMP/$VM_NAME.png" ]; then
    echo "FAIL: CAPTURE_FAILED — $VM_NAME.png not produced"
    exit 7
fi
cp "$TMP/$VM_NAME.png" "$OUT"

HASH=$(python3 -c "
from PIL import Image
import hashlib
img = Image.open('$OUT').convert('RGBA')
print(hashlib.sha256(img.tobytes()).hexdigest())
")
echo "$HASH" >"$RUN_DIR/04-vnc.normalized.sha256"

if [ ! -f "$GOLDEN" ]; then
    echo "WARN: NO_GOLDEN — captured hash $HASH"
    exit 6
fi

G_HASH=$(python3 -c "
from PIL import Image
import hashlib
img = Image.open('$GOLDEN').convert('RGBA')
print(hashlib.sha256(img.tobytes()).hexdigest())
")
if [ "$HASH" = "$G_HASH" ]; then
    echo "OK: MATCH"
    exit 0
fi
echo "DIFF: actual=$HASH golden=$G_HASH"
exit 5
