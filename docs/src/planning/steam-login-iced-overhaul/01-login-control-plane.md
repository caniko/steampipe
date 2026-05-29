# Phase 01 — Login control-plane refactor (typed outcome + shared callable + provider seam + inline Guard submit)

> **Status: shipped, then corrected (2026-05-29).** This phase has been executed.
> Its first cut wrongly submitted the Guard code via a fresh *positional*
> `steamcmd +login user pass <code>`; that was corrected to relay the code into the
> **live** session via `tmux send-keys` (see Execution Notes). Verify mode should
> check the corrected mechanism, not the positional one.

> **Recommended Codex model: GPT 5.5 high**
>
> This is the spine every other phase builds on: it reshapes the in-process
> login control flow, introduces the `GuardCodeProvider` seam, and drives the
> existing `tmux send-keys` Guard submit **inline** (in one invocation) instead of
> deferring to a separate `steam guard` process. The work is a non-trivial refactor
> across `steam.rs` and
> `lib.rs` with subtle control-flow and secret-handling implications, and its
> public types are consumed by phases 02, 04, 05, and 06 — getting the type
> shapes wrong forces churn downstream. Complex design at an orchestrator-grade
> role in the plan, so `high`; it is a well-specified refactor (dossier in hand,
> not novel research), so not `max`.

## Working tree

`/data/nvme0/can/Projects/steampipe` (the `steampipe` repo / `cluster-ctl`
binary). This phase runs from the current tree with no prerequisite phases. It
**owns the first edit to `src/game/steam.rs`** in this plan; later phases rebase
on it.

## Goal

The login flow obtains a Guard code through a pluggable `GuardCodeProvider` and
completes the login in a **single invocation**. Concretely: `automated_login`'s
`GuardRequired` branch no longer returns `LoginOutcome::LeftRunning` with
"complete from another shell" guidance; instead it asks a provider for the code
and, when one is supplied, relays it into the **live** steamcmd session via
`tmux send-keys` (the existing `steamcmd_guard_submit_script` +
`wait_for_steamcmd_guard_completion`), then falls through to `finish_steam_login`
and shutdown. A `PreSupplied` provider (from `--code`/env) and a `FailFast`
provider (typed
"guard code needed" error) exist; the interactive GUI/stdin providers are added
in later phases against this seam. Login result becomes a typed value
(`Completed | GuardRequired | Failed`) that the CLI/MCP layers can branch on
deterministically.

## Why this matters now

The entire overhaul pivots on one branch. Today:

```text
src/game/steam.rs:1322-1333
    if wait == SteamCmdWait::GuardRequired {
        log.line("  Steam Guard required ...");
        log.line("  Complete from another shell with: cluster-ctl steam guard <vm> --code <CODE>");
        return Ok(LoginOutcome::LeftRunning);
    }
```

`LoginOutcome::LeftRunning` (`steam.rs:1281-1285`) makes "guard pending"
indistinguishable from success in the exit code, leaves a VM + tmux session
running, and forces a second process.

**Critical mechanism constraint — do NOT use a positional code.** Email Steam
Guard (the always-available default) issues the one-time code *in response to the
login attempt*: Steam mails it only when `steamcmd +login user pass` runs from an
unrecognized machine. So there is no code to pass beforehand — a single-shot
`steamcmd +login user pass <code>` cannot satisfy a first-login email challenge.
The correct flow keeps the no-code bootstrap (which triggers the email and parks
at the `Steam Guard code:` prompt) and relays the obtained code into that **live**
session via the existing `tmux send-keys` mechanism (`steamcmd_guard_submit_script`
+ `wait_for_steamcmd_guard_completion`) — the same mechanism `guard()` already
uses, just driven inline. `--code`/`STEAMPIPE_GUARD_CODE` is a convenience for the
on-demand mobile authenticator / automation, fed through the same send-keys path.
Folding code-acquisition + submit + completion + finish into one invocation
collapses ~5 downstream incompatibilities (dossier incompatibilities #1, #5, #6,
#7, #11), so it must land first.

## Out of scope

- The iced helper binary and the `HelperGui` provider — Phase 03/04.
- `--non-interactive`/`--no-gui` flags and `$DISPLAY`/TTY/MCP detection — Phase
  02. (This phase wires the `FailFast` provider as the default when no
  interactive provider is selected; Phase 02 adds the real selection logic.)
- The state-sync/detection convergence fix — Phase 05.
- Retiring `interactive_login()` / `guard()` / `steam guard` — Phase 07. **Leave
  them compiling and callable** this phase; `guard()` keeps working as the
  fallback path.
- Touching `src/mcp.rs`, `src/harness/runner.rs`, docs, or tests beyond what is
  needed to keep the build green.

## Plan

1. **Introduce a typed login result.** Replace the binary `enum LoginOutcome {
   Completed, LeftRunning }` (`steam.rs:1281-1285`) with a richer typed outcome,
   e.g.:
   ```rust
   enum LoginOutcome {
       Completed,
       /// A Guard code was required but could not be obtained in this context
       /// (no provider could supply one). Carries why, for the CLI/MCP layer.
       GuardCodeNeeded(GuardCodeNeeded),
       // Failed is represented by the function returning Err(_), as today.
   }
   ```
   Keep `Failed` as today's `anyhow::Err`. `GuardCodeNeeded` should carry enough
   for a deterministic caller message (vm name, steam_user, the
   `SessionStatus::login_reason()` that triggered it) **without** secrets.
2. **Define the `GuardCodeProvider` seam.** Add a trait (in `steam.rs` or a new
   `src/game/guard.rs` module — keep it in `game` and `pub(crate)`):
   ```rust
   pub(crate) struct GuardPromptContext<'a> {
       pub vm_name: &'a str,
       pub steam_user: &'a str,
       pub reason: &'a str,   // from SessionStatus::login_reason / "first login"
       pub invalid_retry: bool,
   }
   pub(crate) enum GuardCodeOutcome { Code(String), Cancelled, Unavailable }
   #[async_trait-or-async-fn]
   pub(crate) trait GuardCodeProvider {
       async fn request(&self, cx: &GuardPromptContext<'_>) -> anyhow::Result<GuardCodeOutcome>;
   }
   ```
   Use a plain `async fn request(...)` via an enum dispatcher if you want to avoid
   an `async_trait` dependency — an enum `GuardProvider { PreSupplied(String),
   FailFast, /* Stdin, HelperGui added later */ }` with an inherent `async fn
   request` is acceptable and avoids a new dep. Prefer the enum-dispatch form.
3. **Implement `PreSupplied` and `FailFast`.**
   - `PreSupplied(code)` returns `Code(code)` once, then `Unavailable` on a retry
     (a pre-supplied code can't be re-prompted on invalid-code).
   - `FailFast` returns `Unavailable` always (no interactive channel).
4. **Reuse the existing live-session submit — do NOT add a positional command.**
   The Guard code is submitted into the already-parked steamcmd session via the
   existing `steamcmd_guard_submit_script` (`tmux send-keys`) +
   `wait_for_steamcmd_guard_completion` (`steam.rs`). There is **no**
   `steamcmd +login user pass <code>` variant: email Guard codes don't exist until
   the no-code attempt triggers them, so the attempt must stay alive and receive
   the code. The code is a secret: pass it in the `secrets` slice to
   `redact_sensitive` for the submit + completion-wait output, and zeroize the
   `String` after the wait.
5. **Rewire `automated_login`'s GuardRequired branch** (`steam.rs:1320-1333`).
   Thread a `&GuardProvider` parameter through `automated_login` (and its callers
   `login_single_vm`, `login_single_vm_automated`, `login_vm_attempt`,
   `auto_login`). On `SteamCmdWait::GuardRequired`, loop:
   1. Call `provider.request(&cx)`.
   2. On `Code(code)`: submit it into the **live** session via
      `steamcmd_guard_submit_script` (`tmux send-keys`), then
      `wait_for_steamcmd_guard_completion`. On the post-submit invalid-code marker
      (`steam.rs:1532`) re-call `provider.request(&cx_with_invalid_retry)` — the
      session stays alive at a fresh prompt for the next code (GUI/stdin providers
      re-prompt; `PreSupplied`/`FailFast` return `Unavailable`, ending the loop).
      On completion, `finish_steam_login` + the normal shutdown. **Never** kill the
      session and re-run a fresh login to submit the code.
   3. On `Cancelled` or `Unavailable`: kill the parked session and return
      `Ok(LoginOutcome::GuardCodeNeeded(..))` (do **not** bail with a generic
      error; the caller maps it to the typed exit).
6. **Extract a shared login orchestration callable.** Pull the credential
   loading, `login_runners_dir`/`login_state_dir` resolution, and the
   `steam::login`/`steam::guard` dispatch out of the `Commands::Steam` match arm
   in `src/lib.rs:892-989` into a function returning a typed result the CLI handler
   and (later) other callers consume. The CLI handler maps `Completed` → exit 0,
   `GuardCodeNeeded` → a distinct non-zero exit with an actionable message,
   `Failed` → existing error exit. Keep the public CLI behavior identical for the
   happy path and `--code`.
7. **Default the provider to `PreSupplied` when `--code`/env is present, else
   `FailFast`.** (Phase 02 replaces the "else" with context-based selection of
   Stdin/HelperGui.) Read an env var (e.g. `STEAMPIPE_GUARD_CODE`) as an alternate
   pre-supplied source so unattended/`--code`-less automation has a channel.
8. **Keep `guard()` working.** The standalone `steam guard` path
   (`steam.rs:820-941`) still exists this phase and already uses the live-session
   send-keys submit — share helpers with the inline path where trivial, but do not
   remove it.
9. Run `cargo build`, `cargo clippy`, and the existing `steam`-related unit tests
   in `steam.rs` (update the marker/parse tests that reference the old
   `LoginOutcome` shape).

## Acceptance criteria

- [ ] `enum LoginOutcome` no longer has a `LeftRunning` variant; a
      `GuardCodeNeeded` (or equivalently named) typed outcome exists and carries
      no secrets.
- [ ] `automated_login` takes a `&GuardProvider` and, on `GuardRequired` with a
      `PreSupplied` code, submits via `steamcmd_guard_submit_script` (`tmux
      send-keys`) into the live session and reaches `finish_steam_login` in the
      **same** call — verified by a unit test asserting the no-code bootstrap is
      `steamcmd +login '<user>' '<pass>' +quit` (no positional code), that no
      `steamcmd_bootstrap_with_code_command` / positional `+login user pass <code>`
      exists, and that the GuardRequired branch no longer emits the string
      `Complete from another shell`.
- [ ] `cluster-ctl steam login <vm> --code <CODE>` (authenticator case) exits 0 on
      success with no "left running"/"complete from another shell" output anywhere
      (`rg "Complete from another shell|left running for Steam login" src` returns
      nothing in the automated path).
- [ ] With no `--code`/env and the default `FailFast` provider, a GuardRequired
      login returns the typed `GuardCodeNeeded` outcome and the CLI maps it to a
      **distinct** non-zero exit code (not the generic error exit), with an
      actionable message naming the VM.
- [ ] The Guard code is added to `redact_sensitive` secret slices for the submit +
      completion-wait output and zeroized after use; `rg`-grep shows no
      `println!`/log of the raw code.
- [ ] `cargo build` and `cargo clippy` are clean under the workspace lints in
      `Cargo.toml`; `cargo test game::steam` passes (tests updated for the new
      outcome type).
- [ ] `guard()` and `steam guard --code` still compile and function (fallback
      preserved).

## Files likely touched

- `src/game/steam.rs` — `LoginOutcome`, `automated_login`, `login_single_vm`,
  `login_single_vm_automated`, `login_vm_attempt`, `auto_login`, the inline
  `complete_guard_login_with_provider` loop (live-session send-keys submit),
  GuardRequired branch, tests. **(Owns the first edit; later phases rebase.)**
- `src/game/guard.rs` *(new, optional)* — `GuardProvider` enum/trait,
  `GuardPromptContext`, `PreSupplied`/`FailFast`.
- `src/game/mod.rs` — module declaration if `guard.rs` is added.
- `src/lib.rs` — extract the shared login orchestration callable from the
  `Commands::Steam` match arm (`lib.rs:892-989`); exit-code mapping.
- `src/bin/cluster-ctl.rs` — exit-code mapping if the new typed outcome surfaces
  here (`cluster-ctl.rs:5-11`).

## Pitfalls

- **Threading the provider through `par_each_vm`.** `login()` and `auto_login()`
  fan out via `par_each_vm` (`steam.rs:656-689`, `1228-1254`). The provider must
  be `Send + Sync` (or cloned per task). For this phase the only providers are
  `PreSupplied`/`FailFast`, both trivially `Send + Sync`; keep it that way so
  Phase 04's `HelperGui` can add a queue without reworking the signature.
  *Recovery:* make `GuardProvider` `Clone + Send + Sync`; pass by reference.
- **Do NOT kill the parked session to re-attempt with a positional code.** This
  was the original (corrected) mistake. The no-code bootstrap
  (`steamcmd +login user pass +quit`, `steam.rs:1369`) is what *triggers* the email
  code and parks at the prompt; feed the obtained code into **that** live session
  via `tmux send-keys`. Killing it and starting `steamcmd +login user pass <code>`
  fresh cannot work for email Guard (the code is issued by the attempt) and may
  trigger a second email / rate-limit. *Recovery:* mirror `guard()` —
  `steamcmd_guard_submit_script` + `wait_for_steamcmd_guard_completion`; only
  `kill_steamcmd_login_session` on cancel/unavailable.
- **Exit-code collision.** If `GuardCodeNeeded` maps to the same exit code as a
  generic failure, headless callers (Phase 06) can't distinguish "needs a code"
  from "broke". *Recovery:* reserve a dedicated exit code (e.g. 3) and document it.
- **Don't delete `guard()` yet.** Phase 07 owns retirement; CLI parse tests
  (`src/ui/cli.rs:962-999`) and Nix wrappers still reference it. Removing it here
  breaks the build and other phases.
- **Steamcmd prompt liveness.** The parked session must stay at the
  `Steam Guard code:` prompt long enough for the operator to fetch the emailed
  code (seconds to ~2 min). `guard()` already relies on this across a separate
  invocation, so it holds; confirm during Phase 05 host verification.

## Reference

- Dossier: [steam-login-iced-overhaul-research.md](../steam-login-iced-overhaul-research.md)
  — incompatibilities #1, #5, #6, #7, #11; "Candidate Next Steps" 2–4.
- Pivot branch: `src/game/steam.rs:1320-1333`; `LoginOutcome` at
  `steam.rs:1281-1285`; bootstrap at `steam.rs:1348-1380`; submit/wait at
  `steam.rs:1492-1550`; CLI dispatch at `src/lib.rs:892-989`.
- Next: Phase 02 (context detection) consumes the provider seam; Phase 04 adds
  the `HelperGui` provider.

## Execution Notes

2026-05-29:

- Shipped: typed `LoginOutcome { Completed, GuardCodeNeeded, ManualCompletionNeeded }`,
  the `GuardProvider` enum seam (`PreSupplied`/`FailFast`/`Stdin`/`HelperGui`), the
  shared login callable in `src/lib.rs`, and the inline GuardRequired handling in
  `complete_guard_login_with_provider`.
- **Mechanism correction:** the first cut submitted via a fresh positional
  `steamcmd_bootstrap_with_code_command` (single-shot `+login user pass <code>`).
  This was wrong for email Steam Guard (the code is issued *by* the login attempt,
  so it cannot be pre-supplied). Corrected to submit into the **live** session via
  `steamcmd_guard_submit_script` (`tmux send-keys`) +
  `wait_for_steamcmd_guard_completion`, looping on invalid-code re-prompt; the
  positional command + its test were removed; the no-code bootstrap now has no
  positional form. Verified: `cargo build`, `cargo clippy --all-targets -- -D
  warnings`, `cargo test` (475 unit + 13 integration) all green.
