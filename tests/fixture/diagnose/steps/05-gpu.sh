#!/usr/bin/env bash
# Probe the VM's graphics stack: DRI nodes, virtio_gpu module, vulkaninfo,
# wayland-info, vkcube one-frame test.
# Exit codes:
#   0  ALL_OK
#   20 NO_DRI         — no /dev/dri/card*
#   21 NO_MODULE      — virtio_gpu not loaded
#   22 NO_VULKAN_DEV  — vulkaninfo finds no device
#   23 VKCUBE_FAIL
#   24 NO_WAYLAND
set -euo pipefail

FIXTURE_DIR="${FIXTURE_DIR:?}"
RUN_DIR="${RUN_DIR:?}"
VM_IP="${VM_IP:-}"
VM_USER="${VM_USER:-fixture}"
SSH_KEY="$FIXTURE_DIR/cluster_key"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -i $SSH_KEY"

if [ -z "$VM_IP" ]; then
    VM_ID_FILE="$RUN_DIR/leased_vm.id"
    [ -f "$VM_ID_FILE" ] || { echo "FAIL: missing leased_vm.id" >&2; exit 24; }
    VM_IP="10.0.100.$(tr -d '\n' <"$VM_ID_FILE")"
fi

OUT="$RUN_DIR/05-gpu.txt"

ssh $SSH_OPTS "$VM_USER@$VM_IP" '
export XDG_RUNTIME_DIR=/tmp/runtime-fixture
export WAYLAND_DISPLAY=wayland-1
echo "===DRI==="
ls -la /dev/dri/ 2>&1 || true
echo "===MODULE==="
(lsmod 2>/dev/null | grep "^virtio_gpu " || ls -la /sys/module/virtio_gpu 2>&1 || echo NO_MODULE)
echo "===VULKAN==="
vulkaninfo --summary 2>&1 | head -60 || echo VULKANINFO_FAIL
echo "===WAYLAND==="
(wayland-info 2>&1 | head -40) || echo WAYLAND_INFO_FAIL
echo "===VKCUBE==="
timeout 10 vkcube --c 1 >/dev/null 2>&1 && echo VKCUBE_OK || echo VKCUBE_FAIL
' >"$OUT" 2>&1

# Parse
fail=""
grep -q "card0" "$OUT" || fail="${fail}NO_DRI "
grep -q -E "^virtio_gpu |/sys/module/virtio_gpu" "$OUT" || fail="${fail}NO_MODULE "
grep -q -E "deviceName|driverName|GPU id" "$OUT" || fail="${fail}NO_VULKAN_DEV "
grep -q "VKCUBE_OK" "$OUT" || fail="${fail}VKCUBE_FAIL "
grep -q -E "wl_compositor|interface:" "$OUT" || fail="${fail}NO_WAYLAND "

if [ -z "$fail" ]; then
    echo "OK: dri, module, vulkan device, vkcube, wayland all present"
    exit 0
fi
# Precedence: most fundamental first
case "$fail" in
    *NO_DRI*)        echo "FAIL: $fail"; exit 20 ;;
    *NO_MODULE*)     echo "FAIL: $fail"; exit 21 ;;
    *NO_VULKAN_DEV*) echo "FAIL: $fail"; exit 22 ;;
    *VKCUBE_FAIL*)   echo "FAIL: $fail"; exit 23 ;;
    *NO_WAYLAND*)    echo "FAIL: $fail"; exit 24 ;;
esac
echo "FAIL: $fail"
exit 23
