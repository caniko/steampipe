# Phase F — Scripted scene navigation with sync checkpoints

> **Recommended Codex model: GPT-5.4 / high effort**
>
> Complex sub-agent work: synchronizing remote input (sway IPC + wtype),
> readiness primitives from Phase B, and capture/validation from Phase
> E into a deterministic scene-walker. The complexity comes from
> timing — every action must pair with a "wait until effect observable"
> predicate, otherwise scripts are flaky. 5.4 at high gives the
> reasoning headroom for the timing logic at half the cost of 5.5
> medium; 5.5 medium is a defensible alternative if you find the
> error-recovery paths multiplying beyond expectations.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase B** for
`wait_until_ready` / `WindowTarget`. Soft dependency on Phase D for
the `Scene` schema (this phase extends it with action lists). Files
touched do not overlap with phases A, C, D — only B (compositor.rs)
and possibly E (capture entry). Plan to land after B and D; can
land in parallel with E if E's `compare` API has stabilized.

## Goal

A YAML/TOML-described scene script can drive a VM from a known
state (e.g. "Steam main menu") to a target scene (e.g. "in-match
turn-1 prompt") with verifiable sync points between each step.
`cluster-ctl visual capture <scene>` and the test harness can call
`run_scene(vm, scene)` which performs:

1. Resolve scene actions (clicks, keystrokes, waits) from config.
2. Execute each action remotely.
3. After each action, poll a sync predicate (window appeared,
   text node visible, file appeared on VM, fixed delay) until met
   or timeout.
4. Once all actions complete, return control to caller for capture.

A failed sync emits a structured error identifying the step and
the predicate that did not hold.

## Why this matters now

Without scripted navigation, golden captures from Phase E only
cover the "first idle frame after boot" — the Steam wallpaper.
Catching real game-state regressions requires reaching specific
scenes deterministically. wayvnc supports input today
([src/game/capture.rs:66](../../../../src/game/capture.rs#L66)
disables it with `--disable-input` — Phase F re-enables it under
controlled conditions); `wtype` is already shipped in the VM image
([nix/cluster-vm-base.nix:120](../../../../nix/cluster-vm-base.nix#L120))
but only used by the interactive `steam-login` flow.

This phase also makes Phase G's timelapse meaningful: the harness
can capture at deterministic scene transitions instead of arbitrary
wall-clock intervals, producing comparable timelines across runs.

## Out of scope

- Mouse-pixel-precise interaction across resolutions. We support
  `wtype` keystrokes, `swaymsg exec` to focus windows, and
  `ydotool`-style coordinate input *only* under fixed resolution
  scenes. Variable-resolution mouse driving is a future extension.
- Recording scripts by demonstration. Scripts are authored by
  hand in TOML for this phase. A "record" mode could come later.
- Cross-VM coordinated scripts (one VM joins lobby created by
  another). The scene runs against a single VM. Multi-VM
  orchestration stays in the test harness's `--players` flow.
- Scripted scenes for the host (local) player. VM-only for now.

## Plan

1. **Extend the scene schema** (Phase D's `Scene`). Add:
   ```toml
   [[visual.scene]]
   name = "in-match-turn-1"
   # ...phase D fields...
   precondition = "main-menu"             # name of scene to start from
   timeout_seconds = 30

   [[visual.scene.action]]
   step  = "press steam"
   keys  = "Return"
   sync  = { window_visible = "regicide", min_age_ms = 500 }

   [[visual.scene.action]]
   step  = "enter lobby"
   keys  = "Return"
   sync  = { sway_node_named = "Lobby" }

   [[visual.scene.action]]
   step  = "wait for first turn"
   delay_ms = 0
   sync  = { file_exists = "/tmp/regicide/turn-1.marker" }
   ```
2. **Action types** in `src/game/scene/action.rs`:
   `Keys { keys: String }` (wtype literal),
   `Combo { combo: String }` (wtype -M / -P / -k for modifiers),
   `Focus { app_id: String }` (swaymsg `[app_id=X] focus`),
   `ShellCommand { cmd: String }` (escape hatch),
   `Delay { ms: u64 }` (pure wait, no sync).
3. **Sync predicates** in `src/game/scene/sync.rs`:
   `WindowVisible { app_id, min_age_ms }` (uses Phase B's
   `check_readiness`),
   `SwayNodeNamed { name }` (sway `get_tree` walk),
   `FileExists { path }` (single SSH stat),
   `LogLineMatches { path, regex }` (tail file, regex match),
   `Fixed { ms }`.
   Each predicate exposes `poll(&Backend, &Vm) -> bool` and a
   `description()` string for error messages.
4. **`run_scene` loop** in `src/game/scene/runner.rs`:
   ```
   fn run_scene(backend, vm, scene) -> Result<(), SceneError> {
       if let Some(pre) = scene.precondition { wait_for_scene(pre)? }
       for (idx, action) in scene.actions.iter().enumerate() {
           perform(action)?;
           wait_for_sync(&action.sync, scene.timeout_seconds)
               .map_err(|e| SceneError::SyncFailed {
                   step: action.step.clone(),
                   index: idx,
                   predicate: action.sync.description(),
                   underlying: e,
               })?;
       }
       Ok(())
   }
   ```
5. **Re-enable wayvnc input *conditionally*.** Today
   [src/game/capture.rs:66](../../../../src/game/capture.rs#L66) hard-codes
   `--disable-input`. Make it a flag on `wayvnc_start_script`:
   when scene scripting is active, start wayvnc *without*
   `--disable-input` so input forwarded over RFB can drive the
   compositor. For non-scripted screenshot flows, keep input
   disabled (defence in depth). Document the policy in the
   capture.rs comment.
6. **Use wtype over SSH, not over RFB**, as the default. RFB input
   forwarding requires the script to maintain an active VNC
   connection — extra complexity. `ssh vm wtype 'string'` is
   simpler and equally deterministic when the target window is
   focused via swaymsg first. Document this trade-off.
7. **CLI surface.**
   `cluster-ctl visual capture <scene> [--vm vm-N]` — drives the
   scene and captures. Replaces the manual "capture then hope
   compositor is ready" flow. Reuses Phase E's record / bless /
   diff for the captured frame.
8. **Integrate with `cluster-ctl test`.** Add a
   `--scenes "main-menu,in-match-turn-1"` flag; for each scene,
   run after the test's normal flow reaches a stable state,
   capture, and validate. Failures attach to the run record.
9. **Tests.** Unit-test each `SyncPredicate` against fixture
   outputs (sway get_tree JSON, fake file presence). Integration-
   style test for `run_scene` using a `MockBackend` that records
   commands and produces canned predicate results.
10. **Manual smoke** on Regicide once Phase E is also landed:
    define a 3-step scene that opens Steam, focuses a window, and
    waits for a marker file. Run it. Capture. Bless. Re-run and
    expect SSIM ≥ 0.985.

## Acceptance criteria

- [ ] `cargo test -p steampipe game::scene` — ≥ 10 tests covering
      each `Action` and each `SyncPredicate`, plus a full
      mock-backend `run_scene` happy-path and one failing-sync
      case.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] `cluster-ctl visual capture main-menu --vm vm-1` drives the
      VM through any preconditions, captures, and exits 0.
- [ ] Provoke a sync failure (e.g. set `file_exists` to a
      never-created path) — error message names the step, action
      index, and predicate description.
- [ ] `cluster-ctl test --scenes a,b,c` runs all three scenes per
      test run and emits one validation row per scene.
- [ ] wayvnc input remains disabled for non-scripted screenshot
      paths (regression check: run `cluster-ctl screenshot` and
      verify `--disable-input` in the started wayvnc command).

## Files likely touched

- `src/game/scene/{mod,action,sync,runner}.rs` (new).
- [src/game/mod.rs](../../../../src/game/mod.rs) — module wiring.
- [src/core/config.rs](../../../../src/core/config.rs) — extend
  `Scene` with `actions` / `precondition` / `timeout_seconds`.
- [src/game/capture.rs](../../../../src/game/capture.rs) — make
  `wayvnc_start_script` parameterize input enablement.
- [src/ui/cli.rs](../../../../src/ui/cli.rs) — `Visual::Capture`,
  `test --scenes`.
- [src/main.rs](../../../../src/main.rs) — dispatch + plumbing.

## Pitfalls

- **wtype needs the target window focused.** If the wrong window
  has focus, keystrokes go to /dev/null (or worse, the terminal
  the user opened in foot). Always emit a `swaymsg "[app_id=X]
  focus"` before any `Keys` action. Symptom: scenes pass on
  developer's machine but fail in CI where window stacking
  differs. Recovery: add an implicit focus before each `Keys` if
  the scene declares a `window` target.
- **`wtype` returns 0 even when XKB_CONFIG_ROOT is wrong.** The
  VM has `XKB_CONFIG_ROOT` set in [nix/cluster-vm-base.nix:87-88](../../../../nix/cluster-vm-base.nix#L87-L88)
  — confirm it's still effective in the SSH-launched env. Symptom:
  keystrokes silently dropped. Recovery: source
  `/etc/profile.d/steampipe-graphics.sh` (the
  [vm-graphics.nix:80-83](../../../../nix/vm-graphics.nix#L80-L83)
  file already exists for this purpose) in the wtype invocation.
- **Sync predicates can deadlock.** A `WindowVisible` predicate
  for a window that never appears blocks for `timeout_seconds`.
  Always cap; never poll indefinitely.
- **`LogLineMatches` race.** Game writes the line, predicate
  starts polling after — line missed. Predicate must `tail -F`
  from before the action is performed, then check whether the
  regex matched any line *after* the action's start timestamp.
  Use the SSH backend's existing log-tail helpers if present.
- **Re-enabling wayvnc input is a security boundary.** wayvnc on
  `0.0.0.0:5900` with input enabled lets anyone on the bridge
  drive the VM. Verify the cluster bridge is host-private (it is —
  see [README.md:282-310](../../../../README.md#L282-L310)) and
  document the trust boundary in capture.rs.

## Reference

- Originating diagnosis: chat overview of 2026-05-11 — gap #5
  "Scene scripting."
- Existing wtype usage:
  [src/game/capture.rs](../../../../src/game/capture.rs) (no actual
  scripted usage today; interactive only in steam-login path).
- Phase B's readiness primitives:
  [02-compositor-readiness.md](./02-compositor-readiness.md).
- Phase D's scene schema base:
  [04-visual-profile-config.md](./04-visual-profile-config.md).
- Related phases: G (timelapse uses scene transitions as
  capture triggers).
