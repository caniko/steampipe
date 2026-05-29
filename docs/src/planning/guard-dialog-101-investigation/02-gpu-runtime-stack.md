# Phase 02 — wgpu/Vulkan/EGL/mesa runtime on NixOS + COSMIC

> **Recommended Codex model: GPT 5.5 medium**
>
> Moderate-to-complex, but bounded: determine whether iced's wgpu backend can
> initialize on this NixOS + COSMIC host and, if not, whether it cleanly falls
> back to tiny-skia or aborts. NixOS GPU driver discovery (Vulkan ICDs, EGL
> vendor, mesa) is fiddly and easy to mis-attribute, so it benefits from a capable
> model — but it is a self-contained investigation against a known stack, not a
> design problem, so `medium` (≈ a cheaper model at high effort) rather than
> `high`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase 01** — read its
`## Findings` first; if 01 already resolved the failure as H1/H2 (stale binary /
unreloaded env) and the dialog now opens, this phase is unnecessary. Proceed only
if a GPU/renderer-level cause is still open. **Investigation only** (no durable
changes); record evidence in `## Findings`.

## Goal

Establish, with evidence, whether the wgpu (hardware) renderer can initialize for
`cluster-guard-prompt` on this NixOS + COSMIC host, what runtime inputs it needs
(Vulkan ICD files, EGL/GL vendor, mesa drivers), and — crucially — whether a wgpu
init failure **panics/aborts** (uncatchable by iced's fallback) or returns an
error that iced falls back from to tiny-skia. Output: the minimal runtime-env set
that makes wgpu work, OR a definitive "force/rely-on tiny-skia" recommendation,
with the reason iced's auto-fallback did or didn't engage.

## Why this matters now

The helper enables both `wgpu` and `tiny-skia` expecting iced to auto-select and
fall back. On NixOS, a non-Nix-wrapped binary typically can't find the Vulkan ICD
(`VK_ICD_FILENAMES`/`VK_DRIVER_FILES` are normally set by the NixOS graphics stack
or a wrapper) or the EGL vendor library, so wgpu may fail. If that failure is a
panic rather than a recoverable error (README H3), iced never falls back and the
process exits 101. Phase 01 should have captured the real backtrace; this phase
interprets and reproduces the GPU-layer portion and finds the minimal fix inputs.

## Out of scope

- Landing the wrapper/packaging fix — Phase 05 (this phase specifies *what* env
  the fix must provide).
- direnv / nested-cargo env *propagation* mechanics — Phase 03 (this phase is
  about *what GPU inputs are needed*, not how they reach the process).
- iced version/feature build correctness & minimal-repro harness — Phase 04
  (coordinate: if Phase 04 builds a bare-iced repro, reuse it here).

## Plan

1. **Inventory the host GPU stack.** Record GPU/driver: `lspci -nnk | grep -iA3 vga`,
   `nix-shell -p vulkan-tools --run vulkaninfo` (or `vulkaninfo` if present),
   `glxinfo -B` / `eglinfo` (via `mesa-demos`/`mesa-utils`), and whether
   `/run/opengl-driver/lib` exists (the NixOS graphics driver path). Note the
   COSMIC compositor version.
2. **Find the Vulkan ICD + EGL vendor inputs.** Determine where the mesa Vulkan
   ICD JSON lives (e.g. `…/share/vulkan/icd.d/*.json`) and whether
   `VK_ICD_FILENAMES`/`VK_DRIVER_FILES` is set in the failing shell (cross-ref
   Phase 01's env diff). Same for `__EGL_VENDOR_LIBRARY_DIRS`/libglvnd and
   `LIBGL_DRIVERS_PATH`. Establish whether a bare binary under the dev shell sees
   these.
3. **Reproduce the wgpu path in isolation.** Run the helper with backend
   selection forced to observe behavior (temporary env, reverted):
   - `WGPU_BACKEND=vulkan` then `WGPU_BACKEND=gl` — does either open a window or
     change the error?
   - With and without `VK_ICD_FILENAMES=$(echo /run/opengl-driver/share/vulkan/icd.d/*.json | tr ' ' ':')`
     (or the mesa ICD path found in step 2).
   - Capture whether failures are panics (exit 101 / abort) or clean errors.
4. **Determine iced's fallback behavior empirically.** With wgpu deliberately
   broken (e.g. `WGPU_BACKEND=secret_does_not_exist` or no ICD), does the helper
   fall back to tiny-skia and open a window, or exit 101? This answers README H3.
   Cross-check against Phase 04's findings on iced 0.14 renderer fallback.
5. **Confirm tiny-skia viability.** Force the software path (e.g. via a temporary
   build with only `tiny-skia`, or `LIBGL_ALWAYS_SOFTWARE=1` / `WGPU_BACKEND=gl`
   with llvmpipe) and confirm a window opens on this COSMIC session. (Phase 01
   already saw tiny-skia open under `nix develop -c`; confirm in the real session.)
6. **Specify the fix inputs.** Write the minimal, ordered env/wrapper requirements
   for Phase 05: exact `VK_ICD_FILENAMES` (or `/run/opengl-driver` reliance), EGL
   vendor dirs, and whether to (a) make wgpu work, (b) force tiny-skia, or
   (c) keep auto-discovery once the ICD env is provided. Note tradeoffs (GPU perf
   vs robustness) — but a one-shot modal almost certainly does not need the GPU.

## Acceptance criteria

- [ ] `## Findings` records the host GPU/driver inventory, the location of the
      Vulkan ICD JSON + EGL vendor libs, and whether the failing shell exposed
      them.
- [ ] Evidence states whether wgpu init **panics** (uncatchable) or **errors**
      (recoverable) on this host, and therefore whether iced's tiny-skia fallback
      engages — directly resolving README H3.
- [ ] A confirmed minimal runtime-env recipe is given that makes the dialog open
      (either wgpu-with-ICD-env, or forced tiny-skia), reproduced at least once in
      the real interactive COSMIC session.
- [ ] A clear recommendation for Phase 05: provide ICD env / force software /
      keep auto-discovery — with rationale (a one-shot prompt likely needs no GPU).
- [ ] No durable repo changes; temporary env/build experiments reverted.

## Files likely touched

- None durably. Temporary, reverted: env exports; optionally a throwaway
  `tiny-skia`-only build of the helper for comparison (revert `Cargo.toml`).
- This phase file's `## Findings`.

## Pitfalls

- **`vulkaninfo` succeeding ≠ wgpu succeeding from this binary.** The system tools
  may be Nix-wrapped (they find ICDs) while the bare `cargo`-built helper is not.
  *Recovery:* test the actual helper, not just `vulkaninfo`.
- **Conflating "no GPU" with "no display".** A NoWaylandLib/winit failure (Phase
  03 territory) is *before* the renderer. Make sure the event loop is created
  (window backend OK) before attributing failure to wgpu. *Recovery:* read Phase
  01's backtrace — winit vs wgpu vs tiny-skia frame.
- **NixOS `/run/opengl-driver`.** On NixOS the driver path is
  `/run/opengl-driver/lib` (+ `share/vulkan/icd.d`); standard `/usr/lib` paths are
  absent. A wrapper must point at it. *Recovery:* prefer `/run/opengl-driver`-based
  env over hardcoded store paths so it survives driver updates.
- **COSMIC specifics.** COSMIC is wlroots-adjacent/Smithay-based; some wgpu+Wayland
  present-mode combos misbehave. If wgpu is flaky here, software tiny-skia for a
  tiny modal is the pragmatic call. *Recovery:* don't over-invest in wgpu for a
  prompt window.

## Reference

- README H3 (wgpu panic vs fallback) and the `nix develop -c` vs `cargo run` clue:
  [README.md](./README.md).
- Phase 01 backtrace + env diff: [01-ground-truth-capture.md](./01-ground-truth-capture.md).
- Renderer features: [crates/cluster-guard-prompt/Cargo.toml](../../../../crates/cluster-guard-prompt/Cargo.toml).
- Coordinate with Phase 04 (iced fallback internals) and feed Phase 05 (fix).

## Findings

Captured on 2026-05-29 from `/data/nvme0/can/Projects/steampipe`.

Phase 02 was intentionally stopped at the Phase 01 dependency gate. Phase 01's
`## Findings` already resolved the observed exit-101 failure as H2, not a
GPU/renderer-level cause:

- The reproduced panic occurs while iced/winit creates the Wayland event loop:
  `WaylandError(Connection(NoWaylandLib))`.
- The panic happens before wgpu, Vulkan, EGL, GL vendor, or tiny-skia renderer
  initialization can be the failing layer.
- The failing context had `LD_LIBRARY_PATH` unset. The working `nix develop -c`
  and refreshed `direnv exec .` contexts expose Wayland, libxkbcommon, libGL,
  Vulkan loader, fontconfig/freetype, and X11 libraries through
  `LD_LIBRARY_PATH`.
- After `direnv reload`, both direct helper execution and nested
  `cargo run --quiet --package cluster-guard-prompt -- ...` opened the dialog in
  the real session and timed out cleanly with `exit=11`.
- Phase 01 explicitly classified H3 (`wgpu init panic / Vulkan ICD`) as refuted
  for this failure, because the captured backtrace and error are at event-loop
  creation rather than renderer initialization.

Because the phase brief says to proceed only if a GPU/renderer-level cause is
still open, the Vulkan/GL inventory and forced-backend experiments were not run.
Running them after Phase 01's resolution would not answer the observed failure
and could incorrectly turn a closed H2 issue into speculative GPU work.

The runtime recipe proven by Phase 01 is therefore the current minimal recipe for
the observed failure: run the helper from a refreshed dev shell / direnv context
that provides the GUI library path. Phase 05 should make that environment
durable for installed or non-direnv launches. At minimum, the wrapper or package
must make the Wayland event-loop dependencies discoverable before considering
renderer policy:

- Wayland client libraries (`wayland`).
- Keyboard/common Wayland support (`libxkbcommon`).
- GL/Vulkan loader libraries already present in the working dev-shell path
  (`libglvnd`, `vulkan-loader`), plus the existing font/X11 libraries used by
  the helper closure.

No durable source changes or temporary GPU/backend experiments were made by this
phase. The Phase 02 GPU-specific acceptance criteria are not applicable because
the Phase 01 gate closed the GPU hypothesis before this phase could validly
proceed.
