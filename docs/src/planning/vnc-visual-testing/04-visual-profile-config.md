# Phase D — Structured visual profile config and golden-image storage

> **Recommended Codex model: GPT-5.5 / medium effort**
>
> Complex design phase with orchestrator role: the schema decisions made
> here ripple into phases E (validation engine), F (scene scripting), and G
> (capture lifecycle + report). A wrong shape costs rework across three
> downstream phases. 5.5 at medium gets the design discretion and
> long-horizon coherence (lots of cross-references) at less than half
> the cost of 5.5 high. Mini-tier is not appropriate — the work is
> design-heavy, not edit-heavy.

## Working tree

`/data/nvme0/can/Projects/steampipe`. New code lives mostly in
[src/core/config.rs](../../../../src/core/config.rs) with a small
addition to [steampipe.toml](../../../../) (no actual root toml — the
schema docs in [docs/src/configuration/](../../configuration/) need
updating). Independent of phases A/B/C in code. Phase E **depends** on
this phase landing — Phase E should not start coding the validator
until the schema is merged or pinned.

## Goal

`steampipe.toml` gains a structured `[visual]` block and one or more
`[[visual.scene]]` entries that describe:

- Where golden images live on disk.
- What window/region each scene targets.
- Tolerances per scene (SSIM threshold, max pixel delta, ignored mask
  polygons, expected resolution).
- Which compositor backend each scene applies to (sway vs weston).

The existing `visual_validator` field on profile becomes one of
multiple validator kinds (`shell` / `golden` / `none`), so legacy
configs keep working. A new `cluster-ctl visual record <scene>` /
`bless <scene>` flow is specified at a contract level (implementation
in Phase E).

## Why this matters now

Today `visual_validator` is one shell string per profile
([src/main.rs:54-67](../../../../src/main.rs#L54-L67)). Every project
that wants real visual testing has to hand-roll a script that knows
about goldens, masks, thresholds, scene names — there is no contract.
This blocks two things: (1) Regicide cannot start writing visual
assertions without first picking a directory convention and a
threshold story; (2) Phase E cannot build the validation engine
without knowing how scenes are addressed.

The schema chosen here also defines how Phase G's report finds
golden vs actual images, and how Phase F's scene scripts reference
the scene they're driving to.

## Out of scope

- Implementing SSIM / pHash / OCR / template matching — Phase E.
- Writing actual `[[visual.scene]]` entries for Regicide — that's
  a downstream consumer task.
- Adding migration helpers from the legacy `visual_validator`
  string. Document the migration in this phase's PR description but
  do not break compatibility — both shapes must coexist.
- Encrypted goldens / LFS-backed goldens. Treat goldens as ordinary
  files in the repo for now.

## Plan

1. **Draft the TOML schema** as a doc-first artefact in
   [docs/src/configuration/visual-testing.md](../../configuration/)
   (new file; also added to SUMMARY.md as part of this phase). Show
   one fully-populated example covering every field. The example
   doubles as a parser test fixture.
   ```toml
   [visual]
   golden_dir = "tests/visual/golden"   # repo-relative
   diff_dir   = "tests/visual/diff"     # gitignored; produced output
   default_tolerance = { ssim = 0.985, max_pixel_delta = 8 }
   default_backend   = "vnc"            # grim or vnc

   [[visual.scene]]
   name        = "main-menu"
   description = "Steam launches, game shows main menu"
   window      = { app_id = "regicide" }
   resolution  = { width = 1280, height = 720 }
   backend     = "vnc"
   tolerance   = { ssim = 0.97, max_pixel_delta = 16 }
   mask        = [ { x = 0, y = 0, w = 1280, h = 32 } ]   # ignore titlebar
   roi         = { x = 64, y = 64, w = 1152, h = 600 }
   wait_for    = { window_visible = true, min_age_ms = 500 }
   ```
2. **Add Rust types** in [src/core/config.rs](../../../../src/core/config.rs):
   `VisualConfig`, `Scene`, `WindowTarget`, `Tolerance`, `MaskRect`,
   `Roi`, `WaitFor`. Use `serde` with `#[serde(default)]` on every
   field. All paths are stored as `PathBuf` and resolved relative to
   the project root.
3. **Path resolution.** Add `VisualConfig::golden_path(scene_name)`
   and `diff_path(scene_name, run_label)` helpers. Goldens live at
   `{golden_dir}/{scene}.png`; diff outputs at
   `{diff_dir}/{run_label}/{scene}.{actual,diff,golden}.png`.
4. **Backwards compatibility.** Keep
   `ProjectConfig::profile.visual_validator` working. If both a
   legacy `visual_validator` and a new `[visual]` block exist for
   the same profile, prefer the new block but emit a deprecation
   warning to stderr.
5. **Validator kind enum.** Extend the profile schema with
   `validator_kind = "shell" | "golden" | "none"`. If unset, infer
   from presence of `visual_validator` (shell) vs `[visual]`
   (golden) vs neither (none). Document the precedence rules in
   the new config doc.
6. **CLI surface contract** (no implementation yet — Phase E
   implements):
   - `cluster-ctl visual list` — list configured scenes.
   - `cluster-ctl visual record <scene> [--vm vm-N]` — capture
     current frame and write it to `golden_dir`.
   - `cluster-ctl visual bless <scene> [--from run-N]` — promote
     the actual frame from a failed run to the new golden.
   - `cluster-ctl visual diff <scene> [--from run-N]` — re-run
     the comparison against an already-captured actual frame.
   Add stub subcommands in `cli.rs` that return `unimplemented!()`
   with a clear "implemented in Phase E" message; Phase E removes
   the stubs.
7. **Update [docs/src/SUMMARY.md](../../SUMMARY.md)** to include
   the new visual-testing config doc.
8. **Parser tests.** Round-trip the example TOML through
   `toml::from_str` / `to_string` and assert structural equality.
   Negative tests: invalid mask (negative width), unknown
   `validator_kind`, missing `golden_dir` when a scene exists, scene
   name with `/` or `..` (path safety).

## Acceptance criteria

- [ ] `cargo test -p steampipe core::config::visual` — ≥ 8 parser
      tests, including round-trip + 3 negative cases.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] An existing project using `visual_validator = "…"` still
      builds and runs the screenshot command without warnings *if*
      no `[visual]` block is present, and with one deprecation
      warning *if* both are present.
- [ ] `cluster-ctl visual list` enumerates scenes and prints them
      as a table (`Name | Window | Backend | Tolerance`).
- [ ] `cluster-ctl visual record vm-1 main-menu` returns the
      `unimplemented!` stub message naming Phase E — verifies CLI
      wiring without making real claims about the feature.
- [ ] New doc page at `docs/src/configuration/visual-testing.md`
      contains the full example, the precedence table, and a
      "migrating from `visual_validator`" section.
- [ ] `docs/src/SUMMARY.md` updated; `mdbook build` succeeds.

## Files likely touched

- [src/core/config.rs](../../../../src/core/config.rs) — `VisualConfig`
  and friends.
- [src/ui/cli.rs](../../../../src/ui/cli.rs) — `Visual` subcommand
  enum, stubs for `record` / `bless` / `diff` / `list`.
- [src/main.rs](../../../../src/main.rs) — dispatch.
- `docs/src/configuration/visual-testing.md` (new).
- [docs/src/SUMMARY.md](../../SUMMARY.md) — link the new page.

## Pitfalls

- **Path safety.** Scene names are user-supplied; they end up in
  filesystem paths. Reject `/`, `..`, NUL, and shell metacharacters
  at parse time. Symptom of skipping: `cluster-ctl visual record
  ../../etc/passwd` overwrites arbitrary files. Recovery: a
  one-line allowlist regex `^[a-zA-Z0-9._-]+$` enforced in the
  `Scene::name` deserializer.
- **Schema bikeshedding sinks the phase.** Anchor decisions to
  consumer needs (E, F, G). When in doubt about a field, defer it to
  a future phase with a clear "reserved" comment in the doc rather
  than guessing now.
- **`#[serde(default)]` masks typos.** If a user writes `tolarance`
  instead of `tolerance`, defaults silently apply. Enable
  `#[serde(deny_unknown_fields)]` on each struct or run a
  post-parse lint that compares parsed-field-set to allowed-set.
- **`golden_dir` semantics.** Is it relative to the project root or
  to `steampipe.toml`'s directory? They're the same in current
  usage but the docs must pick one and stick to it. Use project
  root (matches `assets_dir`, `steam_api_lib`).
- **The `validator_kind` precedence rules.** Pin them in a single
  table in the new doc, *with worked examples*. Future-you will
  thank you when phase E asks "what wins if both are set."

## Reference

- Originating diagnosis: chat overview of 2026-05-11 — gap #8
  "Configuration surface."
- Current single-string surface:
  [src/main.rs:54-67](../../../../src/main.rs#L54-L67),
  [src/game/capture.rs:372-408](../../../../src/game/capture.rs#L372-L408).
- Existing config types:
  [src/core/config.rs](../../../../src/core/config.rs).
- Related phases: Phase E (consumer — depends on this schema).
  Phase F (consumes `WindowTarget` and `WaitFor`). Phase G (uses
  `diff_dir` + scene metadata in report). No conflicts with A/B/C.
