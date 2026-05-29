# Phase 05 — Retire in-VM login internals + CLI/docs/tests + live verify

> **Recommended Codex model: GPT 5.5 medium**
>
> Mostly mechanical removal of the now-dead steamcmd/GUI login internals plus
> CLI-help/docs/test updates, then an operator-run live verification. Bounded and
> well-specified once Phase 04 has landed the mint engine; the judgement is making
> sure nothing still-used is deleted (steamcmd/tmux helpers may be shared with the
> `steam guard` fallback or game-run Steam start). Bounded scope → `medium`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase 04 being green.** Edits
`src/game/steam.rs`, `src/ui/cli.rs`, and `docs/`. Includes an operator-run live
verification on the host (one real Guard code).

## Goal

The login path is mint-only: the dead in-VM steamcmd-bootstrap + Steam-GUI-
validation internals are removed (or clearly gated off the login path), CLI help
and docs describe the headless mint flow, tests cover it, and a real
`cluster-ctl steam login vm-1` on the host has been verified to produce an
`Ok` session that the detector accepts — with the live evidence recorded.

## Why this matters now

After Phase 04, the old steamcmd/GUI functions are unused for login; leaving them
rots the file and confuses readers. Docs/CLI still describe the old flow. And the
plan isn't truly done until a real host login is verified end-to-end (the one step
that needs a Guard code and can't be unit-tested).

## Out of scope

- Game-run Steam GUI startup / re-adding Xwayland (separate effort). Note in docs
  that game runs are unaffected by this plan and the GUI-start issue persists for
  them.
- Removing the iced dialog or `GuardProvider` (still used by the mint).

## Plan

1. **Identify dead login internals** now that mint replaces them: `automated_login`'s
   old steamcmd path, `complete_guard_login_with_provider`, `finish_steam_login`,
   `steamcmd_state_sync_script`, `steam_gui_validation_script`,
   `steamcmd_bootstrap_command`, the bootstrap/guard wait scripts, and the
   `LoginOutcome::ManualCompletionNeeded` variant if unused. **Check each for other
   callers first** — `steam guard` (the fallback subcommand), `steam check`,
   `steam warm`, and game-run Steam start (`ensure_steam`) may still use some
   helpers (e.g. `graceful_stop_steam`, `ensure_steam`). Remove only what is truly
   login-only and now unused.
2. **Decide `steam guard`'s fate.** With send-keys-into-steamcmd gone from the
   login path, the standalone `steam guard` (tmux send-keys) no longer has a live
   steamcmd session to talk to. Retire `steam guard` and its Nix wrappers, OR keep
   it only if it still serves a purpose. Update `command_uses_credentials` and CLI
   parse tests accordingly.
3. **Update CLI help** (`src/ui/cli.rs` `SteamAction::Login`/`Guard`): describe the
   headless mint ("logs in by minting a Steam session token; raises the Guard
   dialog or accepts `--code`/`STEAMPIPE_GUARD_CODE`; no in-VM Steam GUI").
4. **Update docs** (`docs/src/configuration/steam-credentials.md`, README steam
   sections, the overhaul plan's status): the login flow is now host-side token
   mint; the in-VM Steam GUI is not used for login; note the separate game-run
   GUI-start caveat. Wire any new pages into `docs/src/SUMMARY.md`.
5. **Tests:** update/extend — remove tests pinning deleted steamcmd/GUI scripts;
   keep the mint-engine, artifact-writer, and orchestration tests; ensure
   `tests/cli_steam_standalone.rs` exact-stdout still passes (adjust strings).
6. **Operator live verification (records the final evidence):**
   - On the host, `cargo run -- steam login vm-1` (session-less): enter the emailed
     Guard code in the dialog (or `--code` for authenticator).
   - Confirm `/var/lib/steampipe/logins/vm-1/config/loginusers.vdf` (17-digit
     SteamID + PersonaName) and `config/<steamID>/local.vdf` (eyJ JWT) exist.
   - `cluster-ctl steam accounts` → `vm-1 … Ok`; decode the JWT and confirm `aud`
     includes `client` and `exp` is in the future.
   - Re-run `steam login vm-1` → skipped (fill-the-gaps); `--force` → re-mints.
   - Record all of the above in this file's `## Findings`.

## Acceptance criteria

- [ ] The login path no longer references the in-VM steamcmd bootstrap or Steam
      GUI validation; only truly-unused login-only helpers were removed (shared
      helpers retained), proven by `cargo build` + `rg` showing no dangling refs.
- [ ] `steam guard`'s disposition is decided and implemented (retired or kept) with
      CLI/tests/Nix wrappers consistent.
- [ ] CLI help and `steam-credentials.md`/README describe the headless mint flow;
      `mdbook build docs` succeeds; SUMMARY updated.
- [ ] `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
      (incl. `tests/cli_steam_standalone.rs`) green.
- [ ] **Live:** `## Findings` records a real host login that wrote the artifacts,
      `steam accounts` → `Ok`, the JWT `aud` includes `client` with a future `exp`,
      and fill-the-gaps skip + `--force` re-mint both observed.

## Files likely touched
- `src/game/steam.rs` — delete dead login internals; keep shared helpers.
- `src/ui/cli.rs` — `SteamAction` help (+ remove `Guard` if retired); parse tests.
- `docs/src/configuration/steam-credentials.md`, `README.md`, `docs/src/SUMMARY.md`,
  overhaul-plan status docs.
- `tests/cli_steam_standalone.rs` — adjust exact-stdout assertions.

## Pitfalls
- **Don't delete shared helpers.** `graceful_stop_steam`, `ensure_steam`, the
  compositor `*_setup` snippets, and `vnc`/screenshot code serve game runs/capture
  — retire only login-only code. *Recovery:* grep each symbol's callers before
  removing.
- **`steam guard` Nix wrappers.** Retiring the subcommand breaks
  `cluster-steam-guard`/`cluster-1v1-steam-guard`/persist wrappers and consumer
  flakes. *Recovery:* retire wrappers in lockstep or keep the subcommand as a
  no-op-with-message; decide explicitly.
- **Live verify needs a real Guard code.** Can't be automated; the operator runs
  it once and pastes the evidence. *Recovery:* if unavailable, mark the live boxes
  blocked and ship the code/tests; do not claim the session works without it.
- **GUI auto-login is unproven until a game run.** The mint writes what the GUI
  *should* consume, but the in-VM GUI start is separately broken (X). State this
  limitation; don't conflate "login session established" with "game can launch".

## Reference
- Engine/writer/wiring: [02-mint-engine.md](./02-mint-engine.md),
  [03-artifact-writer.md](./03-artifact-writer.md), [04-wire-login-engine.md](./04-wire-login-engine.md).
- Current internals to retire: `src/game/steam.rs` (`finish_steam_login`,
  `steam_gui_validation_script`, `steamcmd_*` helpers, `complete_guard_login_with_provider`).
- Game-run GUI-start caveat: see the stable
  [Steam Guard dialog helper](../../configuration/steam-credentials.md#steam-guard-dialog-helper)
  docs for the resolved host-dialog runtime failure mode; game-run Steam GUI
  startup is separate from the headless login mint path.
