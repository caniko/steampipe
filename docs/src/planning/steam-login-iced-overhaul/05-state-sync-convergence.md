# Phase 05 — State-sync / session-detection convergence fix

> **Recommended Codex model: GPT 5.5 high**
>
> Complex and consequential: the dossier's highest-risk finding is a structural
> mismatch between what a steamcmd-only login writes and what session detection
> reads. If wrong, "auto-login when a session is missing" loops forever. The fix
> needs real-host verification (which `.vdf` holds the refresh-token JWT) plus
> careful detector/sync logic and a regression test — mediocre work ships an
> infinite auto-login loop that only manifests in production. Sub-agent role but
> high blast radius, so `high`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase 01** (so the new
single-invocation login flow is what produces the artifacts you verify). The host
verification step also needs **a real Steam account on the atlas host** — this
phase cannot be fully closed without it. Touches `src/game/steam.rs` (the sync
script function) and `src/game/accounts.rs` (the detector); rebase on Phase 01.

## Goal

After a steamcmd-only login, `cluster-ctl steam accounts` /
`classify_session_status` reliably reports the VM as `Ok`, so the auto-login
trigger fires exactly once and does not re-trigger on the next `up`. The state
that steamcmd writes (specifically the refresh-token JWT) is guaranteed to land
where the detector looks, by extending the sync script and/or the detector after
verifying on a real host which file holds the token.

## Why this matters now

The detector prefers a per-SteamID file the sync script never copies:

```text
src/game/accounts.rs:344-352  (find_refresh_token_file)
    config/<steamID>/local.vdf   ← checked FIRST
    config/config.vdf            ← fallback

src/game/steam.rs:1628-1666  (steamcmd_state_sync_script) copies:
    config/loginusers.vdf, config/config.vdf, registry.vdf, ssfn*
    — NEVER config/<steamID>/local.vdf
```

If a steamcmd-only login writes the modern refresh-token JWT into
`config/<steamID>/local.vdf` rather than `config/config.vdf`, the sync never
brings it to the host share, `classify_session_status` returns
`NoToken`/`Expired`, and the new "auto-login when session missing" behavior
re-triggers a full login **every time** — an infinite loop. This is verified as a
structural mismatch in the dossier; only a real-host run settles which file holds
the token. The convergence fix gates Phase 07 (don't retire fallbacks until
auto-login provably converges).

## Out of scope

- The control-flow refactor (Phase 01) and provider wiring (Phases 02/04).
- Changing what counts as `Ok`/`Stale`/`Expired` thresholds
  (`DEFAULT_WARN_WITHIN_DAYS`) — only ensure the token is *found*.
- The GUI / fail-fast contract.

## Plan

1. **Verify on a real host (atlas).** Run a fresh interactive login for one
   account on a login-runner VM and complete the Steam Guard challenge: run
   `cluster-ctl steam login vm-1`, which makes steamcmd attempt the login (this is
   what *triggers* Steam to email the code) and parks at the prompt; enter the
   emailed code at the dialog/stdin prompt; the login then completes in the same
   invocation. (Do **not** pre-supply `--code` for the email case — there is no
   code until the attempt triggers the email; `--code`/`STEAMPIPE_GUARD_CODE` only
   applies when using the on-demand mobile authenticator.) Then inspect the VM's
   `~/.local/share/Steam/config/` and the host share
   `/var/lib/steampipe/logins/<vm>/config/`:
   - Which file contains the `eyJ...` refresh-token JWT —
     `config/config.vdf` or `config/<steamID>/local.vdf` (or both)?
   - Does `cluster-ctl steam accounts` report `Ok` for that VM immediately after?
   Record the findings in this phase's acceptance evidence (paste the file
   listing + which file held the `eyJ` token).
2. **Extend the sync script** (`steamcmd_state_sync_script`, `steam.rs:1628-1666`)
   to also copy `config/<steamID>/local.vdf` from each candidate base
   (`$HOME/.steam/steamcmd`, `$HOME/Steam`, `$HOME/.local/share/Steam`) into the
   GUI config dir, preserving the `config/<steamID>/` subpath. Since the SteamID
   isn't known a priori in the script, copy the whole `config/<17-digit-id>/`
   directory (glob `config/[0-9]*/`) or specifically `local.vdf` within it. Keep
   the existing "skip if a valid login is already present" guard
   (`STEAMCMD_SYNC_SKIPPED_LOGIN_PRESENT`).
3. **Harden the detector** (`find_refresh_token_file`, `accounts.rs:344-352`) so
   it is robust to either layout: it already checks `local.vdf` then `config.vdf`
   — confirm both paths parse the JWT via `read_refresh_jwt`/`extract_refresh_jwt`
   and add the per-SteamID path coverage if the directory layout differs from the
   assumption.
4. **Add a regression test** for the detector against a fixture directory that
   contains only `config/<steamID>/local.vdf` with a JWT (no `config.vdf`),
   asserting `classify_session_status` returns `Ok` (not `NoToken`). Add a second
   fixture with only `config/config.vdf` to keep the fallback covered. These are
   pure-filesystem tests (no backend), so they run in CI.
5. **Add a sync-script unit test** asserting the generated script copies the
   per-SteamID `local.vdf` (string-match on the new copy line, mirroring the
   existing `steamcmd_state_sync_targets_only_known_gui_artifacts` test at
   `steam.rs:2358`).
6. **Confirm convergence end-to-end** on the host: after one login, a subsequent
   `cluster-ctl up` does **not** re-trigger login for that VM (fill-the-gaps skip
   fires because the token is now found).

## Acceptance criteria

- [ ] Host evidence recorded: which `config/*.vdf` file holds the `eyJ`
      refresh-token JWT after a steamcmd-only login, and a successful
      `cluster-ctl steam accounts` → `Ok` for that VM.
- [ ] `steamcmd_state_sync_script` copies the per-SteamID `config/<steamID>/local.vdf`
      (or the whole `config/<id>/` dir) to the GUI config dir — asserted by a
      unit test string-match.
- [ ] A pure-filesystem regression test proves `classify_session_status` returns
      `Ok` when only `config/<steamID>/local.vdf` (with a valid JWT) is present,
      and still `Ok` when only `config/config.vdf` is present.
- [ ] On the atlas host, a second `cluster-ctl up` after a successful login does
      **not** re-trigger login for that VM (no auto-login loop).
- [ ] `cargo build`/`clippy`/`test` green (`cargo test game::accounts`,
      `game::steam`).

## Execution Notes

2026-05-29:

- Code/test portion completed locally: the sync script now copies
  `config/<numeric-id>/local.vdf`, and detector regression tests cover
  local-only and `config.vdf`-only refresh-token layouts.
- Verified gates: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test game::accounts`, `cargo test game::steam`, and `cargo test`.
- Host verification remains blocked. This shell is on `atlas`, but
  `/var/lib/steampipe/logins` currently has no `loginusers.vdf`, `config.vdf`,
  or `local.vdf` artifacts. A fresh **interactive** login is still required
  before the first and fourth acceptance boxes can be checked.
- Mechanism note (corrected 2026-05-29): the inline flow now submits the Guard
  code into the **live** steamcmd session via `tmux send-keys` (not a positional
  `--code` re-login). For **email** Steam Guard the code is mailed *only after*
  the login attempt parks at the prompt, so it cannot be pre-supplied; the
  operator runs the login, waits for the email, and enters the code at the
  prompt. `--code`/`STEAMPIPE_GUARD_CODE` is only usable with the on-demand mobile
  authenticator.
- Regenerate the missing host evidence interactively (email-Guard default):

  ```sh
  export XDG_RUNTIME_DIR=/run/user/$(id -u)
  # Triggers the login attempt (Steam emails the code), parks at the prompt,
  # then completes once you enter the emailed code at the dialog/stdin prompt:
  cluster-ctl --vm-count 8 steam login vm-1
  # (Mobile-authenticator accounts only: STEAMPIPE_GUARD_CODE=<current TOTP> may
  # be exported beforehand to skip the prompt.)
  ```

- Validate the regenerated evidence without printing token contents:

  ```sh
  export XDG_RUNTIME_DIR=/run/user/$(id -u)
  find /var/lib/steampipe/logins/vm-1/config -maxdepth 3 -type f \
    \( -name loginusers.vdf -o -name config.vdf -o -name local.vdf \) \
    -printf '%p %s bytes\n' | sort
  grep -RIl 'eyJ' /var/lib/steampipe/logins/vm-1/config
  cluster-ctl --vm-count 8 steam accounts
  cluster-ctl --vm-count 8 up
  ```

## Files likely touched

- `src/game/steam.rs` — `steamcmd_state_sync_script` (+ its unit test). *(Disjoint
  from the control-flow functions Phase 01/02/04 edit; still rebase on Phase 01.)*
- `src/game/accounts.rs` — `find_refresh_token_file` hardening + detector tests.
- Test fixtures under `tests/` or inline `tempfile` fixtures in `accounts.rs`
  tests.

## Pitfalls

- **No atlas access blocks closure, not the code.** If a real account/host isn't
  available, land the sync-script + detector + tests (steps 2–5) but leave the
  host-verification acceptance boxes unchecked and **flag the phase as blocked on
  host verification** — do not mark the auto-login premise proven from unit tests
  alone. *Recovery:* coordinate a host run before Phase 07 retires fallbacks.
- **Copying the wrong SteamID dir.** The `config/` dir can contain multiple
  per-ID subdirs (old accounts). Copy by the 17-digit-ID glob and let the detector
  pick by the active SteamID from `loginusers.vdf`. *Symptom:* stale token found.
  *Recovery:* `find_refresh_token_file` already keys off the parsed `steam_id`.
- **JWT in both files but stale in one.** If `config.vdf` has an old token and
  `local.vdf` the fresh one, copying both is fine because the detector prefers
  `local.vdf`. Keep that precedence.
- **virtiofs share timing.** The token must be flushed to the host share before
  teardown; the inline flow (Phase 01) keeps the VM up through `finish_steam_login`
  — confirm the sync runs before `graceful_stop_steam`.

## Reference

- Dossier: "Blockers And Missing Artifacts" (state-sync mismatch); critic gap
  "state-sync vs detection mismatch"; "Work That Should Survive" (host-side
  persistence via virtiofs share).
- Detector: `src/game/accounts.rs:241-352`; sync: `src/game/steam.rs:1628-1666`;
  existing sync test: `steam.rs:2358`.
- Gates Phase 07 (retirement) — convergence must be proven first.
