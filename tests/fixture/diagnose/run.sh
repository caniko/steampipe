#!/usr/bin/env bash
# Orchestrate the 6-step diagnostic ladder against the in-tree fixture.
# Writes results to tests/fixture/diagnose/_run-<timestamp>/.
#
# Usage:
#   tests/fixture/diagnose/run.sh [--workload NAME] [--keep-up] [--no-vnc]
#
# Exit code is the classifier's verdict code (0 GREEN, 11-17 various, 99 broken).
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$FIXTURE_DIR/../.." && pwd)"

WORKLOAD="foot-banner"
FLAVOR="default"
KEEP_UP=0
NO_VNC=0

while [ $# -gt 0 ]; do
    case "$1" in
        --workload) WORKLOAD="$2"; shift 2 ;;
        --flavor)   FLAVOR="$2"; shift 2 ;;
        --keep-up)  KEEP_UP=1; shift ;;
        --no-vnc)   NO_VNC=1; shift ;;
        -h|--help)
            sed -n '2,12p' "$0"; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [ "$FLAVOR" = "default" ]; then
    CLUSTER_NAME="fixture"
else
    CLUSTER_NAME="fixture-$FLAVOR"
fi

# Locate cluster-ctl. Prefer release; fall back to debug; build if neither.
CLUSTER_CTL="$REPO_ROOT/target/release/cluster-ctl"
if [ ! -x "$CLUSTER_CTL" ]; then
    CLUSTER_CTL="$REPO_ROOT/target/debug/cluster-ctl"
fi
if [ ! -x "$CLUSTER_CTL" ]; then
    echo "[run.sh] building cluster-ctl..." >&2
    if command -v cargo >/dev/null 2>&1; then
        (cd "$REPO_ROOT" && cargo build --release --quiet)
    else
        (cd "$REPO_ROOT" && nix develop -c cargo build --release --quiet)
    fi
    CLUSTER_CTL="$REPO_ROOT/target/release/cluster-ctl"
fi

TS="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="$SCRIPT_DIR/_run-$TS"
mkdir -p "$RUN_DIR"
echo "[run.sh] RUN_DIR=$RUN_DIR" >&2

export FIXTURE_DIR RUN_DIR WORKLOAD FLAVOR CLUSTER_NAME CLUSTER_CTL NO_VNC

run_step() {
    local name="$1" script="$2"
    echo "[run.sh] === $name ===" >&2
    set +e
    bash "$script" >"$RUN_DIR/$name.out" 2>"$RUN_DIR/$name.err"
    local rc=$?
    set -e
    echo "$rc" >"$RUN_DIR/$name.exit"
    echo "[run.sh]   exit=$rc" >&2
    if [ -s "$RUN_DIR/$name.out" ]; then
        sed 's/^/      /' "$RUN_DIR/$name.out" >&2
    fi
}

run_step "01-boot"        "$SCRIPT_DIR/steps/01-boot.sh"
run_step "02-compositor"  "$SCRIPT_DIR/steps/02-compositor.sh"
run_step "03-grim"        "$SCRIPT_DIR/steps/03-grim.sh"
run_step "04-vnc"         "$SCRIPT_DIR/steps/04-vnc.sh"
run_step "05-gpu"         "$SCRIPT_DIR/steps/05-gpu.sh"
run_step "06-wayvnc-log"  "$SCRIPT_DIR/steps/06-wayvnc-log.sh"

echo "[run.sh] === 07-classify ===" >&2
set +e
nix shell nixpkgs#python3 -c python3 "$SCRIPT_DIR/steps/07-classify.py"
VERDICT_EXIT=$?
set -e

if [ "$KEEP_UP" != "1" ]; then
    echo "[run.sh] tearing down VM (set --keep-up to skip)" >&2
    "$CLUSTER_CTL" --project-root "$FIXTURE_DIR" --cluster "$CLUSTER_NAME" --vm-count 1 down \
        >"$RUN_DIR/99-down.log" 2>&1 || true
fi

echo "[run.sh] summary at: $RUN_DIR/summary.md" >&2
echo "[run.sh] verdict.json at: $RUN_DIR/verdict.json" >&2
exit "$VERDICT_EXIT"
