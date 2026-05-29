# Phase 04 — iced 0.14 renderer-fallback & build/feature correctness

> **Recommended Codex model: GPT 5.5 medium**
>
> Moderate complexity requiring specific knowledge of iced 0.14's renderer
> architecture (how/whether the wgpu→tiny-skia fallback engages, and whether a
> backend init failure surfaces as a catchable error or a panic) plus a small
> throwaway reproducer. Bounded and self-contained, with no cross-system
> orchestration, so `medium` is the right tier — enough capability to reason about
> the crate internals without paying for `high`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase 01** — read its
`## Findings`; skip if 01 resolved it. **Investigation only** (a minimal,
throwaway iced repro is fine; do not commit it); record in `## Findings`.

## Goal

Determine whether the `cluster-guard-prompt` binary is actually built with the
intended renderer features and whether iced 0.14's renderer **auto-fallback**
(wgpu → tiny-skia) behaves as assumed: specifically, whether a wgpu backend
initialization failure is converted into a fallback (window still opens) or
propagates as a **panic** that aborts the process (exit 101). Output: a
definitive statement of iced's fallback semantics on this version, whether the
helper's features are correct, and whether a code change (e.g. force tiny-skia,
or catch/guard the renderer init) is warranted in Phase 05.

## Why this matters now

The helper enables both `wgpu` and `tiny-skia`
([crates/cluster-guard-prompt/Cargo.toml](../../../../crates/cluster-guard-prompt/Cargo.toml))
on the assumption iced auto-selects wgpu and falls back to software on failure.
README H3 questions that: if wgpu init *panics* (rather than returning an
`Err`/`GraphicsCreationFailed` that iced's fallback compositor handles), the
`catch_unwind` in `main` should turn it into exit 12 — but the observed exit is
**101**, implying the panic escaped `catch_unwind` (e.g. on a wgpu/winit worker
thread) or the failure isn't where we think. Phase 02 covers the GPU inputs; this
phase covers iced/winit's *behavior* given those inputs and the build's
correctness.

## Out of scope

- Host GPU driver/ICD specifics — Phase 02 (consume its findings on whether wgpu
  init fails here).
- Env propagation — Phase 03.
- Landing the fix — Phase 05 (this phase recommends whether the fix is code-level
  — force/guard renderer — or purely runtime-env).

## Plan

1. **Verify the actual build features.** Confirm the compiled helper really has
   both renderers: `cargo tree -p cluster-guard-prompt -e features -i iced`,
   inspect `iced_renderer`'s enabled features, and confirm `iced_wgpu` and
   `iced_tiny_skia` are both present. Rule out a feature-resolution surprise.
2. **Pin iced/winit/wgpu versions** from `Cargo.lock` (iced 0.14.x, winit,
   wgpu/naga) and read the iced 0.14 renderer-fallback docs/source: does
   `iced_renderer::Renderer` (the "fallback" compositor) try wgpu then tiny-skia,
   and is the wgpu compositor creation failure a recoverable `Error` or a panic?
   Quote the relevant iced/iced_wgpu code path.
3. **Minimal reproducer.** Build a tiny throwaway iced 0.14 app (a bare window) in
   a scratch crate or example with the same features, and run it in the failing
   interactive shell under `RUST_BACKTRACE=full`:
   - default (auto) renderer,
   - wgpu deliberately broken (no ICD / `WGPU_BACKEND=invalid`),
   - tiny-skia forced.
   Record which combinations open a window vs exit 101 vs exit 12. This isolates
   iced behavior from cluster-ctl/steampipe entirely.
4. **Locate the panic frame.** Using Phase 01's `RUST_BACKTRACE=full` capture (and
   the repro), identify whether the panic originates in `iced_winit` (event loop),
   `iced_wgpu` (adapter/surface/device request), or elsewhere, and whether it's on
   the main thread (catchable) or a pool/worker thread (escapes `catch_unwind`,
   → exit 101).
5. **Assess `catch_unwind` adequacy.** The helper wraps `iced::run` in
   `catch_unwind`. Determine whether the observed panic escapes it (worker thread
   / FFI abort). If so, note that a code-level guard (pre-flight adapter probe, or
   forcing tiny-skia when no GPU) is needed rather than relying on catch_unwind.
6. **Recommend.** State whether Phase 05 should: (a) keep auto-discovery and only
   fix the runtime env (if wgpu init is a clean fallback given the ICD), (b) force
   tiny-skia for this one-shot modal (simplest, no GPU dependency), or (c) add a
   pre-flight renderer probe / explicit error handling. Include the iced-version
   evidence backing the choice.

## Acceptance criteria

- [ ] `## Findings` proves the helper builds with both `iced_wgpu` and
      `iced_tiny_skia` (via `cargo tree`), with versions pinned from `Cargo.lock`.
- [ ] A clear, source/doc-backed statement of iced 0.14's wgpu→tiny-skia fallback
      semantics: does wgpu init failure fall back, return an error, or panic?
- [ ] A minimal iced reproducer's results are recorded for auto / broken-wgpu /
      forced-tiny-skia in the failing shell (window vs 101 vs 12), isolating iced
      from steampipe.
- [ ] The panic frame is attributed (winit vs wgpu vs other; main vs worker
      thread) and `catch_unwind` adequacy is judged.
- [ ] A concrete Phase 05 recommendation (env-only / force-tiny-skia / code-guard)
      with rationale, noting a one-shot prompt needs no GPU.
- [ ] No durable repo changes; the reproducer crate/example is throwaway/removed.

## Files likely touched

- None durably. Throwaway: a scratch iced example/crate for the minimal repro
  (outside the workspace or removed before finishing).
- This phase file's `## Findings`.

## Pitfalls

- **Feature unification surprises.** Another workspace member could unify iced
  features unexpectedly; confirm the *actual* resolved features for the helper, not
  the declared ones. *Recovery:* `cargo tree -e features`.
- **catch_unwind can't catch everything.** Panics on threads other than the one
  calling `catch_unwind`, or `panic = "abort"`, or FFI aborts, escape it →
  exit 101. Don't assume the helper's `catch_unwind` makes all GUI failures
  exit 12. *Recovery:* attribute the thread of the panic from the backtrace.
- **Repro drift.** If the throwaway repro uses different features/versions than the
  helper, its results don't transfer. *Recovery:* mirror the helper's exact iced
  features and lock versions.
- **Over-engineering the renderer.** A 420×230 one-shot modal does not benefit from
  the GPU; if wgpu is at all flaky on COSMIC, forcing tiny-skia is the robust call.
  Don't recommend a complex GPU-enablement path for a trivial prompt unless it's
  free. *Recovery:* bias the recommendation toward software for this use case.

## Reference

- README H3 (wgpu panic vs fallback): [README.md](./README.md).
- Renderer features: [crates/cluster-guard-prompt/Cargo.toml](../../../../crates/cluster-guard-prompt/Cargo.toml);
  helper `main` + `catch_unwind`: [crates/cluster-guard-prompt/src/main.rs](../../../../crates/cluster-guard-prompt/src/main.rs).
- Phase 01 backtrace; Phase 02 GPU-init outcome (coordinate the wgpu-broken case).
- Feeds Phase 05 (decides env-only vs code-level fix).

## Findings

Captured on 2026-05-29 from `/data/nvme0/can/Projects/steampipe`.

### Phase 01 gate

Phase 01 already resolved the observed exit-101 failure as a winit/Wayland
runtime-library problem, not a renderer/GPU problem. The panic frame is:

```text
iced_winit-0.14.0/src/lib.rs:81:10
Create event loop: ... winit-0.30.13/.../wayland/event_loop/mod.rs:89
WaylandError(Connection(NoWaylandLib))
```

That call happens before iced creates a graphics compositor, before wgpu requests
an adapter/device, and before tiny-skia fallback can be considered. Phase 02
therefore correctly skipped the GPU-runtime investigation for the observed
failure. Phase 04 still verified feature resolution and iced fallback semantics
below so Phase 05 can make the renderer decision with evidence.

### Resolved build features and versions

`cluster-guard-prompt` is built with both intended renderers. The declared iced
dependency uses `default-features = false` with `wgpu`, `tiny-skia`, `x11`,
`wayland`, and `tokio`; `cargo metadata` confirmed that exact feature list.

`cargo tree -p cluster-guard-prompt -e features -i iced` shows:

```text
iced v0.14.0
├── iced feature "tiny-skia"
├── iced feature "tokio"
├── iced feature "wayland"
├── iced feature "wgpu"
├── iced feature "wgpu-bare"
└── iced feature "x11"
```

`cargo tree -p cluster-guard-prompt -e features -i iced_renderer` shows
`iced_renderer` has both `iced_wgpu` and `iced_tiny_skia` enabled:

```text
iced_renderer feature "iced_tiny_skia"
iced_renderer feature "iced_wgpu"
iced_renderer feature "tiny-skia"
iced_renderer feature "wgpu"
iced_renderer feature "wgpu-bare"
```

`cargo tree -p cluster-guard-prompt -e features -i iced_wgpu` and
`cargo tree -p cluster-guard-prompt -e features -i iced_tiny_skia` both resolve
through `iced_renderer v0.14.0`. There is no feature-resolution surprise.

Pinned versions from `Cargo.lock`:

```text
iced 0.14.0
iced_renderer 0.14.0
iced_wgpu 0.14.0
iced_tiny_skia 0.14.0
iced_winit 0.14.0
iced_widget 0.14.2
winit 0.30.13
wgpu 27.0.1
naga 27.0.3
```

### iced 0.14 fallback semantics

With both renderer features enabled, `iced_renderer-0.14.0/src/lib.rs` aliases
the default compositor to a fallback pair:

```text
fallback::Compositor<iced_wgpu::window::Compositor,
                     iced_tiny_skia::window::Compositor>
```

`iced_renderer-0.14.0/src/fallback.rs` implements that compositor by trying the
primary compositor first and, only if it returns `Err`, trying the secondary
compositor with the same backend candidate. If both fail, it returns
`graphics::Error::List(errors)`.

`iced_wgpu-0.14.0/src/window/compositor.rs` converts normal initialization
failures into errors, not panics:

- `request_adapter(...).await` maps failure to `Error::NoAdapterFound`.
- incompatible surfaces return `Error::IncompatibleSurface`.
- `request_device(...).await` failures are collected into
  `Error::RequestDeviceFailed`.
- `impl From<Error> for graphics::Error` reports
  `GraphicsAdapterNotFound { backend: "wgpu", ... }`.

Therefore iced 0.14's intended wgpu-to-tiny-skia fallback is real for recoverable
wgpu compositor creation failures. It does not catch panics, and it does not
apply to failures before compositor creation, such as `iced_winit` event-loop
creation.

Two important non-fallback panic sites remain:

- `iced_winit-0.14.0/src/lib.rs:81` calls
  `EventLoop::with_user_event().build().expect("Create event loop")`; this is
  the Phase 01 `NoWaylandLib` panic and occurs before renderer selection.
- `iced_winit-0.14.0/src/lib.rs` panics on
  `compositor::SurfaceError::OutOfMemory` during present; this is later than
  init and not a wgpu-to-tiny-skia fallback path.

### Runtime checks

No separate scratch crate was kept. To avoid repro drift, the current
`cluster-guard-prompt` binary itself was used as the minimal one-window iced
repro after `cargo build -p cluster-guard-prompt` completed successfully.

All display-positive runs used the known host Wayland socket:

```text
XDG_RUNTIME_DIR=/run/user/1000
WAYLAND_DISPLAY=wayland-1
```

Results:

```text
direnv exec . env ... target/debug/cluster-guard-prompt --timeout-secs 1
exit=11
```

Default auto renderer opened the dialog and timed out cleanly.

```text
direnv exec . env ... WGPU_BACKEND=vulkan \
  VK_ICD_FILENAMES=/nonexistent/icd.json \
  target/debug/cluster-guard-prompt --timeout-secs 1
exit=11
```

Deliberately broken wgpu inputs with auto renderer still opened the dialog and
timed out. This demonstrates the wgpu `Err` to tiny-skia fallback on this locked
version.

```text
direnv exec . env ... ICED_BACKEND=tiny-skia WGPU_BACKEND=vulkan \
  VK_ICD_FILENAMES=/nonexistent/icd.json \
  target/debug/cluster-guard-prompt --timeout-secs 1
exit=11
```

Forced tiny-skia opened the dialog and timed out cleanly.

```text
direnv exec . env ... ICED_BACKEND=wgpu WGPU_BACKEND=vulkan \
  VK_ICD_FILENAMES=/nonexistent/icd.json \
  target/debug/cluster-guard-prompt --timeout-secs 1
cluster-guard-prompt: failed to start the GUI event loop: the application graphics context could not be created
exit=12
```

Forced wgpu with the deliberately broken Vulkan ICD did not panic. iced returned
a graphics-context creation error, and the helper converted it to exit 12.

For comparison, reproducing Phase 01's missing-GUI-library case still exits 101:

```text
env -u LD_LIBRARY_PATH XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 \
  RUST_BACKTRACE=full target/debug/cluster-guard-prompt --timeout-secs 1
cluster-guard-prompt: GUI backend panic: panicked at .../iced_winit-0.14.0/src/lib.rs:81:10:
Create event loop: ... WaylandError(Connection(NoWaylandLib))
exit=101
```

This failure is winit, not wgpu, and it occurs before renderer fallback.

### catch_unwind adequacy

For recoverable renderer creation failures, `catch_unwind` is not the important
mechanism: iced returns `Err`, and the helper exits 12 through its normal
`Ok(Err(error))` branch.

For the Phase 01 failure, the helper's panic hook does run, but
`catch_unwind` does not regain control: the process exits 101 and the helper does
not print its post-catch message
`the GUI backend panicked while opening the window.` Treat winit event-loop
creation panics as not adequately contained by the current `catch_unwind` guard.
The panic is attributable to `iced_winit`/`winit` event-loop creation on the main
thread, before any renderer worker thread or wgpu adapter/device path is active.

### Phase 05 recommendation

Phase 05 should not chase an iced/wgpu auto-fallback bug for the observed
exit-101 failure. The helper's features are correct and iced 0.14 falls back
from recoverable wgpu compositor errors to tiny-skia when both renderers are
enabled and no explicit incompatible `ICED_BACKEND` is requested.

The required Phase 05 fix is runtime-environment durability: installed and
non-direnv launches must provide the Wayland/X11 and related dynamic libraries
needed by winit so event-loop creation does not panic before renderer fallback.

For renderer policy, forcing tiny-skia remains a reasonable simplification for
this one-shot 420x230 modal because it removes avoidable GPU dependency and
still works in the verified environment. It is not required to fix the current
exit 101, but it is a pragmatic hardening step if Phase 05 wants the smallest
runtime surface. A more complex pre-flight wgpu probe is not warranted for this
helper.
