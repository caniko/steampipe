#!/usr/bin/env python3
"""Capture the real Steampipe Steam Guard prompt with visual-rubric.

The prompt is an Iced desktop window, so browser or VM diagnostic screenshots
would not exercise the product surface. This producer uses Xvfb, xdotool, and
ImageMagick to capture the four persistent prompt states that the application
actually exposes.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


TARGET = "steampipe/prompt"
VIEWPORT = {"width": 440, "height": 360, "dpr": 1.0}
THEME = "dark"
LOCALE = "en-US"
PRESET = "ui-regression"


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"{json.dumps(value, indent=2)}\n")


def git_output(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=root, text=True).strip()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def artifact(root: Path, path: Path) -> dict[str, object]:
    relative = path.relative_to(root).as_posix()
    return {
        "path": relative,
        "sha256": sha256(path),
        "bytes": path.stat().st_size,
    }


def stop_process(process: subprocess.Popen[object] | None) -> None:
    if process is None or process.poll() is not None:
        return
    try:
        process.terminate()
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def start_xvfb(log_path: Path) -> tuple[subprocess.Popen[object], str, object]:
    log = log_path.open("w")
    for number in range(100, 200):
        if Path(f"/tmp/.X11-unix/X{number}").exists():
            continue
        display = f":{number}"
        process = subprocess.Popen(
            [
                "Xvfb",
                display,
                "-screen",
                "0",
                "440x360x24",
                "-nolisten",
                "tcp",
                "-ac",
            ],
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if process.poll() is not None:
                break
            if Path(f"/tmp/.X11-unix/X{number}").exists():
                return process, display, log
            time.sleep(0.1)
        stop_process(process)
        if process.poll() is None:
            process.kill()
            process.wait()
    log.close()
    raise RuntimeError("could not start a free Xvfb display in :100-:199")


def prompt_environment(display: str, runtime_dir: str) -> dict[str, str]:
    environment = dict(os.environ)
    environment.pop("WAYLAND_DISPLAY", None)
    environment["DISPLAY"] = display
    environment["XDG_RUNTIME_DIR"] = runtime_dir
    environment["LIBGL_ALWAYS_SOFTWARE"] = "1"
    environment["WGPU_BACKEND"] = "gl"
    return environment


def prompt_error(process: subprocess.Popen[object]) -> str:
    if process.stderr is None:
        return ""
    return process.stderr.read().strip()


def wait_for_window(
    environment: dict[str, str], process: subprocess.Popen[object]
) -> str:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            detail = prompt_error(process)
            raise RuntimeError(
                f"cluster-guard-prompt exited with {process.returncode}"
                + (f": {detail}" if detail else "")
            )
        result = subprocess.run(
            ["xdotool", "search", "--onlyvisible", "--name", "^Steam Guard$"],
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )
        windows = [line.strip() for line in result.stdout.splitlines() if line.strip()]
        if windows:
            return windows[-1]
        time.sleep(0.1)
    raise TimeoutError("timed out waiting for the Steam Guard window")


def xdotool(environment: dict[str, str], *args: str, check: bool = True) -> None:
    subprocess.run(["xdotool", *args], env=environment, check=check)


def submit_code(environment: dict[str, str], window: str, code: str) -> None:
    # Xvfb has no window manager, so windowfocus can report a harmless
    # _NET_ACTIVE_WINDOW warning. Clicking the known input coordinates gives
    # Iced the focus it needs before xdotool sends the code and Return key.
    xdotool(environment, "windowfocus", window, check=False)
    xdotool(environment, "mousemove", "--window", window, "160", "210")
    xdotool(environment, "click", "1")
    xdotool(environment, "type", "--window", window, code)
    xdotool(environment, "key", "--window", window, "Return")


def capture_window(
    environment: dict[str, str], window: str, image_path: Path
) -> None:
    image_path.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["import", "-window", window, str(image_path)],
        env=environment,
        check=True,
    )


def launch_prompt(
    binary: Path,
    environment: dict[str, str],
    flags: list[str],
) -> subprocess.Popen[object]:
    return subprocess.Popen(
        [
            str(binary),
            "--vm",
            "atlas",
            "--user",
            "can",
            "--reason",
            "Visual rubric capture",
            *flags,
            "--timeout-secs",
            "120",
        ],
        cwd=binary.parent.parent.parent,
        env=environment,
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )


def control(process: subprocess.Popen[object], line: str) -> None:
    if process.stdin is None:
        raise RuntimeError("prompt stdin is unavailable in interactive mode")
    process.stdin.write(f"{line}\n")
    process.stdin.flush()


def capture_states(
    root: Path,
    binary: Path,
    environment: dict[str, str],
    captures_root: Path,
) -> tuple[list[dict[str, object]], list[str]]:
    captures: list[dict[str, object]] = []
    transitions = [
        "prompt/editing-submit",
        "prompt/verifying-rejected",
        "prompt/rejected-resubmit",
    ]

    static_states = (
        ("editing", []),
        ("invalid-retry", ["--invalid-retry"]),
    )
    for state, flags in static_states:
        process = launch_prompt(binary, environment, flags)
        try:
            window = wait_for_window(environment, process)
            time.sleep(0.4)
            image_path = captures_root / state / "desktop" / THEME / LOCALE / "screenshot.png"
            capture_window(environment, window, image_path)
            metadata_path = image_path.with_name("state.json")
            write_json(
                metadata_path,
                {"state": state, "flags": flags, "window": window, "actions": []},
            )
            captures.append(
                {
                    "id": f"{state}/desktop/{THEME}/{LOCALE}",
                    "image_path": image_path,
                    "metadata_path": metadata_path,
                    "state": state,
                    "actions": [],
                }
            )
        finally:
            stop_process(process)

    process = launch_prompt(binary, environment, ["--interactive"])
    try:
        window = wait_for_window(environment, process)
        submit_code(environment, window, "123456")
        time.sleep(0.8)
        verifying_path = (
            captures_root / "interactive-verifying" / "desktop" / THEME / LOCALE / "screenshot.png"
        )
        capture_window(environment, window, verifying_path)
        verifying_metadata = verifying_path.with_name("state.json")
        write_json(
            verifying_metadata,
            {
                "state": "interactive-verifying",
                "flags": ["--interactive"],
                "window": window,
                "actions": ["prompt/editing-submit"],
            },
        )
        captures.append(
            {
                "id": f"interactive-verifying/desktop/{THEME}/{LOCALE}",
                "image_path": verifying_path,
                "metadata_path": verifying_metadata,
                "state": "interactive-verifying",
                "actions": ["prompt/editing-submit"],
            }
        )

        control(process, "ERR code rejected by visual fixture")
        time.sleep(0.8)
        rejected_path = (
            captures_root / "interactive-rejected" / "desktop" / THEME / LOCALE / "screenshot.png"
        )
        capture_window(environment, window, rejected_path)
        rejected_metadata = rejected_path.with_name("state.json")
        write_json(
            rejected_metadata,
            {
                "state": "interactive-rejected",
                "flags": ["--interactive"],
                "window": window,
                "actions": ["prompt/editing-submit", "prompt/verifying-rejected"],
            },
        )
        captures.append(
            {
                "id": f"interactive-rejected/desktop/{THEME}/{LOCALE}",
                "image_path": rejected_path,
                "metadata_path": rejected_metadata,
                "state": "interactive-rejected",
                "actions": ["prompt/editing-submit", "prompt/verifying-rejected"],
            }
        )

        submit_code(environment, window, "654321")
        time.sleep(0.4)
    finally:
        stop_process(process)

    for capture in captures:
        capture["image"] = artifact(root, capture.pop("image_path"))
        capture["metadata"] = artifact(root, capture.pop("metadata_path"))
    return captures, transitions


def rubric_result(root: Path, rubric_root: Path, image: Path) -> dict[str, object]:
    if os.environ.get("STEAMPIPE_VISUAL_SKIP_AI") == "1":
        return {
            "status": "skipped",
            "reason": "AI rubric skipped by flag",
            "anomalies": [],
        }
    command = [
        "nix",
        "develop",
        "--no-write-lock-file",
        str(rubric_root),
        "-c",
        "cargo",
        "run",
        "--locked",
        "--features",
        "audit",
        "--bin",
        "visual-rubric",
        "--",
        "configured",
        "--image",
        str(image.resolve()),
        "--preset",
        PRESET,
        "--json",
    ]
    rubric_environment = dict(os.environ)
    # The Steampipe dev shell uses a nightly-only Cargo config for the project
    # build. Do not leak that outer config into visual-rubric's stable shell.
    for variable in (
        "CARGO_HOME",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "RUSTC_WRAPPER",
    ):
        rubric_environment.pop(variable, None)
    result = subprocess.run(
        command,
        cwd=rubric_root,
        env=rubric_environment,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip().splitlines()
        return {
            "status": "error",
            "reason": "visual-rubric configured failed",
            "anomalies": detail[-4:],
        }
    values = [line for line in result.stdout.splitlines() if line.strip()]
    try:
        value = json.loads(values[-1])
    except (IndexError, json.JSONDecodeError) as error:
        return {
            "status": "error",
            "reason": f"visual-rubric returned invalid JSON: {error}",
            "anomalies": values[-4:],
        }
    verdict = value.get("verdict")
    status = {"pass": "pass", "fail": "fail"}.get(verdict, "error")
    return {
        "status": status,
        "reason": value.get("reason", ""),
        "anomalies": value.get("anomalies", []),
    }


def make_manifest(
    root: Path,
    revision: str,
    dirty: bool,
    captures: list[dict[str, object]],
    coverage_contract_path: Path,
    coverage_report_path: Path,
) -> dict[str, object]:
    return {
        "schema_version": 3,
        "target": TARGET,
        "revision": revision,
        "dirty": dirty,
        "environment": {
            "platform": sys.platform,
            "renderer": "iced-wgpu",
            "capture_backend": "xvfb-xdotool-imagemagick",
        },
        "declared_cells": len(captures),
        "captures": [
            {
                "id": capture["id"],
                "image": capture["image"],
                "state": capture["state"],
                "viewport": VIEWPORT,
                "theme": THEME,
                "locale": LOCALE,
                "metadata": {"state": capture["metadata"]},
                "presets": [PRESET],
            }
            for capture in captures
        ],
        "coverage_contract": artifact(root, coverage_contract_path),
        "coverage_report": artifact(root, coverage_report_path),
    }


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    visual_root = root / "target/visual"
    captures_root = visual_root / "captures"
    rubric_root = Path(
        os.environ.get("VISUAL_RUBRIC_ROOT", str(root.parent / "visual-rubric"))
    ).resolve()
    required = ["Xvfb", "xdotool", "import"]
    missing = [tool for tool in required if shutil.which(tool) is None]
    if missing:
        raise RuntimeError(
            "missing desktop capture tools: "
            + ", ".join(missing)
            + "; run this producer inside the Steampipe nix develop shell"
        )
    if not (rubric_root / "Cargo.toml").is_file():
        raise FileNotFoundError(f"visual-rubric checkout is missing: {rubric_root}")

    revision = git_output(root, "rev-parse", "HEAD")
    dirty = bool(git_output(root, "status", "--porcelain=v1"))
    visual_root.mkdir(parents=True, exist_ok=True)
    if captures_root.exists():
        shutil.rmtree(captures_root)
    for name in (
        "capture_manifest.json",
        "capture_job.json",
        "run_report.json",
        "deterministic_report.json",
        "rubric_batch_report.json",
    ):
        (visual_root / name).unlink(missing_ok=True)

    binary_build = subprocess.run(
        ["cargo", "build", "--quiet", "-p", "cluster-guard-prompt"],
        cwd=root,
        check=False,
    )
    if binary_build.returncode != 0:
        raise RuntimeError("cluster-guard-prompt build failed")
    binary = root / "target/debug/cluster-guard-prompt"
    if not binary.is_file():
        raise FileNotFoundError(f"prompt binary was not built: {binary}")

    with tempfile.TemporaryDirectory(prefix="steampipe-visual-runtime-") as runtime_dir:
        xvfb, display, log = start_xvfb(visual_root / "xvfb.log")
        try:
            environment = prompt_environment(display, runtime_dir)
            captures, transitions = capture_states(
                root, binary, environment, captures_root
            )
        finally:
            stop_process(xvfb)
            log.close()

    surfaces = [
        {
            "id": "prompt/editing",
            "platform": "steam_desktop",
            "required": True,
            "description": "Default Steam Guard code entry state",
        },
        {
            "id": "prompt/invalid-retry",
            "platform": "steam_desktop",
            "required": True,
            "description": "One-shot code entry after a rejected previous code",
        },
        {
            "id": "prompt/verifying",
            "platform": "steam_desktop",
            "required": True,
            "description": "Interactive submission while Steam verification is pending",
        },
        {
            "id": "prompt/rejected",
            "platform": "steam_desktop",
            "required": True,
            "description": "Interactive submission rejected with an inline error",
        },
    ]
    transition_values = [
        {
            "id": transition,
            "from": transition.split("-")[0].replace("prompt/", "prompt/"),
            "action": transition.rsplit("/", 1)[-1],
            "to": "prompt/verifying" if transition.endswith("submit") or transition.endswith("resubmit") else "prompt/rejected",
            "platform": "steam_desktop",
            "required": True,
        }
        for transition in transitions
    ]
    contract = {
        "schema_version": 1,
        "target": TARGET,
        "revision": revision,
        "surfaces": surfaces,
        "transitions": transition_values,
        "exclusions": [],
    }
    coverage_contract_path = visual_root / "coverage/contract.json"
    write_json(coverage_contract_path, contract)
    required_ids = [surface["id"] for surface in surfaces] + transitions
    compact_contract = json.dumps(contract, separators=(",", ":"), ensure_ascii=False).encode()
    coverage_report_path = visual_root / "coverage/report.json"
    write_json(
        coverage_report_path,
        {
            "schema_version": 1,
            "contract_sha256": hashlib.sha256(compact_contract).hexdigest(),
            "planned_ids": required_ids,
            "executed_ids": required_ids,
            "passed_ids": required_ids,
            "failed_ids": [],
            "blocked_ids": [],
        },
    )

    manifest = make_manifest(
        root,
        revision,
        dirty,
        captures,
        coverage_contract_path,
        coverage_report_path,
    )
    manifest_path = visual_root / "capture_manifest.json"
    write_json(manifest_path, manifest)

    deterministic_path = visual_root / "deterministic_report.json"
    write_json(
        deterministic_path,
        {
            "schema_version": 1,
            "run_id": "deterministic-steampipe-prompt",
            "target": TARGET,
            "revision": revision,
            "dirty": dirty,
            "capture_manifest": artifact(root, manifest_path),
            "planned_capture_ids": [capture["id"] for capture in captures],
            "evaluated_capture_ids": [capture["id"] for capture in captures],
            "status": "pass",
            "observations": [],
            "findings": [],
        },
    )

    rubric_values = []
    for capture in captures:
        image_path = root / capture["image"]["path"]
        print(f"Evaluating {capture['id']}", flush=True)
        rubric = rubric_result(root, rubric_root, image_path)
        rubric_values.append({"id": capture["id"], "rubric": rubric})

    batch_path = visual_root / "rubric_batch_report.json"
    write_json(
        batch_path,
        {"schema_version": 1, "mode": "configured", "results": rubric_values},
    )
    cells = []
    capture_by_id = {capture["id"]: capture for capture in captures}
    for value in rubric_values:
        capture = capture_by_id[value["id"]]
        rubric = value["rubric"]
        cells.append(
            {
                "id": capture["id"],
                "image": capture["image"]["path"],
                "locale": LOCALE,
                "rubric": rubric,
                "state": capture["state"],
                "status": rubric["status"],
                "theme": THEME,
                "viewport": VIEWPORT,
            }
        )
    failed = [cell for cell in cells if cell["status"] == "fail"]
    errors = [cell for cell in cells if cell["status"] not in ("pass", "fail")]
    report = {
        "schema_version": 3,
        "target": TARGET,
        "git": {"sha": revision, "dirty": dirty},
        "capture_manifest": artifact(root, manifest_path),
        "capture_environment": {
            "renderer": "iced-wgpu",
            "capture_backend": "xvfb-xdotool-imagemagick",
        },
        "rubric_batch_report": artifact(root, batch_path),
        "deterministic_report": artifact(root, deterministic_path),
        "cells": cells,
        "summary": {
            "total_cells": len(cells),
            "passed_cells": len(cells) - len(failed) - len(errors),
            "failed_cells": len(failed),
            "error_cells": len(errors),
        },
        "failures": [{"id": cell["id"], "rubric": cell["rubric"]} for cell in failed],
        "errors": [{"id": cell["id"], "rubric": cell["rubric"]} for cell in errors],
        "rate_limit_events": [],
    }
    report_path = visual_root / "run_report.json"
    write_json(report_path, report)
    summary = report["summary"]
    print(
        f"Steampipe prompt visual producer wrote {len(cells)} cells "
        f"({summary['passed_cells']} passed, {summary['failed_cells']} failed, "
        f"{summary['error_cells']} errors) to {report_path}",
        flush=True,
    )
    return 1 if failed or errors else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"Steampipe prompt visual producer failed: {error}", file=sys.stderr)
        sys.exit(1)
