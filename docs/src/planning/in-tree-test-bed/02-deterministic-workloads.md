# Phase B — Deterministic graphical workloads for the fixture

> **Recommended Codex model: GPT-5.4 / medium effort**
>
> Moderate-complexity sub-agent work: package three small graphical
> workloads as Nix derivations, expose them through the fixture flake,
> and write small autostart units that launch them on VM boot. The
> complexity is in producing *deterministic* output frame-to-frame
> (fixed seeds, fixed text, fixed shader uniforms) — not in writing
> a lot of code. 5.4/medium gives enough reasoning headroom for the
> determinism design; mini-tier risks producing time-varying outputs
> that defeat golden comparison.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Depends on Phase A of this
plan: the fixture flake and `tests/fixture/` skeleton must exist
before workloads can be wired into it. All new files live under
`tests/fixture/workloads/` plus extensions to
`tests/fixture/flake.nix`. No external repos.

## Goal

The fixture VM, on boot, can run any one of three deterministic
graphical workloads on demand, addressable by name from the host:

1. **`foot-banner`** — `foot` terminal full-screen showing a fixed
   ASCII banner with monotonic frame counter blanked out. The
   captured frame is bit-stable across runs (same text, same
   colours, same compositor wallpaper).
2. **`vkcube-frozen`** — `vkcube --c 1` (single frame) at a fixed
   resolution. Geometry deterministic by virtue of vkcube's fixed
   model + fixed camera + `--c 1`.
3. **`wgpu-checker`** — a tiny in-repo Rust binary that draws a
   16×16 chequerboard to a wgpu surface, presents one frame, and
   blocks. Provides a known content that exercises the same
   Vulkan/wgpu/Wayland path the eventual consumer (Regicide /
   any wgpu-backed game) uses.

`ssh fixture@10.0.100.1 'workload-run foot-banner'` launches the
workload; `workload-stop` kills it; `workload-current` reports
which one is active.

## Why this matters now

Phase A boots a VM. That VM, idle, shows a black sway desktop —
not useful as a comparison target. Every later phase of the
visual-testing plan needs *something* known-good on screen:

- `vnc-visual-testing/01` (VNC client correctness) needs a frame
  with known content to verify capture correctness, not just
  liveness.
- `vnc-visual-testing/05` (validation engine) needs at least one
  scene's worth of golden image to validate SSIM on.
- `vnc-visual-testing/06` (scene scripting) needs a target window
  whose `app_id` is known to script against.

`foot-banner` covers the trivial CPU-rendered case (catches
capture-loop bugs). `vkcube-frozen` covers the GPU/swapchain path
(catches the failure mode from
[../../migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md):
`Fallback system failed to choose present mode. Mode: Fifo`).
`wgpu-checker` covers the production rendering stack (the one
real games — and Regicide specifically — actually use).

Without these, every test downstream still has to use a real game
to produce on-screen content, which is exactly the entanglement
this plan exists to remove.

## Out of scope

- Capturing the golden images. Phase C.
- Diagnostic automation around the workloads. Phase D.
- Multi-frame animation / video workloads. Phase G of
  `vnc-visual-testing` may want them later; for now, one stable
  frame per workload.
- Cross-resolution workloads. Pin each to 1280×720.
- Input-driven workloads. Phase F of `vnc-visual-testing` covers
  scripted interaction; these workloads do not respond to keys.
- Running the workloads under user-mode (non-root, non-`fixture`).
  All three run as the `fixture` user under the autostarted sway
  session.

## Plan

1. **Decide where to run the workloads.** Recommendation: start
   sway as a `systemd --user` unit owned by the `fixture` user,
   then have an `ExecStartPost` source point to a writable
   `~/.workload` symlink that the user can `ln -sf` to switch
   workloads. The boot path is: VM up → `sway.service` (user) →
   sway autostart runs the workload pointed at by `~/.workload`.
   Document this clearly in `tests/fixture/README.md`.
2. **Package `foot-banner`** as a `pkgs.writeShellApplication`:
   ```bash
   #!/usr/bin/env bash
   # Render a fixed banner. No timestamps, no PID, no user prompt.
   exec foot --title=workload-foot-banner --fullscreen \
     --override=initial-window-size-chars=120x32 \
     -- bash -c 'printf "%s\n" \
       "================================================" \
       "  STEAMPIPE IN-TREE FIXTURE — FOOT BANNER       " \
       "  resolution 1280x720  shell foot v$(foot --version 2>&1 | head -1)" \
       "================================================" \
       "  This frame is deterministic. Any pixel-level   " \
       "  diff vs. the golden indicates a capture or     " \
       "  compositor regression.                          " \
       "================================================" ; \
       sleep infinity'
   ```
   Notes:
   - `--title=workload-foot-banner` exposes a stable sway
     `app_id`/`title` for later scripting.
   - `--fullscreen` so the captured frame is fully foot's content,
     not sway wallpaper around it.
   - `sleep infinity` keeps the window open and content frozen.
   - Avoid embedding `$(date)`, `$$`, `$RANDOM`, or any other
     time-varying string in the banner.
3. **Package `vkcube-frozen`** as a wrapper around
   `pkgs.vulkan-tools`'s `vkcube`:
   ```bash
   exec vkcube --c 1 --present-mode immediate
   ```
   `--c 1` makes vkcube exit after rendering one frame; that frame
   sits on the swapchain until the workload is killed. If the
   `--present-mode immediate` flag is unsupported on the pinned
   vkcube version, fall back to default fifo and document the
   choice in a comment. The exit-after-one-frame behaviour means
   the workload is technically not "running" by the time we
   capture — confirm whether the swapchain image persists; if not,
   wrap in `while true; do vkcube --c 1; done` and document the
   visible-flicker tradeoff.
4. **Package `wgpu-checker`** as a tiny Rust binary under
   `tests/fixture/workloads/wgpu-checker/`. Use `wgpu` crate from
   nixpkgs (pinned via the steampipe Cargo workspace if practical;
   otherwise its own crate with its own Cargo.lock — decide based
   on whether you can avoid pulling all of steampipe's deps into
   the fixture). The binary:
   - opens a 1280×720 winit window with title
     `workload-wgpu-checker`,
   - draws a 16×16 chequerboard alternating fixed
     `(0xFF, 0xFF, 0xFF, 0xFF)` and `(0x10, 0x20, 0x40, 0xFF)`,
   - calls `present()` once,
   - blocks on `winit::event_loop` until killed.
   No animation, no input handling beyond `WindowEvent::CloseRequested`.
5. **Expose all three** as flake outputs in
   `tests/fixture/flake.nix`:
   ```nix
   packages.foot-banner = …;
   packages.vkcube-frozen = …;
   packages.wgpu-checker = …;
   ```
6. **Inject into the VM.** Add a NixOS module
   `tests/fixture/workloads.nix` that:
   - Adds all three packages to `environment.systemPackages`.
   - Provides a `workload-run NAME`, `workload-stop`, and
     `workload-current` shell wrapper under
     `pkgs.writeShellApplication` in
     `environment.systemPackages`.
   - Configures `services.greetd` (or systemd-user) to start sway
     as the `fixture` user on boot.
   - Drops a sway autostart line that `exec`s
     `~/.workload` if the symlink exists.
   Import this module in the fixture's NixOS configuration via the
   call site that `mkTestCluster` exposes — refer to Phase A's
   wiring.
7. **Confirm the workload-run/-stop/-current contract.** After
   booting the fixture and running
   `ssh fixture@10.0.100.1 'workload-run foot-banner'`, then
   `swaymsg -t get_tree | grep -i 'workload-foot-banner'` should
   find a node with that title within 3 s. `workload-stop`
   removes it.
8. **Add a smoke driver** to `tests/fixture/README.md`: a copy-
   pasteable sequence to launch each of the three workloads and
   confirm a window appears.
9. **Sanity-check determinism.** Boot the fixture twice. Launch
   the same workload. Take a `grim` screenshot from inside the VM
   both times. The two PNGs should differ only in metadata, not
   pixels — verify with `sha256sum` after stripping PNG metadata
   (`pngcrush -rem alla` or similar). Document the verification
   command in the README so future contributors can re-run it.

## Acceptance criteria

- [ ] `nix build ./tests/fixture#foot-banner` succeeds.
- [ ] `nix build ./tests/fixture#vkcube-frozen` succeeds.
- [ ] `nix build ./tests/fixture#wgpu-checker` succeeds.
- [ ] After `cluster-1v1-up`, SSH command
      `ssh fixture@10.0.100.1 'workload-run foot-banner'` returns 0
      within 3 s.
- [ ] `swaymsg -t get_tree` inside the VM reports a node with
      `title == "workload-foot-banner"` within 5 s of `workload-run`.
- [ ] Same check passes for `vkcube-frozen`
      (`title == "workload-vkcube-frozen"` or `app_id == "vkcube"` —
      pick whichever wlroots actually sets and document the choice).
- [ ] Same check passes for `wgpu-checker`
      (`title == "workload-wgpu-checker"`).
- [ ] `workload-stop` removes the active workload window within
      2 s of invocation.
- [ ] Determinism: two consecutive `grim` captures of
      `foot-banner` on the same boot produce identical SHA-256
      hashes after PNG-metadata stripping.
- [ ] `tests/fixture/README.md` documents each workload's title,
      resolution, expected hash, and how to relaunch / switch
      between them.
- [ ] No regressions: `cargo test --workspace` still green;
      `nix flake check ./tests/fixture` still green.

## Files likely touched

- `tests/fixture/workloads/foot-banner.nix` (new) — derivation +
  shell script.
- `tests/fixture/workloads/vkcube-frozen.nix` (new).
- `tests/fixture/workloads/wgpu-checker/{Cargo.toml,src/main.rs}`
  (new).
- `tests/fixture/workloads/wgpu-checker.nix` (new) — derivation
  wrapping the Rust crate.
- `tests/fixture/workloads.nix` (new) — NixOS module wiring.
- `tests/fixture/flake.nix` (extend) — expose packages, import
  workloads module into VM config.
- `tests/fixture/README.md` (extend).

## Pitfalls

- **`vkcube --c 1` exits and the swapchain image may or may not
  persist.** Behaviour depends on driver + compositor. Symptom:
  capturing after `vkcube` exits returns sway wallpaper. Cause:
  the surface was destroyed. Recovery: either loop vkcube (`while
  true; do vkcube --c 1; done`) accepting visible blink between
  frames, or pick a different one-frame-and-block tool. Decide and
  document.
- **`foot --fullscreen` flag may not exist.** Newer foot uses
  `--maximized` or a sway-side rule. Symptom: `unknown option
  --fullscreen`. Recovery: switch to a sway-side window rule in
  the autostart script (`for_window [title="workload-foot-banner"]
  fullscreen enable`).
- **Banner determinism is harder than it looks.** `foot --version`
  embeds a version string. If the nixpkgs version bumps, the
  golden hash changes. Pin foot's version in `cluster-vm-base.nix`
  for the fixture, or omit version strings from the banner.
- **wgpu-checker pulling in steampipe's full Cargo workspace
  is a 90-second incremental build cost.** If sharing the
  workspace causes pain, give the workload its own
  `tests/fixture/workloads/wgpu-checker/Cargo.toml` and accept
  the dep duplication.
- **Sway autostart timing.** If `workload-run` runs before sway
  is ready, the new window never appears. The autostart symlink
  pattern (sway exec's `~/.workload` when sway itself starts)
  avoids this; do not introduce a separate "run after boot"
  systemd unit that races sway.
- **Determinism check across boots is *not* an acceptance
  criterion.** Cross-boot determinism depends on cursor position,
  initial sway state, etc. Verify same-boot determinism only;
  cross-boot stability is a Phase C concern (capture once, treat
  as canon).
- **`wgpu` and `winit` versions matter.** Pin in the workload's
  Cargo.toml. If you take `wgpu = "*"` you'll see frame
  differences after every `cargo update`. Lock the lockfile and
  commit it.

## Reference

- Originating context: in-chat exchange of 2026-05-11 — the user
  asked for an in-tree fixture *before* exercising the
  visual-testing plan against regicide, and explicitly named
  `foot`, `vkcube`, and a small wgpu workload as the candidate
  content.
- Failure mode the wgpu workload mirrors:
  [../../migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md)
  (`Fallback system failed to choose present mode. Mode: Fifo`).
- Related phase: Phase A of this plan (fixture scaffolding —
  prerequisite). Phase C (goldens — consumes the deterministic
  output produced here). Phase D (diagnostic recipes — drives
  these workloads).
- Related downstream plan: every phase of
  [../vnc-visual-testing/](../vnc-visual-testing/README.md) uses
  these workloads as test content.
