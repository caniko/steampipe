# Phase C — In-VM GPU / Vulkan preflight subcommand

> **Recommended Codex model: GPT-5.4 / medium effort**
>
> Moderate complexity, sub-agent role. The work is mostly mechanical
> (shell invocations of `vulkaninfo`, `wayland-info`, a tiny headless
> Vulkan probe; parse the output; surface results) but the failure
> taxonomy needs careful design so the resulting errors actually
> help operators (the current Regicide failure is "Unable to find a
> GPU!" — a useless message). 5.4 at medium gives enough reasoning
> for the error-classification logic without paying for 5.5.

## Working tree

`/data/nvme0/can/Projects/steampipe`. This phase is fully independent
of phases A, B, D, E, F, G — it adds a new subcommand, a new in-VM
diagnostic helper, and a small NixOS module change. Can land in
parallel with all other phases. Files touched have no overlap with
the capture path except a single `cli.rs` insertion (rebase risk
shared with Phase B's new subcommand — small, mechanical to resolve).

## Goal

A new `cluster-ctl gpu-preflight [--runners-dir DIR] [TARGET]` command
that, for each selected VM, verifies the entire graphics stack the
game depends on:

- `/dev/dri/card0` and `/dev/dri/renderD128` exist and the VM user can
  open them.
- `virtio_gpu` kernel module is loaded.
- `vulkaninfo --summary` lists at least one physical device with
  required extensions (`VK_KHR_swapchain`, `VK_KHR_external_memory_fd`,
  `VK_EXT_external_memory_dma_buf`).
- `wayland-info` reports the expected interfaces (`wl_compositor`,
  `zwp_linux_dmabuf_v1`, `zxdg_output_manager_v1`).
- A tiny headless Vulkan render probe completes and writes a known
  pattern to a host-pulled PNG.

`cluster-ctl doctor` is extended to invoke `gpu-preflight` on every
VM that has `microvm.graphics.enable = true`.

## Why this matters now

Two of the originating failures from
[docs/migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md)
are GPU-readiness symptoms that present *after* a long timeout:

```text
Fallback system failed to choose present mode. Mode: Fifo, Options: []
…
NO_PROGRESS_TIMEOUT (local: 602s without semantic progress > 600s)
Unable to find a GPU!
```

Today there is no diagnostic that distinguishes "GPU missing entirely"
from "Vulkan loader cannot find ICD" from "compositor present-mode
mismatch." Operators wait ten minutes for the game to time out, then
have to manually SSH in and run `vulkaninfo`. A preflight subcommand
turns that into a sub-second classification before the test even
starts. It also gives downstream phases (E golden compare, G capture
lifecycle, F scene scripting) a precondition they can assert on:
"GPU preflight green" → frames worth capturing.

## Out of scope

- Fixing the actual Regicide downstream Vulkan issue. That's a
  Regicide-side concern. We only need to *detect* it.
- GL/EGL fallback testing (the project default is Vulkan per
  [nix/vm-graphics.nix](../../../../nix/vm-graphics.nix)).
- Multi-GPU selection. Cluster VMs have one virtio-gpu by design.
- Replacing `microvm.graphics.enable` semantics.
- Cross-platform support — VMs are NixOS Linux only.

## Plan

1. **Add the Vulkan probe binary.** Two options:
   a. Ship a tiny precompiled probe in the VM image. A 200-line Rust
      binary using `ash` or `vulkano` that creates an instance, picks
      device 0, runs a 16×16 compute shader writing a known pattern
      to a buffer, reads back, exits 0. Add as a nix derivation in
      [nix/vm-graphics.nix](../../../../nix/vm-graphics.nix), exposed
      under `environment.systemPackages` when `microvm.graphics.enable
      = true`. Name it `steampipe-vk-probe`.
   b. Cheaper: `vulkaninfo --summary` + `vkcube --c 1` (vkcube exits
      after rendering one frame with `--c`). Already in
      `vulkan-tools`. Less precise; less custom code.
   Recommend option (b) for first cut; promote to (a) if (b) misses
   real failures.
2. **Define `GpuPreflightReport` struct** in
   `src/vm/preflight.rs` (extend the existing file). Fields:
   `dri_devices: Vec<String>`, `module_loaded: bool`,
   `vulkan_devices: Vec<VulkanDevice>` (name, driver, api_version,
   extensions), `wayland_interfaces: Vec<String>`,
   `probe_result: ProbeResult` (Pass / Fail(reason) / Skipped).
3. **Implement remote probing helpers.** Run a single SSH command
   per VM that emits delimited blocks of JSON for each check so we
   only pay one round-trip:
   ```
   echo '---dri---'
   ls -la /dev/dri/ | grep -E 'card|render'
   echo '---module---'
   lsmod | grep -E '^virtio_gpu '
   echo '---vulkan---'
   vulkaninfo --summary 2>/dev/null || echo '__FAIL__'
   echo '---wayland---'
   wayland-info 2>/dev/null || echo '__FAIL__'
   echo '---probe---'
   timeout 10 vkcube --c 1 >/dev/null 2>&1 && echo PASS || echo FAIL
   ```
   Parse into the `GpuPreflightReport`.
4. **Add `cluster-ctl gpu-preflight` subcommand** to
   [src/ui/cli.rs](../../../../src/ui/cli.rs) with `--runners-dir` to
   boot VMs if not running, mirror of the `steam check` pattern. Add
   `--json` flag for machine-readable output. Default human-readable
   table:
   ```
   VM     DRI   Module  Vulkan         Wayland   Probe
   ────────────────────────────────────────────────────────────────
   vm-1   OK    OK      llvmpipe       OK        PASS
   vm-2   OK    OK      virtio-gpu     OK        PASS
   vm-3   OK    FAIL    NONE           OK        FAIL: no device
   ```
5. **Wire into `cluster-ctl doctor`.** Extend
   [src/vm/preflight.rs](../../../../src/vm/preflight.rs) so doctor
   calls `gpu-preflight` on VMs when graphics are enabled. Print as
   warnings (not errors — doctor today is a soft check; GPU failures
   shouldn't block `down` / `net-down` operations).
6. **Error taxonomy.** When the probe fails, classify into:
   `NoDevice`, `NoLoaderIcd`, `NoSwapchainExt`, `NoDmabufExt`,
   `WaylandMissing`, `ProbeCrash`, `Unknown`. Each carries a hint
   string: "set WGPU_BACKEND=vulkan and verify VK_ICD_FILENAMES"
   etc. The Regicide "Unable to find a GPU!" maps to `NoDevice` or
   `NoLoaderIcd` depending on what `vulkaninfo` returns.
7. **Update NixOS module.** In
   [nix/vm-graphics.nix](../../../../nix/vm-graphics.nix), make sure
   `wayland-utils` (provides `wayland-info`) is in
   `environment.systemPackages` alongside `vulkan-tools`.
8. **Unit tests.** Feed captured fixture outputs through the parser
   and assert each report variant. No live VM needed.
9. **Manual smoke** against `cluster-1v1-uifull-stage1-up`: the
   command should complete in < 5 s on a healthy VM, and reproduce
   the Regicide `Unable to find a GPU!` symptom on the broken cluster
   as a structured error well before the test would otherwise time
   out at 600 s.

## Acceptance criteria

- [ ] `cargo test -p steampipe vm::preflight::gpu` — minimum 5
      parser tests with fixture outputs (one all-pass, one missing
      DRI, one missing Vulkan ICD, one missing wayland-info, one
      probe crash).
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] `cluster-ctl gpu-preflight` against a healthy 1-VM cluster
      completes in under 5 s and prints all-OK.
- [ ] `cluster-ctl gpu-preflight` against the known-broken Regicide
      UI-full cluster fails with a non-`Unknown` taxonomy entry
      (`NoDevice` or `NoSwapchainExt` expected based on the symptom)
      and a remediation hint.
- [ ] `cluster-ctl doctor` output includes a graphics-readiness row
      per VM when `microvm.graphics.enable = true`.
- [ ] `cluster-ctl gpu-preflight --json` emits parseable JSON; a
      one-line `jq` filter `.[] | select(.probe_result != "PASS")`
      identifies broken VMs.

## Files likely touched

- [src/ui/cli.rs](../../../../src/ui/cli.rs) — new subcommand.
- [src/main.rs](../../../../src/main.rs) — dispatch.
- [src/vm/preflight.rs](../../../../src/vm/preflight.rs) — extend
  with `gpu_preflight()`.
- [nix/vm-graphics.nix](../../../../nix/vm-graphics.nix) — add
  `wayland-utils` to packages.

## Pitfalls

- **`vulkaninfo` exit code is unreliable across versions.** Some
  releases return 0 even when no devices are found. Always parse
  stdout for `deviceName` lines; do not rely solely on `$?`.
- **`vkcube` flag spelling varies.** `--c` vs `--frames` vs `-c`.
  Run `vkcube --help` once on a target VM and pin the flag in a
  code comment. Symptom: probe times out at 10 s because vkcube
  is waiting for window close. Recovery: confirm flag in fixture
  capture.
- **Wayland-info needs `WAYLAND_DISPLAY` set.** Sourcing it from
  `/run/user/$UID/wayland-1` requires the compositor to be running.
  If you call `gpu-preflight` before sway starts, `wayland-info`
  failure does not imply a real bug. Decide policy: either start
  weston/sway as part of the check, or document that the wayland
  row is conditional.
- **`lsmod | grep virtio_gpu` fails on systems where the module is
  builtin.** Fall back to checking `/sys/module/virtio_gpu` —
  builtin modules appear there too.
- **Adding new packages to the VM image bloats boot time.** Verify
  `wayland-utils` and (if option a) `steampipe-vk-probe` don't push
  past the runtime store-disk size budget.

## Reference

- Originating diagnosis: chat overview of 2026-05-11 — gap #6
  "Per-VM GPU sanity preflight."
- Failure log:
  [docs/migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md)
  ("`Unable to find a GPU!`" and
  "`Fallback system failed to choose present mode. Mode: Fifo`").
- Existing module surface:
  [nix/vm-graphics.nix](../../../../nix/vm-graphics.nix).
- Related phases: Phase G consumes preflight green-status as a
  capture-eligibility gate. Phase E's golden compare benefits from
  preflight as a precondition. No file-level conflicts with phases
  A or B.
