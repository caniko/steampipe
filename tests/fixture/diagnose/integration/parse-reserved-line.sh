#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STEP_DIR="$(cd "$SCRIPT_DIR/../steps" && pwd)"
# shellcheck source=tests/fixture/diagnose/steps/lib.sh
. "$STEP_DIR/lib.sh"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

LOG="$TMP_DIR/up.log"

cat >"$LOG" <<'EOF'
==> Reserving 2 VM(s)...
  vm-1 held by 'other-cluster' (pid 4242), skipping
  vm-3 held by 'other-cluster' (pid 4343), skipping
  Reserved: vm-2, vm-4
==> Starting VMs...
EOF

vm_id="$(parse_reserved_vm_id_from_log "$LOG")"
if [ "$vm_id" != "2" ]; then
    echo "expected parser to extract vm id 2, got: '$vm_id'" >&2
    exit 1
fi

cat >"$LOG" <<'EOF'
==> Reserving 1 VM(s)...
  vm-1 held by 'other-cluster' (pid 5555), skipping
EOF

vm_id="$(parse_reserved_vm_id_from_log "$LOG")"
if [ -n "$vm_id" ]; then
    echo "expected empty parse result when Reserved line is absent, got: '$vm_id'" >&2
    exit 1
fi

echo "parse_reserved_vm_id_from_log ok"
