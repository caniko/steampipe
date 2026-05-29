# Phase 03 — Session artifact writer (VDF) → loginStateDir share

> **Recommended Codex model: GPT 5.5 high**
>
> The artifacts must satisfy two independent consumers — the repo's session
> detector AND the real Steam GUI's auto-login — and the GUI's `local.vdf` token
> format is finicky and under-documented. Getting `loginusers.vdf` flags or the
> token nesting subtly wrong yields a "passes our detector but the GUI re-prompts"
> half-success. Format-correctness against an opaque consumer at a sub-agent role
> → `high`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase 01** for the exact token
shape; can run in parallel with Phase 02 (disjoint new file). Adds a writer module
+ detector round-trip tests.

## Goal

Given a `MintedSession` (steam_id64, account_name, persona_name, refresh_token,
expires_at) and a target `loginStateDir/vm-N/`, atomically write the VDF artifacts
so that BOTH (a) `classify_session_status` reports `Ok`, and (b) the real Steam
GUI client would auto-login from them without prompting: `config/loginusers.vdf`
(SteamID + AccountName + PersonaName + RememberPassword/AllowAutoLogin/MostRecent)
and `config/<steamID>/local.vdf` carrying the refresh-token JWT in the structure
the client reads.

## Why this matters now

The mint engine (Phase 02) yields an in-memory session; nothing is usable until
it lands on disk in the precise format. This is the contract surface between our
code and Valve's client, and the detector — get it right once, with tests, so
Phase 04 just calls it.

## Out of scope

- The auth/mint itself — Phase 02. The orchestration/CLI wiring — Phase 04.
- Making the VM's Steam GUI actually *start* (the X issue) — out of plan scope;
  this phase only writes what the GUI *would* consume.

## Plan

1. **Pin the detector contract** (read `src/game/accounts.rs`): `loginusers.vdf`
   needs a top-level `"<17-digit steamID64>"` block with `"PersonaName" "..."`
   (and conventionally `AccountName`, `RememberPassword "1"`, `AllowAutoLogin "1"`,
   `MostRecent "1"`, `Timestamp`); the refresh JWT must appear as a quoted
   `"eyJ..."` value in `config/<steamID>/local.vdf` (preferred) or `config.vdf`.
2. **Determine the real GUI `local.vdf` shape** from Phase 01 evidence / a real
   Steam profile: the modern client stores the refresh token under a
   `MachineUserConfigStore`-style tree keyed by SteamID (e.g.
   `"<steamID>" { "...": { "RefreshToken" "eyJ..." } }` — capture the exact keys
   from a known-good profile if available; otherwise document the assumption and
   make the detector-satisfying minimum the floor).
3. **Implement `src/game/steam_session.rs`** with
   `write_session(login_state_dir: &Path, vm: &str, session: &MintedSession) -> anyhow::Result<()>`:
   - compute SteamID64; create `loginStateDir/vm-N/config/` and
     `config/<steamID>/`.
   - write `loginusers.vdf` (escaping VDF strings) with the keys above.
   - write `config/<steamID>/local.vdf` with the refresh token.
   - **atomic writes** (temp file + rename) so a partial write never leaves a
     half-valid session; set restrictive perms (the share is operator-private).
4. **Round-trip tests** (pure-filesystem, no network): write a synthetic
   `MintedSession` (with a JWT carrying a future `exp`) to a tempdir and assert
   `classify_session_status` returns `Ok`, `read_account_info` yields the right
   persona/steam_id/expiry, and `find_refresh_token_file` finds the JWT in
   `local.vdf`. Add a second test for the `config.vdf` fallback path.
5. **Idempotency/overwrite:** writing again (e.g. `--force` re-mint) cleanly
   replaces prior artifacts.

## Acceptance criteria

- [ ] `write_session` produces `config/loginusers.vdf` + `config/<steamID>/local.vdf`
      under `loginStateDir/vm-N/` with correct VDF escaping and atomic writes.
- [ ] A pure-filesystem test proves the written artifacts make
      `classify_session_status` → `Ok` and `read_account_info` returns the correct
      persona, 17-digit SteamID, and a positive `expires_in_days`.
- [ ] The `local.vdf` structure matches a real Steam client's refresh-token layout
      (documented in `## Findings`/comments with the source), not just the
      detector's minimum — so the GUI would auto-login (to be confirmed live in
      Phase 05).
- [ ] Re-writing (force re-mint) replaces artifacts without leaving stale/partial
      files; perms are restrictive.
- [ ] `cargo build`/`clippy`/`test` green.

## Files likely touched
- `src/game/steam_session.rs` *(new)* — the writer.
- `src/game/mod.rs` — module declaration.
- `src/game/accounts.rs` — tests only (round-trip), no behavior change.

## Pitfalls
- **Detector-pass ≠ GUI-login.** Writing only what `accounts.rs` greps for can
  satisfy our detector while the real client still re-prompts (wrong `local.vdf`
  tree or missing `RememberPassword`/`AllowAutoLogin`). *Recovery:* mirror a
  real profile's structure; verify live in Phase 05.
- **VDF escaping.** Account/persona names with quotes/backslashes must be escaped;
  the SteamID block key is the 17-digit ID as a quoted string. *Recovery:* a small
  VDF-escape helper + a test with a tricky persona.
- **SteamID3 vs SteamID64.** loginusers.vdf uses SteamID64 (17 digits); the
  `userdata/<accountid>/` tree uses SteamID3 (the lower 32 bits). Don't confuse
  them. *Recovery:* compute both explicitly; the detector wants SteamID64.
- **Non-atomic write race.** A crash mid-write could leave a loginusers.vdf with a
  SteamID but no token (or vice-versa) → detector flaps. *Recovery:* temp+rename
  both files; write the token file before loginusers.vdf so the session only
  "appears" once complete.

## Reference
- Detector: `src/game/accounts.rs` (`parse_loginusers_file`,
  `find_refresh_token_file`, `extract_refresh_jwt`, `jwt_exp_unix`).
- Phase 01 token/format evidence: [01-mint-feasibility-spike.md](./01-mint-feasibility-spike.md).
- Consumed by Phase 04; pairs with Phase 02 (mint engine).
