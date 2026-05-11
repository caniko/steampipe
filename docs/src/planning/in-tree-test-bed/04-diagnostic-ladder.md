# Phase D — Diagnostic ladder automation against the fixture

> **Recommended Codex model: GPT-5.4 / medium effort**
>
> Moderate-complexity sub-agent role: write `just` recipes and a
> driver script that runs the six-step diagnostic ladder
> (compositor IPC → grim → VNC → GPU info → byte inspection → wayvnc
> log) against the in-tree fixture, capturing structured output that
> classifies failures into phase-A/B/C/downstream buckets. Output
> formatting and per-step parsing benefit from real reasoning, not
> mechanical translation — 5.4/medium fits. Lower tiers tend to skip
> the failure-classification logic and leave operators staring at
> raw logs.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Depends on Phase A (fixture
boots), Phase B (workloads run), and Phase C (goldens to diff
against). Touches the top-level [justfile](../../../../justfile),
adds a driver script under `tests/fixture/diagnose/`, and writes
its output into `tests/fixture/diagnose/_run-<timestamp>/` which
must be gitignored.

## Goal

A developer can run one command — `just diag-fixture` or
`tests/fixture/diagnose/run.sh` — and get within ~3 minutes:

- A `_run-<ts>/summary.md` that classifies the current cluster
  state into one of these verdicts:
  - `GREEN`: all six ladder steps pass, captures match goldens
    within trivial tolerance.
  - `VNC_CLIENT_BUG`: grim captures match goldens, VNC captures
    are corrupt → maps to `vnc-visual-testing` Phase A.
  - `COMPOSITOR_NOT_READY`: sway IPC reports no outputs or no
    workload window → maps to Phase B.
  - `GPU_BROKEN`: no Vulkan device / vkcube fails / no DRI nodes
    → maps to Phase C.
  - `MIXED`: multiple categories tripped — the summary names
    each.
- Per-step raw outputs (`02-compositor.json`, `03-grim.png`,
  `04-vnc.png`, `05-gpu.txt`, `06-wayvnc.log`, etc.) so the
  failure is auditable, not just classified.
- An exit code that mirrors the verdict: 0 for `GREEN`, non-zero
  for every other category, with distinct codes per category so
  CI / scripts can branch on them.

## Why this matters now

The diagnostic ladder I sketched in chat
(steps 1–6: boot → compositor → grim → VNC → GPU → wayvnc log) is
useful, but only if it's *fast to run* and *consistent to
interpret*. Running it by hand:

- Takes ~15 minutes per attempt.
- Requires the operator to remember six SSH commands and a Python
  one-liner.
- Produces "vibes" verdicts that don't survive being passed to
  another contributor.

Automating it costs ~half a day and pays back the first time
anyone needs to compare two cluster boots — which will be every
commit during the `vnc-visual-testing` plan's execution. It also
sets up the data shape for Phase G of `vnc-visual-testing` (the
HTML report consumer), so this phase's output JSON should
anticipate that.

## Out of scope

- Replacing `cluster-ctl doctor`. That command exists for cluster
  *configuration* health; this script targets *graphics path*
  health. Keep them separate.
- Per-VM detail beyond `vm-1`. The fixture is single-VM by design.
- Long-term trend storage. Each run writes to its own timestamped
  directory; aggregating across runs is Phase G of
  `vnc-visual-testing`.
- Implementing the visual SSIM comparator. Use a shallow
  pixel-equality + size check here, not real perceptual diff.
  Real diff is Phase E of `vnc-visual-testing`.
- Modifying `cluster-ctl` itself. This phase is pure shell + Python
  + a thin `just` recipe layer.

## Plan

1. **Decide the directory layout** for diagnostic runs:
   ```
   tests/fixture/diagnose/
   ├── run.sh                     # entry point
   ├── steps/
   │   ├── 01-boot.sh
   │   ├── 02-compositor.sh
   │   ├── 03-grim.sh
   │   ├── 04-vnc.sh
   │   ├── 05-gpu.sh
   │   ├── 06-wayvnc-log.sh
   │   └── 07-classify.py
   └── _run-<timestamp>/          # gitignored; one dir per run
       ├── summary.md
       ├── verdict.json
       ├── 02-compositor.json
       ├── 03-grim.png
       ├── 04-vnc.png
       ├── 05-gpu.txt
       ├── 06-wayvnc.log
       └── 07-classify.log
   ```
2. **`01-boot.sh`**: idempotently bring the fixture up. If
   `cluster-ctl status` shows VM up, skip. Otherwise `net-up`
   (if needed) + `cluster-1v1-up`. Then `workload-run
   foot-banner` (the cheapest workload to exercise the full
   stack). Wait for SSH ready. Emit a one-line OK to stdout.
3. **`02-compositor.sh`**: SSH in, run:
   ```bash
   export XDG_RUNTIME_DIR=/tmp/runtime-fixture
   swaymsg -t get_outputs
   swaymsg -t get_tree
   ```
   Save raw JSON. Set step exit code based on whether ≥1 output
   is active and a node titled `workload-foot-banner` exists.
4. **`03-grim.sh`**: SSH in, run `grim /tmp/grim.png`, rsync
   back. Compute sha256 of the captured frame stripped of
   metadata (`oxipng --strip all`). Compare to
   `tests/fixture/golden/foot-banner-1280x720.png`'s same-
   normalized hash. Equal → step OK; differ → step DIFF.
5. **`04-vnc.sh`**: run `cluster-ctl --project-root tests/fixture
   --vm-count 1 screenshot --screenshot-backend vnc -o
   _run-<ts>/`. Copy resulting `vm-1.png` to `04-vnc.png`.
   Compare to the same golden as step 3. Same OK / DIFF logic.
6. **`05-gpu.sh`**: SSH in, run the consolidated GPU probe from
   the diagnostic ladder (DRI nodes, virtio_gpu module, vulkaninfo,
   wayland-info, vkcube --c 1). Save concatenated output to
   `05-gpu.txt`. Parse for "no devices", "no swapchain", "VKCUBE_OK"
   markers to set step status.
7. **`06-wayvnc-log.sh`**: SSH in, `tail -n 200
   $XDG_RUNTIME_DIR/wayvnc.log`. Save raw. Parse for known error
   substrings (`screencopy`, `failed to start encoder`, `dmabuf`)
   to set step status.
8. **`07-classify.py`**: read all step exit codes + the two
   "image matches golden?" results, emit `verdict.json`:
   ```json
   {
     "verdict": "VNC_CLIENT_BUG",
     "exit_code": 11,
     "steps": {
       "01-boot": "OK",
       "02-compositor": "OK",
       "03-grim": "MATCH",
       "04-vnc": "DIFF",
       "05-gpu": "OK",
       "06-wayvnc-log": "OK"
     },
     "phase_mapping": {
       "VNC_CLIENT_BUG": "vnc-visual-testing/01-vnc-client-correctness"
     }
   }
   ```
   Verdict logic (in order — first match wins):
   - all OK + 03-MATCH + 04-MATCH → `GREEN` (exit 0)
   - 02 fails → `COMPOSITOR_NOT_READY` (exit 12)
   - 05 fails OR vkcube fail → `GPU_BROKEN` (exit 13)
   - 03 MATCH + 04 DIFF → `VNC_CLIENT_BUG` (exit 11)
   - 03 DIFF (with 02/05 OK) → `COMPOSITOR_REGRESSION` (exit 14)
     — workload painted but content differs from golden
   - everything else → `MIXED` (exit 15)
9. **`07-classify.py`** also writes `summary.md`:
   one screen of plain markdown — the verdict in big text, then
   a table of step results, then "next step": which phase doc
   to open, and the SSH commands to drill deeper.
10. **`run.sh`** orchestrates 01 → 06 sequentially, runs 07,
    prints `summary.md` to stdout, exits with the verdict's exit
    code. Accepts `--keep-up` to skip teardown, `--workload
    NAME` to substitute another workload (defaults to
    foot-banner), `--no-vnc` to skip step 4 (useful while Phase A
    of vnc-visual-testing is in progress and VNC is known
    broken).
11. **`justfile` recipes** at the repo root:
    ```
    diag-fixture *ARGS:
        bash tests/fixture/diagnose/run.sh {{ARGS}}

    diag-fixture-keep *ARGS:
        bash tests/fixture/diagnose/run.sh --keep-up {{ARGS}}
    ```
12. **Gitignore.** Add `tests/fixture/diagnose/_run-*` to
    `.gitignore`. Add a sample `_run-<ts>/` in
    `tests/fixture/diagnose/example-run/` (committed) showing
    what a `GREEN` run looks like — handy for new contributors.

## Acceptance criteria

- [ ] `just diag-fixture` runs end-to-end in under 4 minutes on a
      warm cache; exits 0 with `verdict.json.verdict == "GREEN"`
      on a healthy fixture.
- [ ] Each of the 5 non-`GREEN` verdicts is reachable by
      deliberate sabotage: simulate by
      - stopping wayvnc → triggers VNC_CLIENT_BUG only if grim
        works (or COMPOSITOR_NOT_READY if it doesn't — the
        verdict tree must handle both),
      - `workload-stop` before capture → `COMPOSITOR_NOT_READY`,
      - removing `/dev/dri/card0` (`sudo chmod 000` then revert)
        → `GPU_BROKEN`,
      - replacing a golden with garbage → `COMPOSITOR_REGRESSION`,
      - combinations → `MIXED`.
      Document each reproduction recipe in a `## Verdict
      sanity-check` section of `tests/fixture/diagnose/README.md`.
- [ ] All raw outputs (`02-…json`, `03-…png`, `04-…png`,
      `05-…txt`, `06-…log`) are produced and non-empty on every
      successful run.
- [ ] `summary.md` is ≤ 80 lines and readable as plain text in a
      terminal.
- [ ] `verdict.json` is valid JSON and includes the
      `phase_mapping` pointer to the appropriate
      `vnc-visual-testing/NN-…md` file.
- [ ] `just diag-fixture-keep` leaves the cluster up and SSH-able
      after exit.
- [ ] Two consecutive `just diag-fixture` runs on an unchanged
      tree produce the same verdict (modulo wall-clock fields in
      the JSON).
- [ ] `tests/fixture/diagnose/example-run/` contains a committed
      `summary.md` + `verdict.json` showing a real `GREEN` run.
- [ ] `.gitignore` excludes `tests/fixture/diagnose/_run-*` but
      NOT `example-run/`.
- [ ] `cargo test --workspace` still green;
      `nix flake check ./tests/fixture` still green.

## Files likely touched

- `tests/fixture/diagnose/run.sh` (new).
- `tests/fixture/diagnose/steps/01-boot.sh` (new).
- `tests/fixture/diagnose/steps/02-compositor.sh` (new).
- `tests/fixture/diagnose/steps/03-grim.sh` (new).
- `tests/fixture/diagnose/steps/04-vnc.sh` (new).
- `tests/fixture/diagnose/steps/05-gpu.sh` (new).
- `tests/fixture/diagnose/steps/06-wayvnc-log.sh` (new).
- `tests/fixture/diagnose/steps/07-classify.py` (new).
- `tests/fixture/diagnose/README.md` (new).
- `tests/fixture/diagnose/example-run/{summary.md,verdict.json,...}`
  (new — committed sample of a GREEN run).
- [justfile](../../../../justfile) — extend with `diag-fixture`
  and `diag-fixture-keep`.
- `.gitignore` — exclude `_run-*`.

## Pitfalls

- **Step ordering matters.** If `04-vnc.sh` runs before sway is
  ready, you classify a compositor problem as a VNC problem.
  `01-boot.sh` must wait for SSH ready *and* sleep an extra ~3 s
  for sway to paint. Replace the sleep with a sway-IPC ready
  check once Phase B of `vnc-visual-testing` lands.
- **The classifier swallowing errors.** If `07-classify.py`
  crashes, no verdict is produced. Wrap with a top-level try
  and emit a `verdict: "CLASSIFIER_BROKEN"` instead of failing
  silently.
- **VNC step requires wayvnc-running invariant.** If wayvnc is
  not yet started by step 4, the script either has to start it
  (matching `ensure_wayvnc` from
  [src/game/capture.rs](../../../../src/game/capture.rs)) or
  rely on `cluster-ctl screenshot` to do so internally. Pick
  one and document it.
- **Golden hash comparison is fragile.** `grim` captures vary
  by mesa version, font hinting, etc. Until Phase E of
  `vnc-visual-testing` provides SSIM, you'll get false-positive
  DIFFs across mesa bumps. Mitigate by treating step-3 DIFF
  with all other steps green as a *warning* (verdict
  `COMPOSITOR_REGRESSION`), not a hard failure. Operators
  decide whether to bless a new golden.
- **`sudo chmod 000 /dev/dri/card0` in the GPU_BROKEN
  reproduction does not actually break the VM** if the file is
  inside the microvm. Test the sabotage recipe end-to-end
  before claiming acceptance criteria; some "obvious"
  sabotages don't reproduce the failure mode you want.
- **Concurrent runs collide.** Two `just diag-fixture`
  invocations on the same machine try to use the same bridge
  and PID files. Document that diag-fixture is single-instance;
  consider acquiring a flock to enforce it.
- **The classifier's mapping table needs to track upstream
  doc moves.** If a `vnc-visual-testing/NN-…md` file gets
  renamed, the `phase_mapping` field points at /dev/null.
  Either keep the mapping inside the upstream doc set (each
  phase doc declares its slug for classifier consumers) or
  accept that the mapping drifts and gets repaired during
  reviews.

## Reference

- Originating context: in-chat 2026-05-11 — the six-step ladder
  ("Is the compositor actually painted? / Try grim / GPU/Vulkan
  state / Inspect the actual noise / Wayvnc-side sanity") was
  laid out as a manual procedure; this phase automates it.
- Failure-mode taxonomy mirror:
  [../../migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md).
- Downstream plan whose phases this script's verdicts point at:
  [../vnc-visual-testing/](../vnc-visual-testing/README.md).
- Related phases in *this* plan: A (boots the fixture), B
  (provides the workload step 1 launches), C (provides the
  golden steps 3 and 4 compare against). All three are
  prerequisites — this phase is the terminal one in this plan.
