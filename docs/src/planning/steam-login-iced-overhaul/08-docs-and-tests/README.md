# Phase 08 — Docs, CLI help, tests, and plan retirement

> **Recommended Codex model for merge/orchestration: GPT 5.5 medium**
>
> The phase-level merge is light (two disjoint sub-layers touching different file
> trees), but closing it out includes retiring the prior planning docs and
> confirming the whole-set acceptance criteria hold — a coordination judgement
> best at `medium`.

## Sub-layers

| # | Slug | Model | Touches | Sub-layer file |
|---|------|-------|---------|----------------|
| 01 | docs-rewrite | 5.5 medium | `docs/src/configuration/steam-credentials.md`, `README.md`, `website/content/_index.md`, `docs/src/getting-started/*`, `docs/src/SUMMARY.md` | [sub-01-docs-rewrite.md](./sub-01-docs-rewrite.md) |
| 02 | tests-and-cli-help | 5.4 medium | `tests/cli_steam_standalone.rs`, new login tests, `src/ui/cli.rs` help strings | [sub-02-tests-and-cli-help.md](./sub-02-tests-and-cli-help.md) |

These two streams touch disjoint files (prose/`docs` + `website` vs `tests` +
`cli.rs` help text), have independent acceptance subcriteria, need no
mid-execution communication, and can be retried independently — so they fan out as
sub-layers. The user dispatches them in parallel and merges.

## Goal (phase-level)

All user-facing surfaces describe the new default flow (auto-steamcmd + host iced
dialog, single invocation, headless fail-fast) and the documented fallbacks
(`steam guard --code`, VNC if retained); the test suite covers the new typed
outcomes and the fail-fast contract and is green; and the shipped login
lifecycle behavior remains covered by stable docs.

## Why this matters now

Phases 01–07 changed behavior the docs and tests still describe the old way of.
The dossier's incompatibility #14 catalogues the doc surfaces that enshrine the
VNC two-phase model (`steam-credentials.md`, README, website), and the critic
flagged that `tests/cli_steam_standalone.rs` asserts exact stdout and has **no**
login/guard coverage. Leaving these stale ships a product whose docs and tests
contradict its behavior.

## Out of scope

- Any behavior change to the login code (Phases 01–07 own it). If a doc/test
  reveals a behavior bug, file it back against the relevant phase; don't fix
  behavior here.
- Removing `steam guard` docs entirely — document it as the fallback, don't delete.

## Merge plan

The user dispatches sub-01 and sub-02 in separate sessions; they edit disjoint
files so the merge is a clean union with no conflicts. After both land:
1. Confirm `docs/src/SUMMARY.md` has both the new flow docs and (sub-02's) nothing
   conflicting — only sub-01 edits SUMMARY.
2. Run `mdbook build docs` and `cargo test` together to confirm the merged state
   is green.
3. Confirm the prior lifecycle behavior remains covered by stable docs.

## Phase-level acceptance criteria

- [ ] `docs/src/configuration/steam-credentials.md` describes auto-steamcmd + host
      dialog as the default, the `--code`/`STEAMPIPE_GUARD_CODE` and headless
      fail-fast contract, and VNC/`steam guard` only as fallbacks; README +
      website copy match.
- [ ] `tests/cli_steam_standalone.rs` passes with any updated exact-stdout
      strings, and new tests cover the typed `GuardCodeNeeded` outcome + fail-fast
      (no-display/`--non-interactive`) path; `cargo test` is green.
- [ ] `src/ui/cli.rs` help strings for `SteamAction::Login`/`Guard` reflect the
      new flow (no "with VNC" on Login; Guard documented as fallback).
- [ ] `mdbook build docs` succeeds; `docs/src/SUMMARY.md` surfaces the new docs.
- [ ] The shipped lifecycle behavior is preserved in stable docs; this
      overhaul's plan set is referenced from the dossier.
- [ ] Whole-set acceptance criteria in the plan README are re-checked and pass.

## Reference

- Dossier: incompatibility #14 (docs/UX); critic gap (`cli_steam_standalone.rs`
  coverage); "Existing Plan Status" (stable lifecycle docs).
- Depends on Phases 01–07. Run sub-layers in parallel, then merge.
