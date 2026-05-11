# Phase G — Capture lifecycle (timelapse / on-stall / video) and HTML report

> **Recommended Codex model: GPT-5.4 / medium effort**
>
> Moderate-complexity sub-agent role: connect already-built pieces
> (Phase A correct capture, Phase B readiness, Phase E validation,
> Phase F scenes) into the test harness's run loop and produce a
> human-readable report. The work is integration-heavy and UI-shaped;
> not algorithmically novel. 5.4 medium fits — promote to 5.4/high
> only if integrating with `test_history.json` reveals migration
> complexity not visible from outside.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on** Phase A
(capture correctness), Phase E (validation results to render), and
Phase F (scene-driven captures). Soft dependency on Phase C
(preflight green status appears in report). Land last in the
plan, or in parallel with F's tail end. Files touched overlap with
the harness's test runner — coordinate with anyone touching
`src/harness/runner.rs` concurrently.

## Goal

1. The test harness captures frames at meaningful checkpoints
   during a run, not only at terminal failure:
   - On every heartbeat-stall detection.
   - At fixed intervals when `--timelapse N` is set.
   - At each scene transition when `--scenes` is set (Phase F).
2. Optional video capture via `wf-recorder` for the full run (gated
   by `--record-video`).
3. A `cluster-ctl visual report [--run RUN_LABEL]` command emits a
   self-contained HTML report with:
   - Per-VM, per-scene cards showing actual / golden / diff side
     by side, with SSIM scores and pass/fail badges.
   - Timelapse strips per VM, per run.
   - Embedded video links (or `<video>` tags) when present.
   - Preflight summary (Phase C results), readiness summary
     (Phase B), and the test exit-code label.
4. `test_history.json` gains a typed `visual_results` field so
   `cluster-ctl history` can report visual pass/fail trends.

## Why this matters now

Currently `capture_on_failure`
([src/game/capture.rs:355-370](../../../../src/game/capture.rs#L355-L370))
fires once at terminal failure. For bugs that develop over a long
run (memory corruption, slow leaks, desync that manifests at
turn 30), a single end-of-run frame is uninformative. The
heartbeat-stall path already exists (30 s timeout in the harness;
see [README.md:592-602](../../../../README.md#L592-L602)) and is a
natural capture trigger.

The report is the consumer for everything Phases A–F produce. Without
it, triaging a failed visual test means opening PNGs by hand and
mentally diffing them. The HTML output should be the artefact
linked from CI runs and team chat.

## Out of scope

- Server-side / persistent report hosting. Reports are static HTML
  on disk; users `python -m http.server` or rsync them if they want
  remote viewing.
- A long-term metrics database. `test_history.json` extension is
  enough for now.
- Real-time live-update reports. The report is generated post-run.
- Cross-run comparison views in the HTML. One run per report.
  Aggregation can come later if useful.

## Plan

1. **Periodic capture hook.** Add an opt-in `CaptureLifecycle` to
   the test runner ([src/harness/runner.rs](../../../../src/harness/runner.rs)):
   ```
   enum CaptureLifecycle {
       Off,
       OnFailureOnly,                     // today's behavior
       OnFailureAndStall,                 // recommended default for --capture-on-failure
       Timelapse { interval_seconds: u32 },
       Scenes,                            // tied to --scenes from Phase F
   }
   ```
   Add CLI flag `--capture-mode <off|failure|failure-and-stall|timelapse>`,
   default `failure` to preserve current behavior.
2. **Wire to the stall detector.** Where the harness today aborts
   on heartbeat-stall (find this via `grep "NO_PROGRESS_TIMEOUT" -n`
   in src/harness), call `screenshot_all` for the configured VMs
   *before* killing them. Saves a frame at the moment of detection.
3. **Timelapse implementation.** Spawn a tokio task per run that
   ticks every `interval_seconds`, captures all VMs (cheap path:
   wayvnc + RFB; no need to restart compositor each tick), writes
   to `diff_dir/<run>/timelapse/<vm>/<seq>.png`. Number frames
   zero-padded so they sort naturally.
4. **Video capture.** Add `--record-video` flag. When set, before
   the run starts, SSH into each VM and start
   `wf-recorder -o <output> -f /tmp/run.webm` in the background.
   Package `wf-recorder` in
   [nix/cluster-vm-base.nix](../../../../nix/cluster-vm-base.nix).
   At run end (success or fail), kill wf-recorder, rsync the
   webm back to `diff_dir/<run>/video/<vm>.webm`. Tolerate
   wf-recorder absence (warn, skip).
5. **`test_history.json` extension.** Add `visual_results:
   Vec<VisualRunResult>` to the run record. Each entry carries
   scene name, vm name, ssim, max_delta, passed, artefact paths
   (relative to history root). Migration: read existing
   history files; absence of the field defaults to empty vec.
6. **`cluster-ctl visual report` command.** Use a templating
   crate (suggest `askama` or `minijinja`; pin one in
   `Cargo.toml`). Output a single `index.html` per run that
   embeds:
   - A summary header (run label, timestamp, exit code, exit
     label, preflight summary, network mode, players).
   - One section per VM. Inside each VM:
     - A scene grid: each scene is a card with actual/golden/diff
       thumbnails (lazy-loaded full size), SSIM score, badge.
     - A timelapse strip (CSS-only horizontal scroll).
     - A `<video controls>` if a video was recorded.
   - Footer: preflight raw JSON in a `<details>` block for
     debugging.
   Encode images as relative file links (the report is checked in
   or shared as a tarball); do not inline-base64 unless requested
   via `--single-file`.
7. **Theming and accessibility.** Use system-font CSS, no JS
   framework. The report should render in `lynx`/text browsers at
   least minimally (alt text on every image). Match the project's
   existing CLI aesthetic — terse, monospace tables, no emojis.
8. **CLI surface:**
   - `cluster-ctl visual report` — default to the most recent
     run.
   - `cluster-ctl visual report --run run-N` — specific run.
   - `cluster-ctl visual report --output PATH` — override
     destination (default `diff_dir/<run>/report/`).
   - `cluster-ctl visual report --single-file` — emit one
     self-contained HTML with base64 images.
9. **History command integration.**
   `cluster-ctl history` already exists
   ([src/harness/history.rs](../../../../src/harness/history.rs)).
   Add a `Visual` column to the table summarizing per-run visual
   pass/fail counts.
10. **Tests.** Render the template with a fixture
    `VisualRunResult` set; snapshot-test the HTML body
    (`insta` crate is a typical choice). Unit-test the
    timelapse tick scheduler with `tokio::time::pause()`. Unit-
    test the history migration (old → new shape preserves all
    fields, adds empty vec).

## Acceptance criteria

- [ ] `cargo test -p steampipe harness::runner::lifecycle` and
      `ui::report` — ≥ 8 tests including timelapse scheduling,
      stall-trigger capture, history migration, report
      template snapshot.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] `cluster-ctl test --capture-mode timelapse --capture-interval 5`
      against a 60-second test produces ≥ 10 timelapse frames per
      VM under `diff_dir/<run>/timelapse/vm-N/`.
- [ ] Forcing a heartbeat-stall (e.g. SIGSTOP the local game)
      produces a screenshot in `diff_dir/<run>/stall/vm-N/` named
      with the stall-detect timestamp.
- [ ] `cluster-ctl test --record-video` produces a playable webm
      per VM and the report embeds it.
- [ ] `cluster-ctl visual report` emits an `index.html` that opens
      in a browser, shows all sections, and contains no broken
      relative image links (`find diff_dir/<run>/report -name
      '*.html' | xargs -I{} … verify links`).
- [ ] `cluster-ctl history` shows a `Visual` column with
      `PASS / FAIL (n/m)` per row.
- [ ] Old `test_history.json` files (pre-this-phase) load and
      render without errors.

## Files likely touched

- [src/harness/runner.rs](../../../../src/harness/runner.rs) —
  `CaptureLifecycle`, hook into stall detection, timelapse task.
- [src/harness/history.rs](../../../../src/harness/history.rs) —
  `visual_results` field, migration.
- [src/game/capture.rs](../../../../src/game/capture.rs) — extend
  `capture_on_failure` with mode awareness; add `wf-recorder`
  start/stop helpers.
- [src/ui/cli.rs](../../../../src/ui/cli.rs) — `--capture-mode`,
  `--capture-interval`, `--record-video`, `visual report`
  subcommand.
- `src/ui/report/` (new directory: `mod.rs`, `template.html`,
  optional `style.css`).
- [src/main.rs](../../../../src/main.rs) — dispatch.
- [nix/cluster-vm-base.nix](../../../../nix/cluster-vm-base.nix) —
  add `wf-recorder`.
- [Cargo.toml](../../../../Cargo.toml) — template + snapshot crates.

## Pitfalls

- **Disk usage explodes.** 5-second timelapse × 60-second run × 7
  VMs × 1.5 MB PNG = ~125 MB *per run*. Add per-run total-size
  budget warning at 500 MB; document a `--max-storage` cap or
  recommend retention pruning.
- **wf-recorder needs the wlroots screencopy interface.** If the
  compositor in the VM is misconfigured (sway without
  `wlr-screencopy`), wf-recorder silently writes a 0-byte file.
  Detect via post-run filesize check; treat 0 bytes as a missing
  artefact, not a successful video. Log a hint.
- **Timelapse and on-failure capture can collide.** If a failure
  occurs during a timelapse tick, two captures race for wayvnc.
  Serialise per-VM with a tokio Mutex. Symptom: one capture
  returns garbage. Recovery: per-VM mutex on the screenshot path.
- **`test_history.json` migrations are forever.** Adding fields is
  safe with `#[serde(default)]`; renaming or removing fields
  breaks old runs. Treat the file as append-only schema; bump
  a `version` field if you must break compatibility.
- **HTML reports with absolute paths break when shared.** Always
  produce relative `<img src>` URLs from the report's directory.
  Test by `tar`ing the report dir, extracting elsewhere, opening.
- **Snapshot tests are noisy.** `insta` snapshot diffs on every
  whitespace change. Use `insta::assert_yaml_snapshot!` on
  structured data (the report's data model) plus a *single*
  HTML snapshot for the rendered shape. Don't snapshot every
  generated file.
- **Heartbeat stall detection lives in the harness runner.** Find
  it before touching the rest of this phase — the trigger point
  is the linchpin of the "on-stall capture" feature, and if you
  guess at where it lives you'll miss it.

## Reference

- Originating diagnosis: chat overview of 2026-05-11 — gaps #7
  (capture lifecycle) and #9 (reporting).
- Existing fail-capture flow:
  [src/game/capture.rs:355-370](../../../../src/game/capture.rs#L355-L370).
- Existing history:
  [src/harness/history.rs](../../../../src/harness/history.rs) and
  the `cluster-ctl history` command at
  [README.md:605-631](../../../../README.md#L605-L631).
- Heartbeat protocol (the stall trigger we hook):
  [README.md:592-602](../../../../README.md#L592-L602).
- Related phases: A (capture correctness — must land first), E
  (data source for reports), F (scene transitions as capture
  triggers).
