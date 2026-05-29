# Plan: Headless Steam token-mint login

> **Recommended Codex model for plan-set orchestration: GPT 5.5 high**
>
> This set implements Steam client authentication from scratch against Valve's
> protocol (RSA-encrypted credential login, Steam Guard, token polling) and must
> produce on-disk artifacts that satisfy two independent consumers (the repo's
> session detector AND the real Steam GUI's auto-login). The hard parts are
> external-protocol uncertainty and format-correctness, not breadth — the
> orchestrator must keep the feasibility spike gating the rest and not let the
> engine/format phases proceed on assumptions. Complex frontier integration at an
> orchestrator role → `high` (not `max`: it is bounded by a clear contract).

## Scope and current state

`cluster-ctl steam login vm-N` must establish a **reusable Steam session with no
in-VM Steam GUI and no X server**, by minting a **SteamClient-platform
(web+client audience) refresh token** directly via Valve's auth protocol and
writing the artifacts the host detector + the VM's Steam GUI auto-login need, into
the VM's virtiofs `loginStateDir` share (`/var/lib/steampipe/logins/vm-N/` ==
the VM's `~/.local/share/Steam`).

This replaces the current login engine (in-VM steamcmd bootstrap + in-VM Steam GUI
validation), whose GUI step is irreparably broken headlessly.

### What already works (reuse, don't rebuild)
- The host-side **iced Steam Guard dialog + `GuardProvider`** (`src/game/steam.rs`)
  — interactive dialog, `--code`/`STEAMPIPE_GUARD_CODE` for unattended, typed
  `GuardCodeNeeded` fail-fast when no code channel (decision #2). This is the
  Guard-code source for the mint flow.
- Credentials loading (`CredentialsMap`/`VmCredentials` steam_user/steam_pass),
  the virtiofs `loginStateDir` share, and the `accounts` session detector.

### What is being abandoned (and why)
- **In-VM Steam GUI bootstrap for login.** Xwayland was removed (commits
  `22581fc`/`e5fdf45`, 2026-05-07); Steam's first-run X11 "xwin" updater UI
  segfaults (`XOpenDisplay failed`). steamcmd authenticates but writes **no**
  `loginusers.vdf` and only a **web-audience** token the desktop client can't use;
  the `steam -login user pass` auto-login flag is deprecated/unreliable. So login
  mints a client-audience token directly instead of via steamcmd or the GUI.

### Detector contract (verified, `src/game/accounts.rs`)
`classify_session_status` reports `Ok` when, under `loginStateDir/vm-N/`:
- `config/loginusers.vdf` contains a 17-digit SteamID (regex `^\s*"(\d{17})"`) and
  `"PersonaName" "..."`; **and**
- an `eyJ`-prefixed JWT (regex `"(eyJ[A-Za-z0-9_\-.]+)"`) with an `exp` payload
  claim exists in `config/<steamID>/local.vdf` (preferred) or `config/config.vdf`.

### Out of scope
- **Game-run Steam GUI startup** (the same in-VM X/`xwin` crash affects game
  runs). This plan establishes the *session*; making the VM's Steam GUI actually
  start for gameplay is a separate effort. (The mint writes artifacts the GUI
  *would* auto-login from once that is fixed.)
- The iced dialog itself (done) and the broader login-overhaul plan.
- Re-adding Xwayland.

## Decisions to lock in Phase 01 (the spike)
- **Mechanism/library:** a Rust crate (e.g. `steam-vent`) doing
  `BeginAuthSessionViaCredentials(EAuthTokenPlatformType_SteamClient)` + RSA
  password encryption + `UpdateAuthSessionWithSteamGuardCode` +
  `PollAuthSessionStatus`, **vs** a packaged helper (DoctorMcKay `steam-session`
  Node / ValvePython). Prefer Rust-native; fall back to a packaged helper only if
  no Rust path mints a *client-audience* token.
- **Where it runs:** host-side in `cluster-ctl` (writes to the share) **vs**
  in-VM. Prefer host-side (no VM interaction for login at all).
- **Token portability:** confirm a token minted host-side is usable by the VM's
  client (not machine/IP-bound in a way that breaks).

## Phase table

| Phase | File | Depends on | Touches (primary) | Can parallel with | Blocking? |
|---|---|---|---|---|---|
| 01 — Feasibility spike: headless SteamClient token mint | [01-mint-feasibility-spike.md](./01-mint-feasibility-spike.md) | — | throwaway PoC crate/example | — | **Yes** (gates all) |
| 02 — Mint engine module + GuardProvider integration | [02-mint-engine.md](./02-mint-engine.md) | 01 | new `src/game/steam_auth.rs` (or crate), `src/game/steam.rs` (provider reuse) | 03 | Yes (gates 04) |
| 03 — Session artifact writer (VDF) → loginStateDir share | [03-artifact-writer.md](./03-artifact-writer.md) | 01 | new `src/game/steam_session.rs`, `src/game/accounts.rs` (tests) | 02 | Yes (gates 04) |
| 04 — Wire mint into `steam login` (replace steamcmd+GUI engine) | [04-wire-login-engine.md](./04-wire-login-engine.md) | 01,02,03 | `src/game/steam.rs`, `src/lib.rs` | — | No |
| 05 — Retire in-VM login internals + CLI/docs/tests + live verify | [05-retire-and-verify.md](./05-retire-and-verify.md) | 04 | `src/game/steam.rs`, `src/ui/cli.rs`, `docs/` | — | No |

> **Serialization hazard — `src/game/steam.rs`.** Phases 02, 04, 05 edit it;
> rebase + re-read before editing. 02 mainly *adds* a module + reuses the
> `GuardProvider`; the heavy edits are 04 (engine swap) then 05 (retirement).

## Parallelism layer (execution waves)

- **Wave 0:** **Phase 01** alone — the feasibility spike. It decides the library,
  host-vs-VM, and proves a client-audience token can be minted headlessly with a
  Guard code. **If 01 finds no viable Rust mint and a packaged helper is needed,
  that reshapes 02/04 — do not start them until 01's verdict is in.**
- **Wave 1:** **Phase 02** (mint engine + provider wiring) and **Phase 03**
  (artifact writer) in parallel — disjoint new files (auth protocol vs VDF
  writing), both consuming 01's decisions (chosen lib + exact token shape).
- **Wave 2:** **Phase 04** — wire the engine into `steam login` (mint → write →
  done), replacing the steamcmd+GUI path. Needs 02+03.
- **Wave 3:** **Phase 05** — retire the now-dead in-VM login internals, update
  CLI/docs/tests, and perform the **operator live verification** (one real Guard
  code on the host).
- **Plan exhausted** after Wave 3.

## Whole-set acceptance criteria

- [ ] `cluster-ctl steam login vm-1` on a session-less VM, on a host with the iced
      dialog (or `--code`), mints a client-audience refresh token and writes
      `config/loginusers.vdf` (17-digit SteamID + PersonaName) and
      `config/<steamID>/local.vdf` (eyJ JWT with `exp`) into
      `/var/lib/steampipe/logins/vm-1/`, in **one** invocation, **no in-VM Steam
      GUI started and no X server involved**.
- [ ] `cluster-ctl steam accounts` then reports `vm-1 … Ok` (the detector accepts
      the minted artifacts), and a subsequent `up` skips re-login (fill-the-gaps).
- [ ] The minted JWT's `aud` claim includes `client` (SteamClient platform), and
      it is decodable with an `exp` in the future — verified by decoding the token.
- [ ] Headless/`--non-interactive`/MCP path returns the typed `GuardCodeNeeded`
      fail-fast (no dialog, no hang); `--code`/`STEAMPIPE_GUARD_CODE` works
      unattended (mobile authenticator).
- [ ] No Steam Guard re-prompt loop: a second `steam login` while the token is
      valid skips (fill-the-gaps), and re-mint after `--force` works.
- [ ] `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
      green; the steamcmd-bootstrap + in-VM-GUI-validation login internals are
      removed or gated off the default path; docs describe the mint flow.

## Global constraints
- **Secrets:** steam_pass, the Guard code, and the minted refresh token are
  secrets — route through `redact_sensitive`, never log, zeroize buffers. The
  refresh token written to the share is sensitive; the share is already
  operator-private (`drwx------`).
- **AI/headless:** never open a window or block on stdin in a non-interactive
  context; reuse the `GuardProvider` selection + typed `GuardCodeNeeded`.
- **Reuse, don't fork:** the `GuardProvider` (dialog/stdin/fail-fast), credentials
  plumbing, `par_each_vm`, and the detector are correct — build on them.
- **Email-Guard timing:** the one-time code is issued by the `BeginAuthSession`
  attempt; relay that prompt to the provider, then submit — same attempt-then-relay
  shape the steamcmd path already uses.

## Reference
- Login overhaul dossier/plan: [steam-login-iced-overhaul-research.md](../steam-login-iced-overhaul-research.md),
  [steam-login-iced-overhaul/README.md](../steam-login-iced-overhaul/README.md).
- Why the GUI path was abandoned: the stable
  [Steam Guard dialog helper](../../configuration/steam-credentials.md#steam-guard-dialog-helper)
  docs record the resolved host-dialog runtime failure mode; the token-mint plan
  avoids the in-VM Steam GUI entirely.
- Detector: `src/game/accounts.rs`; Guard provider + login engine:
  `src/game/steam.rs`.
- Run each phase in a fresh `codex exec` session; prompt `verify` when done.
