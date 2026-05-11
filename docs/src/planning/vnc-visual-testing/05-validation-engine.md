# Phase E — Golden-image validation engine (SSIM, masks, ROI, bless)

> **Recommended Codex model: GPT-5.5 / medium effort**
>
> Complex orchestrator-tier work: integrating perceptual diff algorithms
> with the existing capture pipeline, building the bless/record/diff
> workflow, and wiring it through the validator-kind enum from Phase D.
> 5.5 at medium balances design coherence (algorithm selection,
> tolerance semantics, mask geometry) with cost. Routing to 5.5 high
> would be defensible if the SSIM tuning turns out to need iterative
> evaluation against a corpus — escalate only if that occurs.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase D.** Do not
start coding before Phase D's `VisualConfig` schema has landed on
trunk (or, if working in parallel, pin a specific Phase D commit and
note in the PR description that it must merge first). Files touched
overlap with phases A and B in `src/game/capture.rs` — coordinate by
landing A → B → E in that order; if A and B are not yet on trunk,
work on a `phase-e` branch and rebase.

## Goal

The `golden` validator kind from Phase D is fully implemented:

- A `cluster-ctl screenshot --scene <name> --validate` flow that
  captures the frame, applies the scene's mask + ROI, and compares
  against the golden image using SSIM with the configured
  tolerance.
- `cluster-ctl visual record <scene>` writes a new golden.
- `cluster-ctl visual bless <scene> --from run-N` promotes an
  actual-capture from a previous failing run to be the new golden.
- `cluster-ctl visual diff <scene>` re-runs comparison without
  re-capturing.
- A failure produces three artefacts in `diff_dir`:
  `<scene>.actual.png`, `<scene>.golden.png`, `<scene>.diff.png`
  (the diff image highlights changed pixels), plus a JSON sidecar
  with SSIM score, max delta, mask-area %, and the threshold that
  was exceeded.

## Why this matters now

The current `validate_image_nonblank`
([src/game/capture.rs:263-297](../../../../src/game/capture.rs#L263-L297))
only checks that an image isn't a flat colour. It cannot tell
"Steam main menu" from "Steam crash dialog" from "compositor
wallpaper with no game window." The downstream Regicide failure
mode is exactly this class of bug — the VM renders *something*, so
the test passes the liveness check, but the something isn't the
game. Without perceptual diff against a known-good baseline, no
amount of capture-correctness (Phase A) or readiness-gating
(Phase B) will catch a "wrong-content" failure.

This phase also unlocks Phase G's HTML report — the report has
nothing to show until per-scene comparison data exists.

## Out of scope

- OCR for textual UI assertions. Useful future work; pHash/SSIM
  cover the immediate need. Note as a "Phase E.1" follow-up in
  the PR.
- Template-matching (find-a-small-image-in-a-large-one). Same
  rationale.
- Cross-resolution comparison. If actual and golden differ in
  resolution, fail loudly with `ResolutionMismatch`; don't try to
  scale. Resizing changes pixel content and obscures bugs.
- Automatic baseline regeneration on CI. `bless` is always a human-
  initiated action.

## Plan

1. **Choose the SSIM crate.** Survey: `image-compare`, `dssim-core`,
   `ssim2`. Prefer pure-Rust with no system dependency. Pin in
   `Cargo.toml`. Record the choice rationale in a one-paragraph
   comment in `src/game/visual/ssim.rs`.
2. **Create `src/game/visual/` module.** Sub-files:
   `ssim.rs` (algorithm wrapper), `mask.rs` (rect mask application),
   `roi.rs` (ROI extraction), `engine.rs` (the top-level comparator),
   `bless.rs` (record/bless workflow), `mod.rs` (re-exports).
3. **`Engine::compare(actual: &Path, scene: &Scene) -> ComparisonResult`**.
   Steps:
   - Load both images. Assert resolutions match scene-declared
     `resolution`; if scene resolution is `None`, derive from
     golden. Mismatch → error `ResolutionMismatch`.
   - Apply ROI if set (crop both images to the same rect).
   - Apply mask: for each `MaskRect`, zero out those pixels in
     both images so they don't contribute to the score.
   - Compute SSIM. Compute max per-channel absolute pixel delta.
   - Compare to tolerance: pass requires SSIM ≥ threshold AND
     max delta ≤ configured cap.
   - Produce `ComparisonResult { passed, ssim, max_delta,
     diff_image: image::RgbaImage }`.
4. **Diff image renderer.** Highlight changed pixels: keep
   unchanged regions desaturated grey at 30% alpha; show changed
   pixels in a per-magnitude colour ramp (small delta → blue,
   large → red). Mask regions stay transparent / overlaid with a
   diagonal stripe so the report reader sees what was ignored.
5. **`run_scene_validation(config, vm, scene, run_label)`**.
   Orchestrates: capture (calling existing screenshot fns with the
   scene's `WindowTarget` + backend), compare, write diff artefacts
   to `diff_dir`, write JSON sidecar.
6. **Wire into `--validate` flag.**
   [src/main.rs:551-560](../../../../src/main.rs#L551-L560) currently
   resolves a single shell validator string. Extend the resolver to
   return one of `ValidatorPlan::Shell(String)` /
   `ValidatorPlan::Golden(VisualConfig, Vec<Scene>)` /
   `ValidatorPlan::None`, based on the profile's `validator_kind`
   from Phase D. The shell path keeps working for legacy configs.
7. **Implement the four CLI stubs from Phase D:**
   - `visual record <scene> [--vm vm-N]` — capture, save to
     `golden_dir`. Refuse to overwrite without `--force`.
   - `visual bless <scene> --from <run-label> [--vm vm-N]` — copy
     `diff_dir/<run-label>/<scene>.actual.png` → golden, with a
     confirmation prompt unless `--yes`.
   - `visual diff <scene> --from <run-label>` — re-run engine
     against existing actual.
   - `visual list` is already implemented in D; ensure it shows
     per-scene golden presence/absence with a column.
8. **Integrate with `test --capture-on-failure`.**
   [src/game/capture.rs:355-370](../../../../src/game/capture.rs#L355-L370)
   currently captures only. When the profile uses
   `validator_kind = "golden"`, also run scene comparisons for the
   failing run. Append per-scene status to test history (Phase G
   formalizes the JSON shape; for now, attach a `visual_results`
   field to the run record opaquely and let Phase G migrate it).
9. **Unit tests.** Synthetic 64×64 fixtures:
   - Identical images → SSIM ≈ 1.0, passed.
   - Single pixel changed → SSIM very close to 1.0, but max-delta
     trips when cap is 0.
   - Masked region differs entirely, unmasked identical → passed.
   - ROI extraction crops correctly and the mask coordinate space
     is post-ROI (lock this convention in a test).
   - Resolution mismatch → error.
10. **Manual smoke** on the live Regicide cluster: record a golden
    from a healthy VM (or use a manually-prepared reference PNG),
    then run validation against a known-broken VM and verify the
    diff image highlights the corrupt region rather than the entire
    frame.

## Acceptance criteria

- [ ] `cargo test -p steampipe game::visual` — minimum 10 tests
      including all five cases listed in step 9 plus 5
      bless/record/diff lifecycle tests.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] `cluster-ctl visual record vm-1 main-menu` writes
      `tests/visual/golden/main-menu.png` and refuses on second run
      without `--force`.
- [ ] `cluster-ctl visual diff main-menu --from run-1` with a
      pre-existing actual reproduces the original verdict in < 1 s.
- [ ] `cluster-ctl test --capture-on-failure --validate` on a
      cluster with `validator_kind = "golden"` produces
      `diff_dir/run-N/<scene>.{actual,golden,diff}.png` for every
      scene that failed.
- [ ] Legacy `visual_validator = "shell …"` configs run unchanged.
- [ ] JSON sidecar at
      `diff_dir/run-N/<scene>.result.json` parses with `jq` and
      contains `ssim`, `max_delta`, `passed`, `threshold` fields.

## Files likely touched

- `src/game/visual/{mod,ssim,mask,roi,engine,bless}.rs` (new).
- [src/game/mod.rs](../../../../src/game/mod.rs) — module wiring.
- [src/game/capture.rs](../../../../src/game/capture.rs) — hook into
  `capture_on_failure`, possibly extract `ScreenshotOptions::scene`.
- [src/main.rs](../../../../src/main.rs) — `ValidatorPlan` resolver.
- [src/ui/cli.rs](../../../../src/ui/cli.rs) — flesh out
  `Visual::Record / Bless / Diff` from Phase D's stubs.
- [Cargo.toml](../../../../Cargo.toml) — SSIM crate dep.

## Pitfalls

- **SSIM thresholds are scene-specific.** A static main menu wants
  ≥ 0.99; a scene with animated particles wants ≥ 0.85. Hardcoded
  defaults always fight one scene. Plan D's `tolerance` per-scene
  field is non-negotiable; verify it's plumbed through.
- **Antialiasing differences are real.** Even between two healthy
  runs, SSIM is rarely 1.000. Set the default tolerance at 0.985,
  not 1.0. Document this in the visual-testing doc.
- **Mask geometry is in *image* coordinates, not screen coordinates.**
  If the user supplies absolute screen coords but the capture is
  cropped to ROI, the mask is wrong. Decide a single convention
  (post-ROI is recommended) and assert it in a test.
- **Diff images can be huge.** A 1080p RGBA8 diff is 8 MB. For long
  test runs storing one diff per failure per scene, the `diff_dir`
  grows fast. Compress as indexed-color PNG where possible (the
  diff has at most ~16 distinct colors).
- **Bless is destructive.** Overwriting goldens with a buggy actual
  ships the bug as the new normal. Always require either an
  interactive confirmation or `--yes`, and consider git-staging the
  changed golden so review surfaces it.
- **Image crate version skew.** If the `image` crate version pinned
  by Phase A and the version expected by the chosen SSIM crate
  disagree, you get cryptic trait-mismatch errors. Check `cargo
  tree -d` early.
- **Resolution mismatches cause cryptic SSIM panics in some
  crates.** Validate dimensions before calling the algorithm.

## Reference

- Originating diagnosis: chat overview of 2026-05-11 — gap #4
  "Validation worth the name."
- Schema source of truth: Phase D
  ([04-visual-profile-config.md](./04-visual-profile-config.md)).
- Existing validator hook:
  [src/game/capture.rs:372-408](../../../../src/game/capture.rs#L372-L408).
- Existing fail-capture flow:
  [src/game/capture.rs:355-370](../../../../src/game/capture.rs#L355-L370).
- Related phases: F (scripted scenes drive the actual captures
  this engine validates), G (HTML report consumes the JSON
  sidecars and diff images).
