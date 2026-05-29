# Phase 02 — Mint engine module + GuardProvider integration

> **Recommended Codex model: GPT 5.5 high**
>
> Turning the spike into a robust, secret-safe, async mint engine that integrates
> the existing `GuardProvider` (email-Guard relay, retry, cancel/fail-fast) is
> complex protocol-integration work where edge cases (Guard type branching, RSA
> rotation, poll timeouts, zeroizing secrets) bite. It is a sub-agent-role module
> against a now-known API, but correctness and secret-handling consequences put it
> at `high`, not `medium`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase 01** — read its
`## Findings` for the chosen library/mechanism, exact call sequence, and
host-vs-VM decision. Adds a new module; reuses the `GuardProvider` in
`src/game/steam.rs`. Rebase on current `steam.rs`.

## Goal

A reusable, headless mint engine: given `VmCredentials` (steam_user/steam_pass)
and a `&GuardProvider`, it authenticates SteamClient-platform and returns a
`MintedSession { steam_id64, account_name, persona_name, refresh_token, expires_at }`
— obtaining the Steam Guard code through the provider (dialog/stdin/`--code`),
relaying the email-issued prompt, retrying on invalid code, and failing fast with
the typed `GuardCodeNeeded` outcome when no code channel exists. All secrets
routed through `redact_sensitive` and zeroized.

## Why this matters now

Phase 01 proved a token can be minted; this makes it production-grade and wires it
to the *already-working* Guard-code source so the engine is headless/AI-compatible
from day one. It is the reusable core Phase 04 calls per VM.

## Out of scope

- Writing the on-disk VDF artifacts — Phase 03 (this phase returns the in-memory
  `MintedSession`).
- Replacing `automated_login` / wiring into `steam login` — Phase 04.
- The dialog UI itself (done).

## Plan

1. **Add the dependency/mechanism** chosen in Phase 01 (a Rust crate in
   `Cargo.toml`, or a packaged helper invoked via `tokio::process::Command`).
   Keep it confined to the new module so the rest of `cluster-ctl` is unaffected.
2. **Create `src/game/steam_auth.rs`** (module name per repo convention) exposing:
   ```rust
   pub(crate) struct MintedSession { pub steam_id64: u64, pub account_name: String,
       pub persona_name: Option<String>, pub refresh_token: String, pub expires_at: i64 }
   pub(crate) async fn mint_session(creds: &VmCredentials, cx_vm: &str,
       login_reason: &str, provider: &GuardProvider) -> anyhow::Result<MintOutcome>;
   pub(crate) enum MintOutcome { Minted(MintedSession), GuardCodeNeeded(GuardCodeNeeded), }
   ```
3. **Implement the flow** (per Phase 01's sequence): fetch RSA key → encrypt
   password → `BeginAuthSessionViaCredentials(platform=SteamClient)` → if a Guard
   code is required, build a `GuardPromptContext{vm,steam_user,reason,invalid_retry}`
   and call `provider.request(&cx)`:
   - `Code(code)` → submit (`UpdateAuthSessionWithSteamGuardCode`) → poll
     `PollAuthSessionStatus`; on an invalid-code signal, loop with
     `invalid_retry=true`.
   - `Cancelled`/`Unavailable` → return `MintOutcome::GuardCodeNeeded(..)`.
   - No Guard required (cached/email-trusted) → poll directly.
4. **Bound the poll** with a sane timeout/interval; map terminal Steam errors
   (bad password, rate-limited, expired session) to clear `anyhow` errors via
   `redact_sensitive`.
5. **Extract SteamID64 + account + persona** from the auth result (Phase 01 noted
   where persona comes from; if the auth response lacks persona, fetch it via a
   follow-up call or leave `None` — the detector only *requires* SteamID +
   PersonaName in loginusers.vdf, so Phase 03 must source persona; coordinate).
6. **Secret hygiene:** zeroize the password, encrypted blob, Guard code, and keep
   the refresh token only as long as needed; never log any. Add `zeroize` usage
   mirroring the existing Guard path.
7. **Tests:** unit-test the Guard-provider interaction with a stub provider
   (`PreSupplied`/`FailFast`) and a faked auth transport if the chosen lib allows
   injection; at minimum test the outcome mapping (Code→submit, Unavailable→
   GuardCodeNeeded, invalid→retry). A live end-to-end auth is exercised in Phase
   05.

## Acceptance criteria

- [ ] `mint_session` exists and returns `MintOutcome::Minted(MintedSession{..})`
      with a non-empty client-audience `refresh_token`, `steam_id64`,
      `account_name`, and `expires_at` derived from the JWT `exp`.
- [ ] It obtains the Guard code **only** through the passed `&GuardProvider`
      (no direct stdin/dialog), and returns the typed `GuardCodeNeeded` outcome on
      `Cancelled`/`Unavailable` — verified by a unit test with `FailFast` and
      `PreSupplied` providers.
- [ ] Invalid-code retry re-prompts the provider with `invalid_retry=true` (test).
- [ ] Password, Guard code, and token are redacted in any surfaced error and
      zeroized after use; `rg` shows no logging of them.
- [ ] `cargo build`/`clippy`/`test` green; the new dependency is confined to the
      mint module (e.g. `cargo tree` shows it not pulled into unrelated paths).

## Files likely touched
- `Cargo.toml` — the chosen mint crate (or none, if a packaged helper is spawned).
- `src/game/steam_auth.rs` *(new)* — the engine.
- `src/game/mod.rs` — module declaration.
- `src/game/steam.rs` — reuse `GuardProvider`/`GuardPromptContext`/`GuardCodeNeeded`
  (make them visible to the new module if needed). *(Rebase; minimal edit.)*

## Pitfalls
- **Provider must stay `Send + Sync + Clone`** for Phase 04's `par_each_vm`; keep
  `mint_session` not capturing non-Send state across awaits.
- **Email code timing.** Don't call `provider.request` before `BeginAuthSession`
  triggers the email — relay the prompt only after the attempt, matching the
  steamcmd path's order.
- **Persona may be absent** from the auth result; decide its source now (follow-up
  API call vs left to Phase 03) so loginusers.vdf can include `PersonaName`.
- **Packaged-helper path (if chosen in 01):** treat its stdout as untrusted, parse
  defensively, pass the code via the provider (not argv where it could leak), and
  zeroize.

## Reference
- Phase 01 verdict (library + call sequence): [01-mint-feasibility-spike.md](./01-mint-feasibility-spike.md).
- Guard provider/context/outcome to reuse: `src/game/steam.rs`
  (`GuardProvider`, `GuardPromptContext`, `GuardCodeOutcome`, `GuardCodeNeeded`).
- Consumed by Phase 04; pairs with Phase 03 (artifact writer).
