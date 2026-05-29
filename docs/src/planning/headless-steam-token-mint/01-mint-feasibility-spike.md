# Phase 01 — Feasibility spike: headless SteamClient token mint

> **Recommended Codex model: GPT 5.5 high**
>
> This is a de-risking spike against an external, partly-undocumented protocol
> (Valve's `IAuthentication` flow) with a hard success criterion — minting a
> token whose audience includes `client`. It gates every other phase: the library
> choice, host-vs-VM decision, and token portability all flow from here, and a
> wrong "it'll work" call cascades into wasted engine/format work. High
> uncertainty + gating role → `high`; it is a bounded PoC against a known API
> surface, not open-ended research, so not `max`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. No prerequisite phases. Produce a
**throwaway** PoC (a scratch example or a `crates/spike-steam-mint` you delete
before finishing); the only durable deliverable is this file's `## Findings`.

## Goal

Prove (or disprove) that a SteamClient-platform refresh token can be minted
**headlessly** from username+password+Steam-Guard-code, and decide the mechanism
the rest of the plan builds on. End state: a working PoC that, given a real test
account and a Guard code, prints the SteamID64, the refresh token (redacted), and
the **decoded `aud` claim showing `client`**, plus a written verdict choosing
(a) the library/mechanism and (b) host-side vs in-VM execution.

## Why this matters now

The whole plan rests on one unproven assumption: that we can obtain a
*client-audience* refresh token without the Steam GUI. steamcmd yields only
web-audience tokens; the GUI is broken headlessly. If no Rust crate can do
`EAuthTokenPlatformType_SteamClient` auth, we must know now (and pivot to a
packaged helper) before building the engine and artifact writer on a false
premise.

## Out of scope

- Production wiring, the `GuardProvider` integration, artifact writing — later
  phases. The spike may hardcode the Guard code via stdin/env to prove the flow.
- Any durable change to `cluster-ctl` sources.

## Plan

1. **Survey Rust options.** Evaluate `steam-vent` (icewind1991) first: does it
   expose `BeginAuthSessionViaCredentials` with platform `SteamClient`, RSA
   password encryption (`GetPasswordRSAPublicKey`), `UpdateAuthSessionWithSteamGuardCode`,
   and `PollAuthSessionStatus` returning a refresh token? Check its docs/source and
   recent releases. Note alternatives (`rust-steam`, raw protobufs via
   `steam-vent`'s codegen, or `tappet`/web-API) and whether any yields a
   *client*-audience token (web-API `IAuthenticationService` typically yields
   web-audience).
2. **Confirm the audience requirement.** Decode a sample token's middle segment
   (base64url JSON) and confirm `aud` for SteamClient login includes `client`
   (and `web`); web-only tokens have `aud:["web"]`. The VM's Steam GUI auto-login
   needs the client audience.
3. **Build the PoC.** In a scratch crate/example, implement: fetch RSA key →
   encrypt password → `BeginAuthSessionViaCredentials(platform=SteamClient,
   persistence=Persistent)` → (if Guard required) read a code from stdin/env →
   `UpdateAuthSessionWithSteamGuardCode` → poll `PollAuthSessionStatus` until a
   refresh token is returned. Use a real test account (operator-supplied creds +
   one emailed Guard code). Print SteamID64, persona/account name if available,
   `redact(refresh_token)`, and the decoded `aud`+`exp`.
4. **Email-Guard timing check.** Confirm the email code is issued by the
   `BeginAuthSession` call (so the production flow can relay the prompt to the
   iced dialog and then submit) — record the exact `confirmation_type`/allowed
   confirmations returned.
5. **Token portability check.** Establish whether the minted token is bound to the
   minting host/IP or is account-scoped and usable by the VM's client. At minimum,
   write the token into a temp `loginStateDir`-shaped dir and confirm the repo's
   `classify_session_status` (src/game/accounts.rs) accepts it; ideally note
   whether a real Steam client elsewhere can consume it.
6. **Decide host-vs-VM.** Given the PoC runs fine on the host and writes to the
   virtiofs share, recommend host-side mint (no VM interaction) unless portability
   evidence says otherwise.
7. **Write the verdict** in `## Findings`: chosen library + version, the exact API
   call sequence (with signatures), persona/account-name source, the
   host-vs-VM decision, token portability evidence, and any blockers. If no Rust
   path works, specify the packaged-helper fallback (DoctorMcKay `steam-session`
   Node or ValvePython) precisely enough for Phase 02 to adopt.

## Acceptance criteria

- [ ] `## Findings` records a chosen mechanism with evidence it can mint a
      **client-audience** refresh token headlessly (decoded `aud` includes
      `client`), or a definitive "no Rust path; use named packaged helper".
- [ ] A PoC actually authenticated a real test account end-to-end (credentials +
      one Guard code) and produced SteamID64 + a refresh token + decoded `aud`/`exp`
      (token value redacted in the writeup).
- [ ] The minted token, placed in a `loginStateDir`-shaped temp dir with a
      matching `loginusers.vdf`, is accepted by `classify_session_status` as `Ok`
      (run a small harness or the real `cluster-ctl steam accounts` against a temp
      dir) — proving the detector contract is satisfiable by the mint.
- [ ] The host-vs-VM decision and the email-Guard issuance timing are recorded
      with evidence.
- [ ] The exact API/call sequence + chosen crate version are documented so Phase
      02 can implement without re-researching.
- [ ] The scratch PoC crate/example is removed; `git status` shows only this file
      changed.

## Files likely touched

- None durably. Throwaway: `crates/spike-steam-mint/` or an `examples/` PoC
  (deleted before finishing).
- This phase file's `## Findings`.

## Pitfalls

- **Web vs client audience is the whole ballgame.** A token that authenticates
  but has `aud:["web"]` is useless to the VM client — always decode `aud`.
  *Recovery:* ensure `platform = SteamClient` (not WebBrowser/MobileApp).
- **RSA timestamp/key rotation.** `GetPasswordRSAPublicKey` returns a key +
  timestamp that must be passed to `BeginAuthSession`; stale timestamps fail.
  *Recovery:* fetch the key immediately before login.
- **Guard type confusion.** Email code (`k_EAuthSessionGuardType_EmailCode`) vs
  device/TOTP (`DeviceCode`) need different `UpdateAuthSession` calls. The default
  here is email (issued on BeginAuth). *Recovery:* branch on the returned allowed
  confirmations.
- **Rate limits / account lockout.** Repeated failed PoC attempts can rate-limit
  the test account. *Recovery:* minimize attempts; reuse one Guard code window.
- **Crate immaturity.** `steam-vent` may require connecting to a CM server and
  speaking the binary protocol rather than a simple web call; budget for that. If
  it can't do credential+Guard auth, say so and pivot — don't force it.

## Reference
- Plan README (contract, decisions, scope): [README.md](./README.md).
- Detector the token must satisfy: `src/game/accounts.rs` (`find_refresh_token_file`,
  `extract_refresh_jwt`, `jwt_exp_unix`, `parse_loginusers_file`).
- Valve `IAuthentication` flow (BeginAuthSessionViaCredentials /
  UpdateAuthSessionWithSteamGuardCode / PollAuthSessionStatus); `steam-vent`,
  DoctorMcKay `steam-session`, ValvePython `steam` as candidate implementations.
- Gates Phases 02 (engine), 03 (artifacts), 04 (wiring).

## Findings

### Status

Blocked before acceptance after the 2026-05-29 rerun: no operator-supplied Steam
test account credentials or live Steam Guard code were available in the
environment. I did not fabricate a token or substitute a sample token for the
required live auth evidence.

Required missing input:

- `STEAM_ACCOUNT`: username for a real throwaway Steam test account.
- `STEAM_PASSWORD`: password for that account.
- One live Steam Guard code generated by the login attempt.

Upstream producer to fix: the operator running the spike must supply the
throwaway account credentials and relay the Guard code when prompted. The
validation workflow is the disposable PoC command below; acceptance requires its
real output, with the refresh token redacted, to show `aud` containing `client`.

### Survey

Chosen Rust candidate, pending live validation: `steam-vent = "0.5"`.
`steam-vent 0.5.0` is the latest release
(`cargo info steam-vent@0.5.0`, repository `https://codeberg.org/steam-vent/steam-vent`,
docs `https://docs.rs/steam-vent/0.5.0/steam_vent/`) and requires Rust
`1.88.0`. The repo MSRV has been raised to `rust-version = "1.88"` so Phase 02
can use the current `steam-vent` release instead of pinning `0.4.2`.
The local toolchain used for the spike was `rustc 1.97.0-nightly`.

Evidence from the crate source:

- Public entry point:
  `Connection::login(&ServerList, account, password, guard_data_store, confirmation_handler)`
  (`docs.rs`: `https://docs.rs/steam-vent/0.5.0/steam_vent/connection/struct.Connection.html`).
- Guard handling:
  `UserProvidedAuthConfirmationHandler::new` / `ConsoleAuthConfirmationHandler`
  prompts for a newline-terminated code and implements `AuthConfirmationHandler`
  (`docs.rs`: `https://docs.rs/steam-vent/0.5.0/steam_vent/auth/struct.UserProvidedAuthConfirmationHandler.html`).
- Machine-token persistence seam:
  `GuardDataStore::{load,store}` and `NullGuardDataStore` exist
  (`docs.rs`: `https://docs.rs/steam-vent/0.5.0/steam_vent/auth/trait.GuardDataStore.html`).
- Guard-provider integration caveat:
  `AuthConfirmationHandler` is public, but `SteamGuardToken` has no public
  constructor in `steam-vent 0.5.0`. A Phase 02 provider should use
  `UserProvidedAuthConfirmationHandler<Read, Write>` with custom async I/O to
  feed the operator-supplied code, or upstream/request a public constructor,
  rather than trying to construct `ConfirmationAction::GuardToken` directly.
- Internal credential sequence in
  `~/.cargo/registry/src/index.crates.io-*/steam-vent-0.5.0/src/auth/mod.rs`:
  `get_password_rsa` sends
  `CAuthentication_GetPasswordRSAPublicKey_Request`, encrypts with
  `steam_vent_crypto::encrypt_with_key_pkcs1`, then sends
  `CAuthentication_BeginAuthSessionViaCredentials_Request` with
  `ESessionPersistence::k_ESessionPersistence_Persistent`,
  `website_id = "Client"`, and
  `CAuthentication_DeviceDetails.platform_type =
  EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient`.
- Internal Guard/poll sequence:
  `StartedAuth::allowed_confirmations()` exposes returned confirmation choices;
  `StartedAuth::submit_confirmation()` sends
  `CAuthentication_UpdateAuthSessionWithSteamGuardCode_Request` with the chosen
  `EAuthSessionGuardType`; `PendingAuth::wait_for_tokens()` polls
  `CAuthentication_PollAuthSessionStatus_Request` until `access_token` /
  `refresh_token` is present.
- Internal CM-login validation:
  after auth, `UnAuthenticatedConnection::login` logs into the CM using
  `tokens.refresh_token.as_ref()` as the `CMsgClientLogon.access_token`, with the
  source comment: "yes we send the refresh token as access token ... this is
  actually required." Therefore `Connection::access_token()` on the returned
  connection is the refresh token needed by the artifact writer, despite the
  method name.

Audience requirement source: DoctorMcKay `steam-session` documents platform
token audiences as `SteamClient -> ['web', 'client']`, `WebBrowser -> ['web']`,
and `MobileApp -> ['web', 'mobile']`
(`https://github.com/DoctorMcKay/node-steam-session`, README
`EAuthTokenPlatformType`). This matches the plan's requirement that the VM
client needs `client` audience and web-only tokens are insufficient.

Alternatives:

- `tappet 0.6.0` is a typed wrapper around official Steam Web API methods. It
  does not target the CM/WebSocket `SteamClient` login path, so it is not the
  right mechanism for client-audience refresh tokens.
- `rust-steam` on crates.io is a thermodynamic steam-tables crate, not Valve
  Steam authentication.
- DoctorMcKay `steam-session` remains the precise fallback if the Rust live
  attempt fails. Use Node package `steam-session`, instantiate
  `new LoginSession(EAuthTokenPlatformType.SteamClient)`, call
  `startWithCredentials({accountName, password})`, submit the Guard code with
  `submitSteamGuardCode(code)`, then read `session.steamID` and
  `session.refreshToken`.
- ValvePython `steam` is a secondary fallback only if the Node helper is
  rejected; no Rust-path blocker has been proven yet.

### Disposable PoC

A scratch crate was created at `/tmp/steampipe-spike-steam-mint`, compiled with
`steam-vent = "0.5.0"`, and then removed. It used only the public `steam-vent`
API:

```rust
let server_list = ServerList::discover().await?;
let connection = Connection::login(
    &server_list,
    &account,
    &password,
    NullGuardDataStore,
    ConsoleAuthConfirmationHandler::default(),
).await?;
let refresh_token = connection.access_token().unwrap();
```

The PoC decoded the JWT middle segment with base64url-no-pad, printed:

- `steam_id64 = u64::from(connection.steam_id())`
- `account = $STEAM_ACCOUNT`
- `refresh_token = <redacted>`
- decoded `aud`
- decoded `exp`

Compile validation completed:

```sh
cargo check --manifest-path /tmp/steampipe-spike-steam-mint/Cargo.toml
# Finished successfully
```

Fail-fast validation for missing foundational input completed on 2026-05-29:

```sh
cargo run --manifest-path /tmp/steampipe-spike-steam-mint/Cargo.toml --quiet
# Error: STEAM_ACCOUNT is required
```

Regenerate the disposable PoC with a temp binary crate that depends on
`anyhow = "1"`, `base64 = "0.22"`, `serde_json = "1"`,
`steam-vent = "0.5"`, and `tokio = { version = "1", features = ["macros",
"rt-multi-thread"] }`, then use the code shape above. Live validation command:

```sh
STEAM_ACCOUNT='<throwaway-account>' \
STEAM_PASSWORD='<throwaway-password>' \
cargo run --manifest-path /tmp/steampipe-spike-steam-mint/Cargo.toml
```

When prompted by `ConsoleAuthConfirmationHandler`, enter the Steam Guard code
that was issued by that `BeginAuthSessionViaCredentials` attempt. The command is
accepted only if the decoded `aud` output includes `client`.

### Detector Contract

Existing detector tests passed:

```sh
cargo test classify_session_status --lib
# 3 passed; 0 failed
```

This verifies that `classify_session_status` accepts a
`loginStateDir/<vm>/config/loginusers.vdf` plus either
`config/<steamid>/local.vdf` or `config/config.vdf` containing a JWT-shaped
refresh token whose `exp` is beyond the warning window. It does **not** satisfy
the phase acceptance criterion for a minted token, because no real refresh token
was minted in this environment.

### Provisional Decision

Do not start Phase 02 as accepted yet. The provisional mechanism is
host-side Rust minting with `steam-vent`, because the crate implements the exact
SteamClient credential auth sequence and can run on the host without VM GUI
interaction. The durable decision still requires the missing live account run to
record:

- real SteamID64
- redacted real refresh token
- decoded `aud` including `client`
- decoded `exp`
- exact returned Guard confirmation type/message from the prompt
- `classify_session_status(...)=Ok` against a temp `loginStateDir` built from
  that real token

If the live `steam-vent` run fails despite valid credentials and Guard code,
pivot Phase 02 to the Node helper fallback:
DoctorMcKay `steam-session` with `EAuthTokenPlatformType.SteamClient`.
