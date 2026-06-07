# Phase 08 · Sub-layer 02 — Tests & CLI help

> **Recommended Codex model: GPT 5.4 medium**
>
> Mostly mechanical test authoring + CLI help-string edits against behavior the
> earlier phases already defined. Leaf role, bounded and well-specified; a
> coding-focused workhorse tier is the right cost/quality point — no orchestration
> or novel design, so `5.4 medium` rather than `5.5`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Independent of sub-01 (disjoint files).
Assumes Phases 01–07 have landed. Does **not** edit `docs/` or `SUMMARY.md`.

## Goal

The test suite covers the new behavior — the typed `GuardCodeNeeded` outcome, the
no-code bootstrap + live-session `send-keys` submit (incl. the `--code`/env
convenience), and the headless fail-fast contract — and the existing exact-stdout
integration tests are updated to match any changed output. The
`SteamAction` CLI help strings reflect the new flow.

## Out of scope

- Docs/SUMMARY — sub-01.
- Behavior changes — Phases 01–07 (if a test exposes a bug, file it back to the
  owning phase).

## Plan

1. **Update `tests/cli_steam_standalone.rs`** exact-stdout assertions
   (`:388,417,452,498`) for any changed strings from `steam accounts`/
   `clean-logins`/`info`. Run the file first to see what drifted:
   `cargo test --test cli_steam_standalone`.
2. **Add login control-flow tests** (unit, in `src/game/` or a new integration
   test that doesn't need a real backend):
   - no-code bootstrap is `steamcmd +login '<user>' '<pass>' +quit` (no positional
     code) and the obtained code is submitted via `steamcmd_guard_submit_script`
     (`tmux send-keys`); assert no positional `+login user pass <code>` form exists
     (mirrors existing `steam.rs` command tests).
   - GuardRequired + `FailFast` (no display, `--non-interactive`) → typed
     `GuardCodeNeeded` outcome + the reserved non-zero exit code; **no** stdin
     block, **no** window.
   - Provider selection policy table (if not already covered by Phase 02's tests,
     extend here).
   - `HelperGui` exit-code mapping via a stub `STEAMPIPE_GUARD_PROMPT_BIN` (if not
     covered by Phase 04).
3. **Update `src/ui/cli.rs` help strings**: `SteamAction::Login` (`:614-617`)
   drops "with VNC" and describes the host-dialog/`--code` flow; `SteamAction::Guard`
   (`:636`) is documented as the headless fallback; document the new
   `--non-interactive`/`--no-gui` globals and the reserved guard-needed exit code.
4. Run the full suite: `cargo test`; `cargo clippy` for the lint gate.

## Acceptance criteria

- [ ] `cargo test --test cli_steam_standalone` passes with updated exact-stdout
      strings.
- [ ] New tests assert: the no-code bootstrap has no positional code and the code
      is submitted via `send-keys` into the live session; a headless GuardRequired
      returns the typed `GuardCodeNeeded` outcome + reserved exit code without
      blocking; provider selection matches the policy.
- [ ] `cluster-ctl steam login --help` / `steam guard --help` reflect the new flow
      (no "with VNC" on Login; Guard as fallback); `--non-interactive`/`--no-gui`
      documented.
- [ ] `cargo test` and `cargo clippy` (workspace lints) are green.

## Files likely touched

- `tests/cli_steam_standalone.rs` — updated exact-stdout assertions.
- New test module(s) under `src/game/` or `tests/` for the login control flow.
- `src/ui/cli.rs` — `SteamAction` help strings (`:614-617`, `:636`); global flag
  help.

## Pitfalls

- **Asserting on secret-bearing output.** Never assert on a real Guard code in
  test output; use placeholders and verify redaction. *Recovery:* assert
  `[REDACTED]` appears where a code/password would.
- **Editing `cli.rs` behavior, not just help.** This sub-layer only touches help
  strings; flag/behavior changes belong to Phases 02/07. *Recovery:* if a help
  string can't be accurate without a behavior change, file it back.
- **Integration tests needing a backend.** Login/guard end-to-end needs a real VM;
  keep new tests at the unit/command-string level (the pattern the existing
  `steam.rs` tests use). *Recovery:* assert on generated scripts/outcomes, not on
  live logins.

## Reference

- Dossier: critic gap (`cli_steam_standalone.rs` has no login/guard coverage).
- CLI help: `src/ui/cli.rs:614-617,636`. Sibling: sub-01 (docs). Phase README:
  [README.md](./README.md).
