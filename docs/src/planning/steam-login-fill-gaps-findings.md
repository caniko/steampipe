# Steam Login Fill-the-Gaps Findings

## Goal And Trigger

User wants `cluster-ctl steam login` (with target `all` or omitted) to skip
VMs that already have a valid Steam session by default, only logging in the
ones that don't. An opt-in flag re-logs everything regardless of state.
Today the command boots and logs in every target unconditionally, which is
wasteful and risky for VMs whose session is already healthy.

## Root Cause / Current Behavior

`steam::login` at [src/game/steam.rs:550-571](src/game/steam.rs#L550-L571)
iterates over every resolved target and calls `login_single_vm` without
any precheck. Targets come from `config.resolve_targets(target,
continue_from)` and include all VMs when `target` is `None` / `"all"`.
There is no concept of "already logged in" in this code path.

The status the user wants to gate on is already computed elsewhere:
`accounts::read_account_info`
([src/game/accounts.rs:207-264](src/game/accounts.rs#L207-L264)) inspects
`loginStateDir/<vm>/config/loginusers.vdf` (gives `logged_in =
steam_id.is_some()`) and the per-SteamID refresh JWT under
`config/<steamID>/local.vdf` or `config/config.vdf`, returning one of
`OK | STALE | EXPIRED | NO_TOKEN`. This is host-side, read-only, fast — no
VM boot required.

A second, in-VM probe exists for `steam check`
([src/game/steam.rs:519-545](src/game/steam.rs#L519-L545),
`steam_identity_probe_script`) but it requires a running VM, so it's the
wrong tool for a precheck.

## Evidence

- `rg -n "steam.*login|SteamLogin" src/`
- `src/ui/cli.rs:614-631` — `SteamAction::Login` enum (target,
  continue_from, login_runners_dir).
- `src/lib.rs:905-931` — dispatch into `steam::login`.
- `src/game/steam.rs:548-571` — `login` fan-out loop, no precheck.
- `src/game/steam.rs:716-783` — `login_single_vm` boots the VM
  unconditionally.
- `src/game/accounts.rs:207-264` — host-side session classification
  (`OK/STALE/EXPIRED/NO_TOKEN`) from `loginStateDir`.
- `src/game/accounts.rs:121` — existing user-facing hint
  ("Run `cluster-ctl steam login` to log in missing VMs") which this
  change makes the default behavior of `login` itself.
- `src/core/nixos_module.rs:64-65` — `login_state_dir` carried on
  `NixosModuleConfig`, needed as input to the precheck.
- `docs/src/configuration/steam-credentials.md` — user-facing docs to
  update (interactive + automated login sections).
- `src/ui/cli.rs:903-932` — existing parse test for the login subcommand
  to extend with `--force` coverage.

## Recommended Fix

A focused, localized change:

1. **CLI.** Add `#[arg(long)] force: bool` to `SteamAction::Login` in
   [src/ui/cli.rs:618-631](src/ui/cli.rs#L618-L631). Doc the flag:
   "Re-login every selected VM even if it already has a valid Steam
   session."

2. **Dispatch.** Thread `force` and the module's `login_state_dir`
   through [src/lib.rs:905-931](src/lib.rs#L905-L931) into
   `steam::login`. The module config is already in scope as `module`.

3. **Precheck in `steam::login`.** Before the per-VM loop in
   [src/game/steam.rs:550-571](src/game/steam.rs#L550-L571):
   - Extract the session-status code from `accounts::read_account_info`
     into a pub(crate) helper so `steam::login` can call it without
     pulling in the table-printing path. Re-use the existing
     `parse_loginusers_file` / `find_refresh_token_file` logic — do not
     duplicate.
   - For each resolved target, classify as `OK | STALE | EXPIRED |
     NO_TOKEN | UNKNOWN` using the host's `loginStateDir`.
   - When `force` is false and the status is `OK`, skip the VM and print
     a one-liner (e.g. `  vm-3: already logged in (persona), skipping
     (use --force to re-login)`).
   - When status is `STALE` / `EXPIRED` / `NO_TOKEN`, log in. This is the
     "fill the gaps" intent: anything that isn't a clean `OK` gets a
     fresh login. Print the reason so the user sees why each VM was
     picked.
   - When `loginStateDir` is unavailable (non-NixOS host, no
     `--login-state-dir` override), fall back to the current behavior
     (log in everything) and print a single notice that fill-the-gaps
     is disabled because session state can't be read. Don't fail.
   - Summary line at the end: `  Logged in: X, Skipped: Y, Failed: Z`.

4. **Tests.**
   - Extend the existing parse test
     ([src/ui/cli.rs:903-932](src/ui/cli.rs#L903-L932)) with a
     `--force` variant and a "default has force=false" variant.
   - Add a unit test for the status-classification helper covering each
     of `OK / STALE / EXPIRED / NO_TOKEN / UNKNOWN` against a temp
     `loginStateDir` fixture (the helpers in `accounts.rs` already
     accept paths, so this is straightforward).

5. **Docs.** Update
   [docs/src/configuration/steam-credentials.md:23-73](docs/src/configuration/steam-credentials.md#L23-L73)
   to state the new default and document `--force`. One paragraph each
   in the "Interactive login" and "Automated login" sections is enough.

## Optional Follow-Ups

- Treat `STALE` as "skip by default, warmable" rather than "re-login":
  warming is cheaper than a full login and already exists as `steam
  warm`. If you take this path, fill-the-gaps becomes "login NO_TOKEN /
  EXPIRED only; STALE → suggest `steam warm`". Skip unless the user
  asks — adds policy without a strong reason today.
- Add a `--only-status <list>` flag to override the policy
  (`--only-status no_token,expired`). Skip; not requested.

## Open Decisions

- **Does `STALE` get a fresh login by default, or is `STALE` treated as
  good-enough until `EXPIRED`?** The simpler "anything not `OK` →
  login" rule is recommended above; the alternative is to only re-login
  `NO_TOKEN | EXPIRED` and leave `STALE` to `steam warm`. This is the
  one policy call worth confirming before implementing. Default
  recommendation: re-login `STALE` too, because the user's framing was
  "fill the gaps" and a stale token is a half-filled gap; `--force`
  remains the explicit "re-login OK too" escape hatch.
- **Explicit single target (`steam login vm-3`) — does the
  skip-if-OK rule still apply, or does naming a VM imply intent to
  log in?** Recommendation: skip-if-OK still applies, because the
  parallel command `steam warm vm-3` already short-circuits gracefully
  when not needed; users who explicitly want to overwrite pass
  `--force`. Worth confirming.
