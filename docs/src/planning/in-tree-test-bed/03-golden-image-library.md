# Phase C — Golden image library and regeneration tooling

> **Recommended Codex model: GPT-5.4 / low effort**
>
> Trivial-to-moderate leaf-role work: capture three frames from a
> running fixture, commit them to a versioned directory, and write
> a regeneration script and review checklist. Almost no design
> discretion (Phase D of `vnc-visual-testing` already specifies the
> golden-storage shape) and minimal tool composition. 5.4/low is
> the matrix-aligned tier; promote to 5.4/medium only if the
> visual-testing plan's `Phase D` schema is still in flux when
> this phase runs.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Depends on Phase A and B of
*this* plan (the fixture must boot and the workloads must run).
Soft-depends on Phase D of the `vnc-visual-testing` plan for the
golden-storage path convention; if that phase has not landed yet,
use the placeholder convention in this doc and migrate the goldens
when the schema lands.

## Goal

`tests/fixture/golden/` contains exactly three committed PNGs:

```
tests/fixture/golden/
├── foot-banner-1280x720.png         # ~50–200 KB indexed PNG
├── vkcube-frozen-1280x720.png       # ~200–600 KB
├── wgpu-checker-1280x720.png        # ~5–20 KB (lots of repetition)
└── MANIFEST.toml
```

Each PNG was captured from a healthy fixture VM at a pinned commit;
`MANIFEST.toml` records the commit SHA, the nixpkgs revision, the
capture date, the sha256 of each file, and the human reviewer who
blessed the frame. `cluster-ctl fixture golden regenerate` produces
candidate replacement PNGs that a human reviews before committing.

## Why this matters now

Phase B produces deterministic on-screen content. That determinism
is only useful if we have a reference frame to compare future
captures against. Without a committed golden:

- Phase A of `vnc-visual-testing` (VNC client correctness) has no
  way to assert "the bytes I captured are right" — only "the bytes
  I captured are not all the same colour."
- Phase E of `vnc-visual-testing` (validation engine) cannot run
  any meaningful SSIM comparison.
- Phase D of *this* plan (diagnostic ladder) cannot do its
  "compare actual to golden" step.

The cost of deferring this phase is that every subsequent visual
test produces "looks reasonable to a human reviewing the PR" instead
of "passes an automated check." That's the kind of slip that turns
into "this used to work but nobody can say when it broke."

## Out of scope

- Implementing the SSIM comparator. That's Phase E of
  `vnc-visual-testing`.
- Generating goldens at every supported resolution. One resolution
  (1280×720) per workload is enough until somebody asks for more.
- Per-VM goldens. The fixture is single-VM; one golden per
  workload covers it.
- Cross-host determinism (golden captured on host A still matches
  on host B). May or may not hold depending on virtio-gpu /
  mesa pinning; we record observations and revisit if it bites.
- Golden encryption / signing. Goldens are public test content.
- LFS / git-annex storage. PNGs are small (max ~600 KB each); plain
  git is fine.

## Plan

1. **Pin the capture environment.** Before generating goldens,
   confirm:
   - The steampipe repo is at a clean commit (no uncommitted Rust
     changes that could affect the capture path).
   - The fixture's `flake.lock` is committed.
   - The host's nixpkgs is identical to the fixture's pinned
     input (or note any divergence).
   Record all three in a one-line note at the top of the run.
2. **Add the feature-gated Rust regeneration command.** The canonical entrypoint is:
   ```bash
   cargo run --features fixture-tools -- fixture golden regenerate
   ```
   `tests/fixture/golden/regenerate.sh` may remain as a compatibility
   shim around this command. The command writes candidates to `golden/.candidate/`, never
   directly to `golden/`. Promotion to canonical requires a
   manual `mv` (or, when Phase E of `vnc-visual-testing` lands,
   `cluster-ctl visual bless`).
3. **Run the command.** Produce three candidate PNGs. Verify each:
   - Opens in `feh`, `xdg-open`, or a browser without errors.
   - Has dimensions exactly 1280×720.
   - Contains the expected content (banner text legible; cube
     visible; chequerboard visible).
   - Doesn't have transparent regions where there should be
     content (rules out partial-frame captures even though we used
     grim, not VNC).
4. **Compress.** Run `optipng -o5` (or `oxipng -o4`) on each PNG
   to minimize git footprint. Re-verify dimensions after
   compression.
5. **Capture metadata.** For each PNG record:
   - sha256.
   - File size.
   - PNG `IHDR` dimensions and colour type.
   - Originating git commit (steampipe trunk SHA).
   - Capture date.
   - Reviewer (name of the human who confirmed the visual
     content).
6. **Write `tests/fixture/golden/MANIFEST.toml`** with that
   metadata:
   ```toml
   [meta]
   captured_at = "2026-05-11"
   steampipe_commit = "<sha>"
   fixture_flake_lock_sha = "<sha-of-flake.lock>"
   reviewer = "<name>"

   [[golden]]
   name = "foot-banner-1280x720"
   path = "foot-banner-1280x720.png"
   sha256 = "<…>"
   resolution = { width = 1280, height = 720 }
   workload = "foot-banner"
   capture_backend = "grim"

   # … same for vkcube-frozen and wgpu-checker
   ```
7. **Add the feature-gated Rust verification command**
   (`cargo run --features fixture-tools -- fixture golden verify`) that reads
   `MANIFEST.toml`, computes sha256 of each listed file, and
   fails if any drift. Useful in CI later (Phase E of
   `vnc-visual-testing` will replace this with a real SSIM run,
   but the hash check is the cheap canary). `golden-verify.sh`
   may remain as a compatibility shim around this command.
8. **Commit.** Single commit titled "tests/fixture: add golden
   image library v1" with the three PNGs, the manifest, the
   regenerate script, and the verify script. Reviewer in commit
   trailers.
9. **Document.** In `tests/fixture/README.md` add a
   `## Golden images` section explaining: what they're for, how
   to regenerate, why direct edits are forbidden (always go
   through `cluster-ctl fixture golden regenerate` + manual review), and how to bump
   the manifest when goldens legitimately change.

## Acceptance criteria

- [ ] `tests/fixture/golden/` contains exactly the four committed artifacts
      listed in Goal (three PNGs + MANIFEST.toml), plus optional compatibility
      shims (`regenerate.sh`, `golden-verify.sh`).
- [ ] Each PNG opens, is 1280×720, and visually contains the
      expected content for its workload (manual review;
      reviewer name recorded in MANIFEST).
- [ ] `sha256sum tests/fixture/golden/*.png` matches the
      `sha256` fields in `MANIFEST.toml`.
- [ ] `cargo run --features fixture-tools -- fixture golden verify` exits 0.
- [ ] Each PNG is under 800 KB after `oxipng -o4` / `optipng -o5`.
- [ ] `cargo run --features fixture-tools -- fixture golden regenerate` runs end-to-end on a
      clean machine and produces candidates under
      `golden/.candidate/` without touching committed files.
- [ ] `.gitignore` excludes `tests/fixture/golden/.candidate/`.
- [ ] `tests/fixture/README.md` has a `## Golden images`
      section covering regeneration, review, and manifest
      bumping.
- [ ] `cargo test --workspace` still green;
      `nix flake check ./tests/fixture` still green.

## Files likely touched

- `tests/fixture/golden/foot-banner-1280x720.png` (new, binary).
- `tests/fixture/golden/vkcube-frozen-1280x720.png` (new, binary).
- `tests/fixture/golden/wgpu-checker-1280x720.png` (new, binary).
- `tests/fixture/golden/MANIFEST.toml` (new).
- `tests/fixture/golden/regenerate.sh` (optional compatibility shim).
- `tests/fixture/golden/golden-verify.sh` (optional compatibility shim).
- `tests/fixture/README.md` (extend with `## Golden images`).
- `.gitignore` — add `tests/fixture/golden/.candidate/`.

## Pitfalls

- **Capturing under VNC instead of grim.** The current VNC
  capture path is buggy (the whole reason for the
  `vnc-visual-testing` plan). Goldens captured via VNC would
  encode those bugs as canon. Use `grim` exclusively for golden
  capture in this phase. VNC captures get compared *against*
  goldens, not the other way around.
- **Capturing before the workload has actually painted.** Three
  seconds of `sleep` is a placeholder; if the compositor hasn't
  presented the workload's first frame, the golden is sway
  wallpaper. Verify each candidate by eye before committing.
  Phase B of `vnc-visual-testing` will replace the sleep with a
  sway-IPC gate.
- **`grim` includes the cursor.** Disable cursor capture
  (`grim -c off` is not a real flag; use a sway autostart line
  `seat seat0 hide_cursor 5000` or a small `swaymsg` to warp
  the cursor off-screen before capture). Symptom: cursor in the
  golden makes pixel-exact comparison flaky.
- **PNG metadata drift.** `grim` embeds creation timestamps;
  `optipng` strips them by default but version differences can
  leave residual differences. Always compare *visual content*
  hashes by stripping all ancillary chunks
  (`oxipng --strip all`), not raw file hashes.
- **Compression breaks colors.** `optipng -o7` and `oxipng -o6`
  can choose indexed-color mode that quantizes subtly. After
  compression, do a final eye review.
- **Reviewer skips actually looking at the PNG.** The review
  step is non-trivial: at least one human must open all three
  files and confirm the content matches the expected workload.
  Symptom of skipping: a "stub-looking" golden (e.g. mostly
  black with a white pixel) gets committed and silently
  succeeds future comparisons against equivalent stubs.
- **Cross-machine reproducibility is not guaranteed.** Different
  mesa / virtio-gpu drivers may produce sub-pixel differences in
  vkcube and wgpu-checker. Note in the MANIFEST any cross-host
  observations. If goldens prove non-portable, the `vnc-visual-
  testing` plan's tolerance configuration will accommodate that
  — do not over-engineer now.

## Reference

- Originating context: in-chat 2026-05-11 — three-workload
  spec emerged from the "let's not entangle with regicide"
  decision.
- Golden storage schema source of truth (when it lands):
  [../vnc-visual-testing/04-visual-profile-config.md](../vnc-visual-testing/04-visual-profile-config.md).
  If that phase has shipped, follow its `golden_dir` convention
  exactly; if not, the path layout in this phase is the
  placeholder and gets migrated when that schema merges.
- Related phases in *this* plan: A (fixture must boot — done),
  B (workloads must run — done), D (diagnostic recipes consume
  these goldens). All three of those are prerequisites or
  consumers — none block this phase except A and B.
