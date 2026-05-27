#!/usr/bin/env python3
"""Read step results from $RUN_DIR and emit verdict.json + summary.md.

Exit code mirrors the verdict so CI / callers can branch on it.
"""
import json
import os
import sys
import time
from pathlib import Path

VERDICTS = {
    "GREEN":                  (0,  "All ladder steps pass; capture matches golden."),
    "VNC_CLIENT_BUG":         (11, "grim matches golden but VNC does not — capture-side bug."),
    "COMPOSITOR_NOT_READY":   (12, "Sway not painted or workload window absent."),
    "GPU_BROKEN":             (13, "GPU stack failed (DRI / virtio_gpu / Vulkan / vkcube)."),
    "COMPOSITOR_REGRESSION":  (14, "Both grim and VNC diverge from golden — compositor or workload regressed."),
    "WAYVNC_LOG_WARN":        (16, "Wayvnc log shows known error signatures."),
    "NO_GOLDEN":              (17, "No committed golden — capture is canonical but unverified."),
    "MIXED":                  (15, "Multiple ladder steps failed; see step results."),
    "CLASSIFIER_BROKEN":      (99, "Classifier itself crashed; raw step results may be usable."),
}

PHASE_MAPPING = {
    "VNC_CLIENT_BUG":        "docs/src/testing/fixture-diagnostics.md#verdicts",
    "COMPOSITOR_NOT_READY":  "docs/src/testing/fixture-diagnostics.md#verdicts",
    "GPU_BROKEN":            "docs/src/testing/fixture-diagnostics.md#verdicts",
    "COMPOSITOR_REGRESSION": "docs/src/testing/fixture-diagnostics.md#verdicts",
    "WAYVNC_LOG_WARN":       "docs/src/testing/fixture-diagnostics.md#verdicts",
    "NO_GOLDEN":             "docs/src/testing/fixture-diagnostics.md#verdicts",
}

# Status codes per step
GRIM_MATCH, GRIM_DIFF, GRIM_NO_GOLDEN, GRIM_FAIL = 0, 5, 6, 7
VNC_MATCH,  VNC_DIFF,  VNC_NO_GOLDEN,  VNC_FAIL, VNC_SKIPPED = 0, 5, 6, 7, 8


def read_exit(path: Path) -> int:
    try:
        return int(path.read_text().strip())
    except Exception:
        return -1


def classify(steps: dict) -> str:
    boot = steps["01-boot"]
    comp = steps["02-compositor"]
    grim = steps["03-grim"]
    vnc = steps["04-vnc"]
    gpu = steps["05-gpu"]
    way = steps["06-wayvnc-log"]

    if boot != 0:
        return "MIXED"
    if comp != 0:
        return "COMPOSITOR_NOT_READY"
    if gpu != 0:
        return "GPU_BROKEN"

    if grim == GRIM_NO_GOLDEN and vnc in (VNC_NO_GOLDEN, VNC_SKIPPED):
        return "NO_GOLDEN"

    grim_ok = grim == GRIM_MATCH
    vnc_ok = vnc in (VNC_MATCH, VNC_SKIPPED)
    grim_diff = grim == GRIM_DIFF
    vnc_diff = vnc == VNC_DIFF

    if grim_ok and vnc_ok and way == 0:
        return "GREEN"
    if grim_ok and vnc_diff:
        return "VNC_CLIENT_BUG"
    if grim_diff and vnc_diff:
        return "COMPOSITOR_REGRESSION"
    if grim_ok and vnc_ok and way != 0:
        return "WAYVNC_LOG_WARN"
    return "MIXED"


def read_leased_vm(run_dir: Path):
    vm_id_path = run_dir / "leased_vm.id"
    if not vm_id_path.exists():
        return None

    vm_id = vm_id_path.read_text().strip()
    if not vm_id:
        return None

    name_path = run_dir / "leased_vm.name"
    ip_path = run_dir / "leased_vm.ip"
    return {
        "id": int(vm_id),
        "name": name_path.read_text().strip() if name_path.exists() else f"vm-{vm_id}",
        "ip": ip_path.read_text().strip() if ip_path.exists() else f"10.0.100.{vm_id}",
    }


def render_summary(verdict, exit_code, steps, run_dir, workload, leased_vm):
    desc = VERDICTS[verdict][1]
    rows = []
    labels = {
        "01-boot": "Boot + workload",
        "02-compositor": "Compositor (sway IPC)",
        "03-grim": "grim capture vs golden",
        "04-vnc": "VNC capture vs golden",
        "05-gpu": "GPU stack",
        "06-wayvnc-log": "wayvnc log",
    }
    for key, label in labels.items():
        code = steps[key]
        rows.append(f"| {label:<24} | {code:>4} | {step_status(key, code)} |")

    next_doc = PHASE_MAPPING.get(verdict, "—")
    lines = [
        f"# Fixture diagnostic — {verdict}",
        "",
        f"_Workload:_ `{workload}`",
        f"_Run directory:_ `{run_dir}`",
        f"_Exit code:_ `{exit_code}`",
        f"_Leased VM:_ `{leased_vm['name']}` (`{leased_vm['ip']}`)" if leased_vm else "_Leased VM:_ unknown",
        "",
        f"**{desc}**",
        "",
        "| Step                     | Exit | Status |",
        "|--------------------------|------|--------|",
        *rows,
        "",
        f"**Next:** see `{next_doc}`" if next_doc != "—" else "**Next:** nothing — fixture is healthy.",
        "",
        "## Raw artefacts",
        "",
        f"- `01-boot.log` — boot trace",
        f"- `02-compositor.json` — sway `get_outputs` + `get_tree`",
        f"- `03-grim.png` — grim capture",
        f"- `03-grim.normalized.sha256` — RGB-only sha256",
        f"- `04-vnc.png` — VNC capture (if not skipped)",
        f"- `05-gpu.txt` — DRI / module / vulkaninfo / vkcube",
        f"- `06-wayvnc.log` — wayvnc tail",
    ]
    return "\n".join(lines) + "\n"


def step_status(key, code):
    if code == 0:
        return "OK"
    if key in ("03-grim", "04-vnc"):
        return {5: "DIFF", 6: "NO_GOLDEN", 7: "CAPTURE_FAILED", 8: "SKIPPED"}.get(code, f"ERR({code})")
    return f"FAIL({code})"


def main():
    run_dir = Path(os.environ["RUN_DIR"])
    workload = os.environ.get("WORKLOAD", "foot-banner")
    leased_vm = read_leased_vm(run_dir)
    steps = {
        "01-boot":       read_exit(run_dir / "01-boot.exit"),
        "02-compositor": read_exit(run_dir / "02-compositor.exit"),
        "03-grim":       read_exit(run_dir / "03-grim.exit"),
        "04-vnc":        read_exit(run_dir / "04-vnc.exit"),
        "05-gpu":        read_exit(run_dir / "05-gpu.exit"),
        "06-wayvnc-log": read_exit(run_dir / "06-wayvnc-log.exit"),
    }
    try:
        verdict = classify(steps)
    except Exception as e:
        verdict = "CLASSIFIER_BROKEN"
        (run_dir / "07-classify.error").write_text(repr(e))

    exit_code, desc = VERDICTS[verdict]
    payload = {
        "verdict": verdict,
        "exit_code": exit_code,
        "description": desc,
        "workload": workload,
        "leased_vm": leased_vm,
        "timestamp": int(time.time()),
        "steps": steps,
        "phase_mapping": {verdict: PHASE_MAPPING.get(verdict)} if verdict in PHASE_MAPPING else {},
    }
    (run_dir / "verdict.json").write_text(json.dumps(payload, indent=2) + "\n")
    (run_dir / "summary.md").write_text(
        render_summary(verdict, exit_code, steps, run_dir, workload, leased_vm)
    )

    print((run_dir / "summary.md").read_text())
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
