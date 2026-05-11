# Phase A — Fix VNC client correctness in `capture_vnc_framebuffer`

> **Recommended Codex model: GPT-5.4 / medium effort**
>
> Bounded protocol-level bug fix in one file with adjacent test coverage.
> Moderate complexity (RFB encoding semantics, partial-update aggregation,
> retry/backoff) but no design discretion — the correct behaviour is dictated
> by the RFB spec and the wayvnc server. Routing to 5.4/medium gives enough
> reasoning headroom for the protocol details while keeping cost down; a
> mini-tier model would likely produce a "first PutPixels wins" regression
> identical to the current bug.

## Working tree

`/data/nvme0/can/Projects/steampipe` — same repo as the rest of the plan.
No external repos. This phase touches one Rust file plus its unit tests, so it
can be developed on a feature branch independent of phases B–G.

## Goal

`capture_vnc_framebuffer` produces a complete, correct, non-truncated RGBA
framebuffer for any wayvnc-served sway session at common resolutions
(640×480, 1280×720, 1920×1080), or returns a descriptive error explaining
which RFB invariant was violated. "First-EndOfFrame-wins" with partial rect
coverage is no longer a success path.

## Why this matters now

The originating symptom: Regicide UI-full VMs return PNGs that look like
noise even when the compositor is healthy. Reading
[src/game/capture.rs:177-202](../../../../src/game/capture.rs#L177-L202),
the loop saves the framebuffer on the first `EndOfFrame` after any
`PutPixels`, regardless of whether every screen rectangle has arrived. Any
short read, dropped rect, or split incremental update produces a partial
image written on top of `vec![0; …]` — the "noise" is actually black-with-a-
strip, which then *passes* `validate_image_nonblank` if there are enough
distinct pixels in the strip. Without fixing this, every downstream phase
(golden compare, scene scripting, reports) inherits garbage frames as input.

Additional sub-bugs to fix while we're in the file:

- Only `Raw` encoding is requested ([capture.rs:160-162](../../../../src/game/capture.rs#L160-L162));
  on full-HD this is slow and increases the chance of a stalled partial
  update. wayvnc supports `Zrle` and `Tight`.
- `request_update(incremental=false)` is called exactly once. If the deadline
  elapses without a complete frame, we time out instead of retrying with a
  fresh full update.
- No diagnostics in the failure path — operators see "VNC framebuffer update
  timed out" with no clue whether 0% or 90% of the screen was received.

## Out of scope

- DES / `vencrypt` / TLS auth (still accept `None` only — wayvnc is
  loopback-restricted by the cluster bridge).
- Compositor readiness gating (Phase B).
- Encoding-tuning beyond enabling `Zrle` + `Tight` + `Raw` fallback.
- Replacing the `vnc` crate. We work with what's vendored unless it cannot
  be made correct, in which case stop and escalate before swapping.
- Any change to the `Grim` capture path.
- Any change to `wayvnc_start_script` (Phase B owns that).

## Plan

1. **Reproduce the partial-frame failure deterministically.** Write a unit
   test that feeds a `MockVncClient` (or `tokio::io::duplex`-backed fake
   server) two `PutPixels` rects covering only the top-left quadrant
   followed by `EndOfFrame`, and asserts the current `capture_vnc_framebuffer`
   returns success with a half-black image. This pins the bug before fixing.
2. **Track rectangle coverage.** Maintain a coverage mask
   (`Vec<bool>` of width×height, or row-spans, or a `rectangles_pending`
   counter from `FramebufferUpdate.number_of_rectangles` — pick whichever the
   `vnc` crate exposes). Only treat a frame as complete when every pixel of
   the requested rect has been written by at least one `PutPixels`.
3. **Loop until complete or deadline.** On `EndOfFrame` with incomplete
   coverage, issue another `request_update(incremental=true)` and continue
   reading. Cap total attempts at 5 to avoid infinite loops against a
   misbehaving server.
4. **Enable better encodings.** Call `set_encodings(&[Encoding::Zrle,
   Encoding::Tight, Encoding::Raw])`. Confirm the crate supports the first
   two; if not, document that fact in a code comment and stay on Raw.
5. **Improve timeout diagnostics.** On timeout, include in the error:
   percentage of rectangle area covered, number of `PutPixels` events
   received, number of `EndOfFrame` events received, last RFB error if any.
6. **Tighten `validate_image_nonblank`.** Add a second check: reject if any
   contiguous row-band of height ≥ 32 pixels is entirely zero-alpha or
   entirely the same colour as `rgba[0..4]`. The current 32-distinct-pixel
   heuristic passes images that are 95% black plus a thin strip.
7. **Add a "partial coverage" property test** with `proptest` or hand-rolled
   cases: random rect splits of a 320×240 frame; assert that any permutation
   of rects covering the full area produces an image identical to the
   monolithic-rect baseline (up to pixel-format conversion).
8. **Run the regression smoke** described in
   [docs/migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md):
   `cluster-ctl --vm-count 1 screenshot --screenshot-backend vnc`
   against a live `cluster-1v1-uifull-stage1-up` cluster. The frame should
   no longer be noise — even if the game has not rendered, you should see
   the sway wallpaper, not a random strip.

## Acceptance criteria

- [ ] `cargo test -p steampipe game::capture` — all existing tests still pass
      and at least 3 new tests added: partial-rect failure repro,
      partial-rect-then-complete success, timeout error includes coverage %.
- [ ] `cargo clippy --all-targets -- -D warnings` is clean for
      `src/game/capture.rs`.
- [ ] Manual smoke against a running 1280×720 sway session: capture
      produces a PNG whose top, middle, and bottom rows all contain
      non-zero alpha pixels (verifiable with
      `cargo run --release -- screenshot --screenshot-backend vnc`
      then `python -c 'import PIL.Image; …'` row sampling, or a one-shot
      check baked into the new test as an integration helper).
- [ ] Manual: artificially kill wayvnc mid-capture (e.g. `pkill -STOP
      wayvnc` from another SSH session at 50% progress) — the error
      message reports coverage percentage and rectangle count, not just
      "timed out".
- [ ] `cargo build --release` succeeds.

## Files likely touched

- [src/game/capture.rs](../../../../src/game/capture.rs) — `capture_vnc_framebuffer`,
  `validate_image_nonblank`, `blit_vnc_pixels`, plus new helper structs/tests.
- [Cargo.toml](../../../../Cargo.toml) — only if `proptest` dev-dependency
  needs adding; skip if hand-rolled cases are sufficient.

## Pitfalls

- **vnc crate API surface unknown until you read it.** The exact name of
  the rectangle-count field, the encoding enum variants, and whether
  `set_encodings` accepts a server-unsupported encoding silently differ by
  version. Read `Cargo.lock` for the pinned version, then `cargo doc
  --open` for that crate before you start coding. Symptom of skipping this:
  compile errors after step 2. Recovery: pull the docs, adjust step 2 to
  the actual API.
- **Server returns smaller rects than you requested.** wayvnc legitimately
  sends multiple `PutPixels` covering the requested area. Don't treat each
  rect as "the whole frame." Coverage tracking must be additive.
- **Incremental updates may return zero rects.** When nothing changed since
  the last update, wayvnc sends `FramebufferUpdate` with 0 rects followed
  by `EndOfFrame`. Your retry loop must treat that as "still waiting,"
  not "done." Symptom: success path returns a still-incomplete frame after
  retry. Cause: misinterpreting "no rects" as "no more rects coming."
  Recovery: distinguish "0 rects this update" from "coverage reached
  100%."
- **Wide pixel formats.** If `set_format` is silently ignored, you may
  receive 16bpp pixels into a buffer sized for 32bpp. Validate
  `bits_per_pixel` on every `PutPixels`. Symptom: stripe artifacts that
  the test catches but production doesn't. Recovery: assert
  `format.bits_per_pixel == 32` after `set_format` and bail with a
  descriptive error otherwise.

## Reference

- Originating diagnosis: chat overview of 2026-05-11 — gap #3 "VNC client
  correctness."
- Downstream symptom log:
  [docs/migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md).
- RFB protocol spec, sections 7.6 (`FramebufferUpdate`), 7.7 (encodings).
- Related phases: Phase B (compositor readiness — independent code path,
  same file; coordinate rebases). Phase E (golden-image compare —
  depends on this phase producing correct frames). Phase G (capture
  lifecycle — depends on retry semantics defined here).
