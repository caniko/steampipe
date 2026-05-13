#!/usr/bin/env bash
# Capture frame with grim (wlroots screencopy). Compare to committed golden.
# Outputs: $RUN_DIR/03-grim.png and 03-grim.normalized.sha256
# Exit: 0 MATCH, 5 DIFF, 6 NO_GOLDEN, 7 CAPTURE_FAILED
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
    [ -f "$VM_ID_FILE" ] || { echo "FAIL: missing leased_vm.id" >&2; exit 7; }
    VM_IP="10.0.100.$(tr -d '\n' <"$VM_ID_FILE")"
fi

OUT="$RUN_DIR/03-grim.png"
LOG="$RUN_DIR/03-grim.log"
GOLDEN="$FIXTURE_DIR/golden/${WORKLOAD}-1280x720.png"

# Capture
ssh $SSH_OPTS "$VM_USER@$VM_IP" \
    "export XDG_RUNTIME_DIR=/tmp/runtime-$VM_USER; \
     export WAYLAND_DISPLAY=wayland-1; \
     grim /tmp/grim.png" >"$LOG" 2>&1 || {
    echo "FAIL: CAPTURE_FAILED — grim invocation failed"
    exit 7
}
rsync -e "ssh $SSH_OPTS" "$VM_USER@$VM_IP:/tmp/grim.png" "$OUT" >>"$LOG" 2>&1 || {
    echo "FAIL: CAPTURE_FAILED — rsync of grim.png failed"
    exit 7
}

# Hash (data-only, ignoring metadata via PIL → raw RGB bytes)
HASH=$(python3 -c "
from PIL import Image
import hashlib
img = Image.open('$OUT').convert('RGBA')
print(hashlib.sha256(img.tobytes()).hexdigest())
")
echo "$HASH" >"$RUN_DIR/03-grim.normalized.sha256"

if [ ! -f "$GOLDEN" ]; then
    echo "WARN: NO_GOLDEN — $GOLDEN missing; captured hash $HASH"
    exit 6
fi

G_HASH=$(python3 -c "
from PIL import Image
import hashlib
img = Image.open('$GOLDEN').convert('RGBA')
print(hashlib.sha256(img.tobytes()).hexdigest())
")
if [ "$HASH" = "$G_HASH" ]; then
    echo "OK: MATCH (sha256 $HASH)"
    exit 0
fi
echo "DIFF: actual=$HASH golden=$G_HASH"
exit 5
