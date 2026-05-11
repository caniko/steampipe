# Phase B — Compositor readiness gate and first-frame handshake

> **Recommended Codex model: GPT-5.4 / medium effort**
>
> Moderate-complexity sub-agent task: introduce a new in-VM readiness check
> using sway IPC and (optionally) `wlr-screencopy` probing, plus a host-side
> wait primitive that capture flows call before requesting frames. The work
> is well-bounded (one new module, a couple of integration points) but
> requires reading sway IPC payloads and reasoning about timing — too risky
> for mini-tier, not novel enough for 5.5.

## Working tree

`/data/nvme0/can/Projects/steampipe`. This phase introduces a new module
(suggested: `src/game/compositor.rs`) and integrates it into existing capture
entry points. It touches [src/game/capture.rs](../../../../src/game/capture.rs)
in `ensure_wayvnc*` and the capture functions — coordinate rebases with
Phase A which also edits that file. Recommended: land Phase A first, then
rebase B on top to avoid resolving conflicts on the capture loop body.

## Goal

Before any capture is taken, the agent can deterministically answer two
questions per VM:

1. Is a wlroots compositor (`sway`) running, has at least one output, and
   has it received at least one buffer commit?
2. (When a target window is specified) is a window with `app_id` /
   `class` matching the target mapped, visible, and not still loading?

Capture functions block on these checks (with configurable deadlines) before
sending RFB / `grim` requests. Failure produces a structured error naming
which subcheck failed.

## Why this matters now

`ensure_wayvnc` ([src/game/capture.rs:82-95](../../../../src/game/capture.rs#L82-L95))
currently only checks that the wayvnc *process* is alive. The script at
[src/game/capture.rs:55-80](../../../../src/game/capture.rs#L55-L80)
sleeps 2 seconds and declares victory. If sway has not yet committed an
output buffer (common during the first ~3 s of boot, especially under
`uifull-stage1`), a capture taken right after `ensure_wayvnc` returns will
contain whatever was in the scanout buffer when wayvnc started — frequently
zeros, sometimes leftover compositor noise.

This is the second half of the "VNC is just noise" symptom on Regicide UI-full
([docs/migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md)).
Phase A makes the bytes we capture correct; Phase B makes sure there *are*
bytes worth capturing.

A "wait for first frame" gate also unblocks Phase F (scene scripting needs
to know when a window has actually mapped before sending input) and
Phase G (timelapse and on-stall captures must distinguish "compositor not
ready" from "game stalled").

## Out of scope

- Anything about wgpu / Vulkan readiness on the client side — that's
  Phase C.
- Changing `wayvnc` command-line flags beyond what readiness checking
  requires (re-enabling input is Phase F).
- Compositor selection logic (sway vs weston) — this phase assumes sway.
  Weston readiness is recorded as a known gap and deferred.
- Changing host-side `grim` capture timing semantics beyond inserting
  the gate before the call.

## Plan

1. **Inventory sway IPC capabilities.** Confirm the running sway version
   in [nix/cluster-vm-base.nix](../../../../nix/cluster-vm-base.nix) (pinned via
   nixpkgs) supports `swaymsg -t get_outputs` and `swaymsg -t get_tree`
   with the `visible` / `focused` / `app_id` fields used below. Pin
   minimum version in a code comment if checking required.
2. **Define a `CompositorReadiness` enum** in `src/game/compositor.rs`:
   `NoSocket`, `NoOutputs`, `NoCommit`, `WindowMissing { app_id }`,
   `WindowUnmapped { app_id }`, `Ready`. Each variant carries enough
   detail for the failure error to be actionable.
3. **Implement `check_readiness(backend, vm, target: Option<WindowTarget>)`**.
   Steps performed remotely over SSH:
   - `swaymsg -t get_outputs` — must return ≥ 1 output with `active=true`.
     Parse JSON.
   - `swaymsg -t get_outputs` again with a 250 ms gap — at least one output's
     `current_mode`/`make` should be stable across the two reads (catches
     mid-modeset state where outputs flicker active/inactive).
   - When `target` is set: `swaymsg -t get_tree` filtered for nodes whose
     `app_id` (Wayland) or `window_properties.class` (Xwayland) matches.
     Node must have `visible: true` and a non-zero `rect`.
4. **Add a wlroots-screencopy buffer-commit probe** (optional but
   recommended): a tiny Rust helper (could be a shell script via `grim
   -t ppm /tmp/_probe.ppm` then `head -c 32`) that confirms the compositor
   has flushed at least one frame. The non-zero check on the first
   16 bytes of the PPM rules out "compositor mapped but never painted."
5. **Build `wait_until_ready`** with exponential backoff (250 ms → 4 s,
   total budget configurable, default 30 s). Mirrors the SSH ready loop in
   [src/core/ssh.rs](../../../../src/core/ssh.rs) — reuse if a generic
   helper exists; otherwise duplicate but mark with a TODO to factor out.
6. **Integrate into `ensure_wayvnc`.** After `wayvnc` is confirmed running,
   call `wait_until_ready(backend, vm, None)`. After this returns Ok,
   capture is safe to attempt.
7. **Integrate into `screenshot_wayland_vm` and `screenshot_vnc_vm`.**
   Same pattern, but the target window (if any) comes from a new field
   on `ScreenshotOptions`. Default `None` preserves current callsites.
8. **Expose `cluster-ctl compositor-status` subcommand** for ops triage,
   printing per-VM readiness in a table. Reuses `par_each_vm`.
9. **Unit tests:** mock `swaymsg` output strings, assert each
   `CompositorReadiness` variant is produced for the right payload.
10. **Manual smoke:** boot a 1-VM cluster, immediately run
    `cluster-ctl screenshot --screenshot-backend vnc`. Without the gate,
    you should reproduce a black-or-incomplete frame; with the gate, the
    screenshot waits up to 30 s for sway to be ready and only then
    captures.

## Acceptance criteria

- [ ] `cargo test -p steampipe game::compositor` — minimum 6 tests
      covering each `CompositorReadiness` variant + the
      output-stability heuristic.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] `cluster-ctl compositor-status` works against a live cluster,
      prints one row per VM showing readiness state.
- [ ] `cluster-ctl screenshot` against a VM that has not booted sway
      yet fails with a `CompositorReadiness::NoSocket` or `NoOutputs`
      error, not an opaque "wayvnc failed" message.
- [ ] `cluster-ctl screenshot` against a freshly-started UI-full
      cluster waits ≤ 30 s and then captures a non-noise frame
      (validated by phase-A's tightened `validate_image_nonblank`).
- [ ] Build hooks in `nix/cluster-vm-base.nix` confirm `swaymsg` is on
      the VM PATH (sway package already provides it; just verify).

## Files likely touched

- `src/game/compositor.rs` (new).
- [src/game/mod.rs](../../../../src/game/mod.rs) — module wiring.
- [src/game/capture.rs](../../../../src/game/capture.rs) — call sites in
  `ensure_wayvnc`, `screenshot_wayland_vm`, `screenshot_vnc_vm`.
- [src/ui/cli.rs](../../../../src/ui/cli.rs) — new `CompositorStatus`
  subcommand (or named `Readiness`).
- [src/main.rs](../../../../src/main.rs) — command dispatch.

## Pitfalls

- **`swaymsg` returns JSON that's nested deeper than you expect.** The
  tree is recursive; nodes can hold children at any depth. A naïve
  top-level scan misses windows. Symptom: `WindowMissing` despite the
  game window being visible. Recovery: write a recursive walker and
  unit-test it against a captured `get_tree` snapshot from a real
  session.
- **wayvnc started ≠ sway ready.** wayvnc can attach to a sway
  session before sway has painted its first frame. Order matters:
  start sway, wait for ready, then start wayvnc (or start wayvnc
  but gate the *capture*, not the wayvnc start, on readiness).
  Current code starts wayvnc inside the same script that assumes
  sway is up; safest is to gate at the capture point only.
- **`grim` probes inside the readiness check can race with the real
  capture.** If the probe writes to `/tmp/_probe.ppm` while wayvnc is
  also serving a client, you can perturb compositor state. Mitigate
  by using a unique path and deleting it after.
- **Xwayland windows have `app_id == null`.** Match on
  `window_properties.class` for those. Steam's overlay window is
  Xwayland in many configs.
- **`useSystemdStage1=true` paths have a slower boot.** The 30 s
  default ready deadline may be tight on cold-boot regicide UI-full.
  Make the deadline configurable per-profile (Phase D will wire this
  to the visual config).

## Reference

- Originating diagnosis: chat overview of 2026-05-11 — gap #2
  "Compositor readiness + scene gating."
- Boot-time evidence:
  [docs/migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md).
- Related phases: Phase A (capture correctness — same file, land first).
  Phase F (scene scripting — consumes `WindowTarget` from this phase).
  Phase G (capture lifecycle — calls `wait_until_ready` before each
  timelapse frame).
