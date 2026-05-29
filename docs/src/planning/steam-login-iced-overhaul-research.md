# Steam Login Overhaul (auto-steamcmd + iced pinentry dialog) Research Dossier

## Goal And Trigger

The operator wants to overhaul the Steam login system so that:

1. Login is performed **automatically** through `steamcmd` whenever a missing or
   expired login session is detected.
2. When a one-time Steam Guard code is requested, a small **iced** GUI dialog
   pops up on the host, *pinentry-style*; the operator types the code in and the
   in-progress login **continues to completion in the same invocation** — not the
   current two-phase "VM left running, run a separate command later" model.
3. The whole flow stays **compatible with AI / headless / non-interactive
   workflows** (this tool ships an MCP server and runs under systemd timers and
   CI where no display exists).
4. Surrounding features that are incompatible with this flow are identified and
   redesigned or retired.

**Topology (settled):** the iced dialog renders on the **host** ("our side"),
never inside a VM. steamcmd runs in the VM and is where the `Steam Guard code:`
prompt originates; the host detects it over SSH, the host-side iced dialog
collects the code, and the host relays it back into the VM's **live** steamcmd
session via `tmux send-keys`. The only path that surfaces Guard *inside the guest*
today is the legacy `interactive_login()` VNC flow, which this dossier marks
**Retire** for exactly that reason (incompatibility #4). Where the *dialog*
renders (host) is independent of where *steamcmd* runs (in-VM today; moving it
host-side is Open Decision #1).

> **Correction (2026-05-29) — code delivery dictates the submit mechanism.** An
> earlier draft of this dossier recommended submitting via a fresh single-shot
> `steamcmd +login user pass <code>`. That is **wrong for email Steam Guard**, the
> always-available default: Steam *issues* the one-time code in response to the
> login attempt (it mails the code only when steamcmd tries to log in from an
> unrecognized machine), so there is no code to pass beforehand. The correct,
> always-works mechanism is: run the no-code `steamcmd +login user pass` bootstrap
> (this triggers the email and parks at the prompt) → obtain the code (dialog /
> stdin / `--code` / env) → relay it into the **live** session via `tmux
> send-keys` → wait/retry → finish, all in one invocation. The single-shot
> positional form only ever fits the on-demand mobile authenticator and is **not**
> the primary path; `--code`/`STEAMPIPE_GUARD_CODE` is an authenticator/automation
> convenience fed through the same send-keys path. The "TOTP rotation race"
> concern below is largely moot for the inline flow (the code is entered and
> relayed within seconds); it only bit the old *deferred separate-command* model.

This dossier collects the evidence so a follow-on implementation plan can act. It
does **not** itself implement the change. Research combined direct source reading
with a 14-agent evidence-gathering workflow (8 subsystem readers, 3 external
research dossiers on iced / pinentry / Steam-Guard mechanics, and 3 cross-cutting
synthesis passes); the most load-bearing facts were re-verified by hand and are
cited below.

## Current Reality

### The login flow today is fundamentally two-phase

`steamcmd` runs **inside the VM** in a tmux session named
`steampipe-steamcmd-login`. A runner shell script execs
`steamcmd +login '{user}' '{pass}' +quit` with **no positional code argument**
([src/game/steam.rs:1348-1380](../../../src/game/steam.rs#L1348-L1380), exact
command at [steam.rs:1369](../../../src/game/steam.rs#L1369)) and writes its exit
code to a STATUS file. The host polls a wait script that greps the VM-side log for
the literal string `Steam Guard code:`
([steam.rs:1454-1490](../../../src/game/steam.rs#L1454-L1490), detection at
[steam.rs:1472](../../../src/game/steam.rs#L1472)).

When a Guard code is required, `automated_login` returns
`LoginOutcome::LeftRunning`, leaves the VM and tmux session running, and prints
guidance to run a **separate** command later:

> `Complete from another shell with: cluster-ctl steam guard <vm> --code <CODE>`
> ([steam.rs:1322-1333](../../../src/game/steam.rs#L1322-L1333), `LoginOutcome`
> enum at [steam.rs:1281-1285](../../../src/game/steam.rs#L1281-L1285))

The second phase is the public `guard()` entry point
([steam.rs:820-941](../../../src/game/steam.rs#L820-L941)): it re-attaches to the
still-running tmux session in a fresh process, submits the code via
`tmux send-keys` ([steam.rs:1492-1506](../../../src/game/steam.rs#L1492-L1506)),
waits for completion with an invalid-code retry marker
([steam.rs:1508-1550](../../../src/game/steam.rs#L1508-L1550)), runs
`finish_steam_login`, and shuts the VM down. Code entry is the synchronous,
TTY-only, blocking `read_guard_code_from_stdin()`
([steam.rs:958-963](../../../src/game/steam.rs#L958-L963)) when `--code` is omitted.

This deferred-submit model is **unsafe for mobile-authenticator codes**: TOTP
codes rotate every ~30 s and each supersedes the last, so by the time a separate
`steam guard --code` runs the code is likely stale — a failure mode the code
already acknowledges
([steam.rs:919-924](../../../src/game/steam.rs#L919-L924)). The wait loops run
`seq 1 300` (~300 s), far exceeding the TOTP window
([steam.rs:1460](../../../src/game/steam.rs#L1460),
[steam.rs:1520](../../../src/game/steam.rs#L1520)).

### There is a second, separate interactive path (VNC)

When no credentials are supplied, `login_single_vm` dispatches to
`interactive_login()` ([steam.rs:1113-1119](../../../src/game/steam.rs#L1113-L1119),
[steam.rs:1787-1864](../../../src/game/steam.rs#L1787-L1864)). It starts
`sway` + `wayvnc` (port 5900) + Steam GUI **inside the VM**, prints
`VNC into <ip>:5900`, and drives a blocking stdin REPL on the host
(`[p]` paste via `wtype`, `[Enter]` done, `[Ctrl+C]` cancel). The operator
completes the Steam Guard step inside the guest GUI; there is no programmatic
completion detection, and a non-blocking stdin dead-ends to `LeftRunning`.

### Session detection already exists and is reusable

`accounts::classify_session_status`
([accounts.rs:313-319](../../../src/game/accounts.rs#L313-L319)) +
`SessionStatus{Ok,Stale,Expired,NoToken,Unknown}` +
`DEFAULT_WARN_WITHIN_DAYS=30` is a synchronous, pure-filesystem, headless-safe
check of the host-side login state. It already gates a "fill-the-gaps" skip in
`login()` ([steam.rs:695-719](../../../src/game/steam.rs#L695-L719)). **The
redesign changes only the post-detection action, not detection itself.** External
research confirms a Guard code is needed *only* on first login on a machine or
after the cached refresh token (~200-day lifetime) is deleted/expired/revoked, so
keying auto-login off `classify_session_status` is correct.

### There is no GUI anywhere, and no AI/MCP login surface

`iced` (and any GUI toolkit) is **absent** from
[Cargo.toml](../../../Cargo.toml) and `Cargo.lock` — this is a brand-new
dependency tree (winit/wgpu/Wayland/X11/Vulkan/fontconfig). The crate is
deliberately headless-friendly today (`vnc` + `ratatui`, no GUI crates;
[packages.nix:67-84](../../../nix/flake/packages.nix#L67-L84) builds with
`strictDeps=true` and zero `buildInputs`).

The MCP server exposes only `nix_run`, `cluster_status`, `cluster_logs`,
`cluster_deploy`, `cluster_stop`, `cluster_test`, `cluster_history` — **no login
or guard tool** ([mcp.rs:289-761](../../../src/mcp.rs#L289-L761)). MCP transport
is stdio, so stdout is reserved for JSON-RPC framing
([mcp.rs:805](../../../src/mcp.rs#L805)) and the code is careful to keep `println`
off it (`verbose: false` at [mcp.rs:630](../../../src/mcp.rs#L630)). The only
escape hatch, `nix_run`, spawns children with **no piped stdin**
([mcp.rs:312-313](../../../src/mcp.rs#L312-L313)), so it can pass a pre-known
`--code` arg but cannot answer an interactive prompt. The transparent post-`up`
`auto_login()` ([steam.rs:1208-1279](../../../src/game/steam.rs#L1208-L1279),
called at [lib.rs:869](../../../src/lib.rs#L869)) runs with `LoginLog::silent()`
and, on Guard, just prints to stderr and counts the VM as failed
([steam.rs:1240-1246](../../../src/game/steam.rs#L1240-L1246)). **Today an
AI/headless caller cannot complete a Guard-required login at all.**

### iced imposes hard, non-negotiable constraints (external research)

- Current iced is **0.14.0** (released 2025-12-07, MSRV 1.88). A trivial OTP modal
  fits the modern `iced::run(update, view)` API with `text_input` + `button`.
- **Main-thread event loop (hard):** iced runs on winit; winit's `EventLoop` must
  be created and driven on the OS **main** thread. Calling it from a worker/tokio
  task panics. Only one event loop per process.
- **Blocking (hard):** `iced::run()` blocks the calling thread until the window
  closes; it cannot be `.await`ed.
- **Headless startup is a PANIC, not a Result (hard):** with no
  `DISPLAY`/`WAYLAND_DISPLAY` or no client lib, iced *panics* during event-loop
  creation (winit's fallible `build()` is unwrapped). The headless fallback must
  **not** rely on matching `iced::Error` — it must pre-check env vars and/or
  isolate iced in a child process whose non-zero exit is observable.
- steampipe builds its multi-thread tokio runtime by hand and `block_on`s on the
  OS main thread (**not** `#[tokio::main]`)
  ([lib.rs:543-548](../../../src/lib.rs#L543-L548)) — the main thread is
  reclaimable, which is the clean integration seam.
- Prefer the **tiny-skia software renderer** over wgpu to avoid a hard GPU
  dependency.

### pinentry is a validated blueprint (external research)

pinentry is a *separate process* gpg-agent spawns over a pipe (Assuan protocol);
the core owns secrets/policy, the helper only renders a prompt. Frontends
(gtk/qt/curses/tty) are interchangeable and selected by display availability with
automatic fallback; the caller pushes context over the protocol (not inherited
env); `PINENTRY_USER_DATA` forces headless/TTY mode; every prompt has a mandatory
timeout that collapses cancel/timeout/no-frontend into one denial signal so the
core never hangs. This maps almost 1:1 onto the desired AI-compatible Guard
prompt.

## Evidence Inventory

| Artifact | What it proves |
|----------|----------------|
| [steam.rs:1281-1333](../../../src/game/steam.rs#L1281-L1333) | `LoginOutcome::LeftRunning` two-phase model; GuardRequired emits "Complete from another shell with: cluster-ctl steam guard". The single highest-impact pivot to redesign. |
| [steam.rs:1369](../../../src/game/steam.rs#L1369) | Bootstrap runs `steamcmd +login '{user}' '{pass}' +quit` with **no** positional code. |
| [steam.rs:820-941](../../../src/game/steam.rs#L820-L941) | `guard()` is the entire second-phase entry point (re-attach tmux, send-keys, finish, shutdown). |
| [steam.rs:958-963](../../../src/game/steam.rs#L958-L963) | `read_guard_code_from_stdin()` blocking TTY-only code entry. |
| [steam.rs:1787-1864](../../../src/game/steam.rs#L1787-L1864) | `interactive_login()` VNC + host stdin REPL (no-creds path). |
| [steam.rs:919-924](../../../src/game/steam.rs#L919-L924) | Code already acknowledges "rejected or superseded by a newer code" — the TOTP race. |
| [steam.rs:549-598](../../../src/game/steam.rs#L549-L598) | `LoginLog{Stdout,Buffered,Silent}` — no structured event/result channel for a GUI or MCP. |
| [steam.rs:656-689](../../../src/game/steam.rs#L656-L689) | `login()` fans out N VMs via `par_each_vm` with buffered logs — conflicts with a single modal dialog. |
| [steam.rs:1919-1936](../../../src/game/steam.rs#L1919-L1936) | `redact_sensitive()` secret scrubbing — durable discipline the new channels must keep. |
| [accounts.rs:313-319](../../../src/game/accounts.rs#L313-L319), [accounts.rs:24-55](../../../src/game/accounts.rs#L24-L55) | `classify_session_status` + `SessionStatus` — reusable auto-login trigger. |
| [accounts.rs:344-352](../../../src/game/accounts.rs#L344-L352) | `find_refresh_token_file` prefers `config/<steamID>/local.vdf`, falls back to `config/config.vdf`. |
| [steam.rs:1628-1666](../../../src/game/steam.rs#L1628-L1666) | `steamcmd_state_sync_script` copies `config/loginusers.vdf`, `config.vdf`, `registry.vdf`, `ssfn*` — **never** `config/<steamID>/local.vdf`. (Detection/sync mismatch.) |
| [mcp.rs:289-761](../../../src/mcp.rs#L289-L761) | 7 MCP tools, none for login/guard; `nix_run` pipes no stdin ([mcp.rs:312-313](../../../src/mcp.rs#L312-L313)). |
| [lib.rs:543-548](../../../src/lib.rs#L543-L548) | Hand-built multi-thread tokio runtime `block_on` on the OS main thread (iced integration seam). |
| [Cargo.toml:21-53](../../../Cargo.toml#L21-L53) | No iced/winit/wgpu; `vnc = "0.4"` present. |
| [src/game/capture.rs:66-150](../../../src/game/capture.rs#L66-L150), [lib.rs:371-372](../../../src/lib.rs#L371-L372) | `vnc` crate is the **screenshot** backend — retiring VNC login does **not** free it. |
| [packages.nix:67-84](../../../nix/flake/packages.nix#L67-L84) | crane build: `strictDeps=true`, zero `buildInputs`, `makeWrapper` only. |
| [cli.rs:613-702](../../../src/ui/cli.rs#L613-L702) | `SteamAction{Login,Guard,Check,Warm,Start,Accounts,Info,CleanLogins}`; Guard `target`+`--code`; no `--non-interactive`/`--no-gui` global. |
| [runner.rs:914-933](../../../src/harness/runner.rs#L914-L933) | Harness `ensure_steam` path assumes a logged-in session, bails on failure, no `classify_session_status`. Reachable from MCP `cluster_test`. |
| [runner.rs:982-983](../../../src/harness/runner.rs#L982-L983) | `--steam-target-id` uses `accounts::local_steam_id()` which reads the **host operator's** `~/.local/share/Steam`, not a per-VM dir. |
| [nix/lib/test-cluster.nix:390-427](../../../nix/lib/test-cluster.nix#L390-L427) | Five login/guard wrappers: `cluster-steam-login`, `cluster-steam-guard`, `cluster-1v1-steam-login`, `cluster-1v1-steam-guard`, `cluster-steam-login-persist`. |
| [nix/lib/warm-timer.nix:86-108](../../../nix/lib/warm-timer.nix#L86-L108) | `cluster-ctl steam warm` one-shot systemd service — a second display-less caller. |
| [steam.rs:444](../../../src/game/steam.rs#L444) | `check()` prints "To fix not-logged-in: run nix run .#cluster-steam-login" — user-facing string tied to the redesigned UX. |
| [tests/cli_steam_standalone.rs:388,417,452,498](../../../tests/cli_steam_standalone.rs) | Integration tests assert **exact stdout** for `steam accounts`/`clean-logins`/`info`. Login/guard have **no** integration coverage. |

Commands and tools run during research:

```bash
rg -n "steamcmd|steam guard|wayvnc|vnc|iced|auto_login|read_guard_code" src
git diff --stat; git diff src/core/state.rs        # working tree = rustfmt-only noise
git show c061ae4 --stat; git log --oneline -20      # headless-steam-login plan + shipped commits
# 14-agent Workflow: 8 subsystem readers + 3 external (iced 0.14, pinentry, steamcmd Guard) + 3 synthesis
```

## Existing Plan Status

The existing `headless-steam-login` plan set
([docs/src/planning/headless-steam-login-research.md](./headless-steam-login-research.md)
and phases 01–03) is about the **virtiofsd lifecycle and parallel login** — it
never mentions a GUI, iced, or a host Guard dialog. The iced/pinentry angle is
genuinely new ground. Audit of that plan against current state:

| Plan item | Status | Evidence |
|-----------|--------|----------|
| Phase 01 — virtiofsd lifecycle (spawn before QEMU, wait for socket, reap on stop) | **Done** | commits `69424ad`, `24d6a0a`; [backend.rs:260-284](../../../src/core/backend.rs#L260-L284), [state.rs](../../../src/core/state.rs) PID helpers |
| Phase 02 — parallelize `login()` + skip-bounce-when-reachable | **Done** | commit `08ea98f`; [steam.rs:656-689](../../../src/game/steam.rs#L656-L689) |
| Fill-the-gaps session detection + `--force` | **Done** | commit `9cd9f42`; [steam.rs:695-719](../../../src/game/steam.rs#L695-L719) |
| Phase 03 — docs refresh (`steam-credentials.md`) | **Done, but superseded by this overhaul** | the refreshed doc still describes the VNC two-phase model |
| Real-host (atlas) verification of any login path | **Not done** | `.calibration.json` marks the set "partial"; no live-account run ever happened |

**Carry forward:** the shipped work (virtiofsd lifecycle, parallel login,
fill-the-gaps detection) is durable and must not regress. **Retire/re-scope:** the
Phase 03 docs and the plan set's framing of VNC + `steam guard` as the canonical
flow are superseded by this overhaul.

## Work That Should Survive

- **Session detection is reused verbatim** as the auto-login trigger:
  `classify_session_status` + `SessionStatus` + `DEFAULT_WARN_WITHIN_DAYS`
  ([accounts.rs:313-319](../../../src/game/accounts.rs#L313-L319)). Synchronous,
  filesystem-only, headless-safe. The redesign keys auto-login off this and must
  **not** force a fresh password login when a valid token exists (external
  research: that would needlessly demand a new code).
- **Parallel login + skip-bounce-when-reachable** (Phase 02) must be preserved for
  VMs that do **not** need a code ([steam.rs:656-689](../../../src/game/steam.rs#L656-L689)).
- **The in-VM SteamCMD/tmux engine survives as the transport** — bootstrap
  ([steam.rs:1348-1380](../../../src/game/steam.rs#L1348-L1380)), guard-prompt
  detection ([steam.rs:1472](../../../src/game/steam.rs#L1472)), and submit-with-
  invalid-code-retry ([steam.rs:1492-1550](../../../src/game/steam.rs#L1492-L1550)).
  The redesign drives these in **one** invocation instead of across two processes;
  the low-level scripts barely change.
- **Host-side state persistence via the writable virtiofs `steam-state` share** —
  `loginStateDir/<vm>/` *is* the VM's `~/.local/share/Steam`, so steamcmd/Steam
  writes land where `accounts.rs` reads. No new host-write path is needed; the
  inline flow must keep the VM up long enough to flush before teardown
  ([steam-credentials.md:58-63](../../../docs/src/configuration/steam-credentials.md#L58-L63)).
- **`redact_sensitive()` discipline** ([steam.rs:1919-1936](../../../src/game/steam.rs#L1919-L1936))
  must extend to the new dialog/IPC/MCP/event channels; the Guard code is a secret
  to redact and zeroize.
- **Credentials plumbing unchanged** — `CredentialsMap` from TOML/age via
  ssh-agent ([credentials.rs:7-97](../../../src/core/credentials.rs#L7-L97)),
  gated by `command_uses_credentials` (Up | steam login | steam guard) at
  [lib.rs:259-267](../../../src/lib.rs#L259-L267). The Guard code is the *only*
  value not in this map.
- **`vnc` crate stays** — it is the screenshot/visual-regression backend
  ([capture.rs:66-405](../../../src/game/capture.rs)), not login infrastructure.
  `sway`/`grim`/`compositor.rs` readiness also stay (game-run + capture).
- **The reclaimable OS main thread** ([lib.rs:543-548](../../../src/lib.rs#L543-L548))
  is the clean iced integration seam (favoring a separate helper binary).

## Blockers And Missing Artifacts

- **State-sync vs detection mismatch — must verify on a real host (highest-risk
  blocker for the "auto-login on missing session" goal).** `find_refresh_token_file`
  prefers `config/<steamID>/local.vdf`
  ([accounts.rs:344-352](../../../src/game/accounts.rs#L344-L352)) but
  `steamcmd_state_sync_script` never copies that per-SteamID file — only
  `config.vdf`/`loginusers.vdf`/`registry.vdf`
  ([steam.rs:1628-1666](../../../src/game/steam.rs#L1628-L1666)). If a steamcmd-only
  login writes the modern refresh-token JWT into `local.vdf` rather than
  `config.vdf`, detection reads `NoToken`/`Expired` **forever** and an auto-login
  loop never converges.
  - **Producer:** `steamcmd_state_sync_script` and/or `find_refresh_token_file`.
  - **Regenerate/validate:** on the atlas host, run a real steamcmd-only login for
    one account, then inspect which file under `~/.local/share/Steam/config/`
    holds the `eyJ…` JWT; confirm `classify_session_status` returns `Ok`. If the
    JWT lives in `local.vdf`, extend the sync to copy `config/<steamID>/local.vdf`.
- **No real-host (atlas) verification of login timing exists.** Every claim about
  steamcmd Guard-prompt timing, tmux-session liveness across a dialog wait, and
  whether `finish_steam_login`'s artifacts satisfy `classify_session_status` is
  unverified against a live Steam account (`.calibration.json` = "partial"). The
  single-invocation timing assumption (TOTP ~30 s vs dialog dwell vs steamcmd's
  own prompt timeout) has **zero empirical grounding** in this repo.
- **No host display/TTY/non-interactive detection in the login path.** There is no
  `--non-interactive`/`--no-gui` global flag and no `$DISPLAY`/`$WAYLAND_DISPLAY`
  probe; the only headless heuristic is the brittle EAGAIN-on-stdin check
  ([steam.rs:1876-1882](../../../src/game/steam.rs#L1876-L1882)), which does not
  detect absence of a display. This gate is a **prerequisite** for safely
  attempting the iced modal (which panics, not errors, when headless).
- **No login/guard surface in MCP** ([mcp.rs:289-761](../../../src/mcp.rs#L289-L761)).
  If AI-driven login is in scope, a non-GUI code-submission channel must be built.

## Risks And Constraints

- **Authenticator code freshness (minor for the inline flow).** A mobile
  authenticator (TOTP) code rotates ~30 s; an email code is valid much longer but
  single-use. The inline flow enters the code and relays it via `send-keys` within
  seconds, so freshness is not a real concern. The hazard only existed in the old
  *deferred separate-command* model, where minutes could pass before submit
  ([steam.rs:919-924](../../../src/game/steam.rs#L919-L924)). Do **not** "fix" this
  with a positional `+login user pass code`: for email Guard the code does not
  exist until the no-code attempt triggers it, so the attempt-then-relay model is
  mandatory, not optional.
- **Parallel login vs a single modal.** `login()` and `auto_login()` run N VMs
  concurrently ([steam.rs:656-689](../../../src/game/steam.rs#L656-L689),
  [steam.rs:1228-1254](../../../src/game/steam.rs#L1228-L1254)); only one iced
  event loop/dialog can be active per process. Concurrent Guard prompts must be
  serialized/queued, or interactive Guard restricted to single-target login.
- **iced headless = panic, and main-thread/blocking.** A robust display pre-check
  and process isolation are mandatory (see external constraints above); otherwise
  CI/MCP/timer runs crash or hang.
- **Post-login state persistence.** `finish_steam_login` must run *after* the code
  is accepted so `loginusers.vdf` + the JWT land in the host share; "completing" on
  steamcmd success alone risks the detection loop above
  ([steam.rs:1552-1626](../../../src/game/steam.rs#L1552-L1626)).
- **Secret leakage across the new boundary.** The OTP (and password) must route
  through `redact_sensitive`, transit any helper pipe without logging, and be
  zeroized. The live steamcmd LOG tail surfaced on failure can contain the
  just-typed code.
- **MCP completion lifetime vs tool timeout.** A held login awaiting an
  out-of-band code can outlive the `nix_run`/MCP 300 s timeout
  ([mcp.rs:300](../../../src/mcp.rs#L300)) and collide with the 300-iteration
  in-VM waits; a two-call MCP protocol needs an explicit token TTL.
- **Dependency/closure growth.** iced adds a winit/wgpu/Wayland/Vulkan/fontconfig
  tree the current zero-`buildInputs` crane build and the rs-harbor dev shell do
  not provide ([packages.nix:67-84](../../../nix/flake/packages.nix#L67-L84)). A
  headless MCP/CI/timer binary should not be forced to link it.
- **Downstream breakage.** Five Nix wrappers reference `steam login`/`steam guard`
  ([test-cluster.nix:390-427](../../../nix/lib/test-cluster.nix#L390-L427)),
  including `cluster-steam-login-persist`; consumer flakes (e.g. regicide) and CLI
  parse tests ([cli.rs:962-999](../../../src/ui/cli.rs#L962-L999)) depend on
  `guard`. Demote, do not delete.
- **Uncovered dependents** the plan must account for: the harness `ensure_steam`
  path (no session check, reachable from MCP `cluster_test`,
  [runner.rs:914-933](../../../src/harness/runner.rs#L914-L933)); `--steam-target-id`
  reading the host operator's own Steam install
  ([runner.rs:982-983](../../../src/harness/runner.rs#L982-L983)); the `warm`
  systemd timer as a second display-less caller
  ([warm-timer.nix:86-108](../../../nix/lib/warm-timer.nix#L86-L108)); teardown
  `graceful_stop_steam` that flushes session tokens
  ([run.rs:36](../../../src/game/run.rs#L36), [mcp.rs:526](../../../src/mcp.rs#L526));
  and the exact-stdout integration tests with **no** login/guard coverage
  ([tests/cli_steam_standalone.rs](../../../tests/cli_steam_standalone.rs)).

## Surrounding Features Incompatible With The New Flow

Consolidated and de-duplicated across all subsystems. Disposition is a
recommendation for the follow-on plan, not a commitment.

| # | Feature | Subsystem | Why incompatible | Disposition |
|---|---------|-----------|------------------|-------------|
| 1 | `LoginOutcome::LeftRunning` two-phase model | login core | Bails on Guard, leaves VM running, defers to a separate command; `Ok(LeftRunning)` makes "guard pending" indistinguishable from success in the exit code ([steam.rs:1281-1333](../../../src/game/steam.rs#L1281-L1333)) | **Redesign** → inline pause + typed outcome `Completed \| GuardRequired \| Failed` |
| 2 | Standalone `steam guard` subcommand + `guard()` | login core + CLI | Exists only as phase two; runtime "exactly one VM" conflicts with parallel login ([steam.rs:820-941](../../../src/game/steam.rs#L820-L941), [cli.rs:636-643](../../../src/ui/cli.rs#L636-L643)) | **Keep as fallback** → fold logic into unified path; keep `--code` as headless escape hatch |
| 3 | `read_guard_code_from_stdin()` | login core | Synchronous, TTY-only, blocks; unusable under MCP/headless; would block a GUI loop ([steam.rs:958-963](../../../src/game/steam.rs#L958-L963)) | **Keep as fallback** behind explicit TTY gating |
| 4 | `interactive_login()` VNC + host stdin REPL | login core + VNC infra | Needs a human vncviewer into the guest GUI, no programmatic completion, dead-ends to `LeftRunning` ([steam.rs:1787-1864](../../../src/game/steam.rs#L1787-L1864)) | **Retire** (keep only as deep opt-in fallback if steamcmd alone can't establish machine trust — open question) |
| 5 | steamcmd Guard submit deferred to a *separate* `steam guard` process | login core | The no-code bootstrap correctly triggers the email/prompt, but the code is submitted later from another invocation, so a parked session lingers and the outcome is ambiguous ([steam.rs:1369](../../../src/game/steam.rs#L1369), [steam.rs:1492-1506](../../../src/game/steam.rs#L1492-L1506)) | **Redesign** → keep the no-code bootstrap + `tmux send-keys` submit, but drive it **inline** in one invocation (not positional — email codes are issued by the attempt) |
| 6 | Blocking single-shot SSH wait scripts (~300 s; GUI validation 2×240 s) | login core | A GUI event loop can't drive these inline; MCP gets no progress until return ([steam.rs:1388-1452](../../../src/game/steam.rs#L1388-L1452), [steam.rs:1668-1785](../../../src/game/steam.rs#L1668-L1785)) | **Redesign** → run cascade on a background task, surface status via channel |
| 7 | `LoginLog{Stdout,Buffered,Silent}` (no structured channel) | login core | A GUI needs an event stream to know when to raise the dialog; MCP needs a machine-readable outcome; Silent swallows the Guard signal; stdout is MCP's JSON-RPC channel ([steam.rs:549-598](../../../src/game/steam.rs#L549-L598)) | **Redesign** → typed login-event/result type routed through `redact_sensitive` |
| 8 | No login/guard MCP tool | MCP surface | AI caller cannot initiate a Guard-aware login or supply a code; `nix_run` pipes no stdin ([mcp.rs:289-761](../../../src/mcp.rs#L289-L761)) | **Redesign** → `cluster_login` + `cluster_submit_guard{token,code}` two-call protocol, never pops a window |
| 9 | Parallel buffered/silent login vs single modal | login core + CLI | One dialog can't map onto N simultaneous prompts ([steam.rs:656-689](../../../src/game/steam.rs#L656-L689), [steam.rs:1228-1254](../../../src/game/steam.rs#L1228-L1254)) | **Redesign** → serialize/queue dialogs or restrict GUI prompt to single-target |
| 10 | No display/TTY/non-interactive detection; no `--non-interactive`/`--no-gui` | CLI + login core | Needed to gate the modal; iced *panics* when headless ([cli.rs:104-145](../../../src/ui/cli.rs#L104-L145), [steam.rs:1876-1882](../../../src/game/steam.rs#L1876-L1882)) | **Redesign** → explicit flag + `$WAYLAND_DISPLAY`/`$DISPLAY`/TTY probe; helper-process isolation |
| 11 | Login/guard dispatch inlined in `lib.rs`, signalled via printed text + `Ok(())` | CLI dispatch + bin | Can't drive login from both CLI (iced) and a new MCP tool; "guard pending" surfaces as text + Ok ([lib.rs:892-989](../../../src/lib.rs#L892-L989), [cluster-ctl.rs:5-11](../../../src/bin/cluster-ctl.rs#L5-L11)) | **Redesign** → shared callable returning a typed outcome; distinct exit code for headless "guard needed" |
| 12 | In-VM GUI login packages (wayvnc, wl-clipboard, wtype, in-VM Steam GUI) + 4 GiB login-runner sizing + per-login `pkill wayvnc/sway` | Nix VM image + sizing + teardown | Provisioned chiefly for the in-VM Guard window; a host iced + steamcmd login needs only steamcmd (+tmux) ([cluster-vm-base.nix:76-129](../../../nix/cluster-vm-base.nix#L76-L129), sizing [test-cluster.nix:195](../../../nix/lib/test-cluster.nix#L195), teardown [steam.rs:949](../../../src/game/steam.rs#L949)) | **Keep as fallback** → gate off the default path; keep `pkill tmux`; sway/grim stay for capture |
| 13 | Headless package + dev-shell closure with zero GUI toolkit | Nix flake + Cargo.toml | iced introduces winit/wgpu/Wayland/Vulkan/fontconfig the strict-deps crane build and rs-harbor shell don't provide; a headless binary shouldn't link it ([Cargo.toml:21-53](../../../Cargo.toml#L21-L53), [packages.nix:67-84](../../../nix/flake/packages.nix#L67-L84)) | **Redesign** → separate helper crate/binary (preferred) or feature-gated iced + tiny-skia; add buildInputs only there |
| 14 | User-facing docs/UX enshrining VNC two-mode/two-phase login | docs + website + CLI help | Documents the obsolete "VNC in + complete Guard" and "Complete from another shell" flow; omits the headless/MCP contract ([steam-credentials.md:23-89](../../../docs/src/configuration/steam-credentials.md#L23-L89), [README.md:177-207](../../../README.md#L177-L207), [steam.rs:444](../../../src/game/steam.rs#L444)) | **Redesign** → rewrite to auto-steamcmd + host dialog default; document no-display behavior; note vnc/wayvnc are screenshots-only |

## Candidate Next Steps

Sequencing hints for the follow-on plan, not commitments.

1. **Verify the state-sync/detection mismatch on a real host first** (gates the
   whole "auto-login on missing session" premise). See Blockers. Cheap, and
   everything else assumes convergence.
2. **Drive the existing `tmux send-keys` submit inline.** Keep the no-code
   `steamcmd +login user pass` bootstrap (it makes Steam *issue* the email code and
   parks at the `Steam Guard code:` prompt), obtain the code, and relay it into the
   **live** session via the existing `steamcmd_guard_submit_script` +
   completion-wait — in the *same* invocation, looping on invalid-code re-prompt.
   Do **not** use a positional `+login user pass code` (email codes don't exist
   until the attempt triggers them; positional only fits the on-demand
   authenticator). Remove the "retry from another shell with the latest code"
   control flow.
3. **Introduce a `GuardCodeProvider` abstraction** — one seam for all channels
   (HelperGui / Stdin-TTY / McpQueue / PreSupplied `--code`|env), selected by
   detected context. This replaces the `LeftRunning` early-return at
   [steam.rs:1322](../../../src/game/steam.rs#L1322) with: *obtain code → submit →
   wait-for-completion (invalid-code retry via the existing post-submit marker) →
   `finish_steam_login`*, in one invocation.
4. **Extract a shared login orchestration callable** returning a typed outcome
   (`Completed | GuardRequired | Failed`) that the CLI handler, the iced flow, and
   a new MCP tool all consume ([lib.rs:892-989](../../../src/lib.rs#L892-L989)).
   Reserve a distinct exit code for the headless "guard code needed" case.
5. **Add an explicit interactivity/display gate** (`--non-interactive`/`--no-gui`
   global flag + `$WAYLAND_DISPLAY`/`$DISPLAY`/TTY probe + MCP-context detection)
   before any modal attempt.
6. **Build the Guard prompt as a separate pinentry-style helper binary** (see
   Recommended below), launched via `tokio::process::Command`, returning the code
   on stdout; non-zero exit / empty stdout = cancelled/unavailable.
7. **Add the MCP code path** (if AI-driven login is in scope): a `cluster_login`
   tool that returns structured `{guard_required, vm, token}` plus a
   `cluster_submit_guard{token, code}` tool, with a token TTL reconciled against
   the in-VM wait timeouts.
8. **Decide the fate of the parallel path, `warm` timer, and harness `ensure_steam`**
   relative to auto-login + Guard (Open Decisions).
9. **Rewrite docs/UX and update the `check()` guidance string**
   ([steam.rs:444](../../../src/game/steam.rs#L444)); retire/re-scope the
   `headless-steam-login` planning tree; demote VNC and `steam guard` to
   documented fallbacks; add the headless/MCP contract.

### Candidate architectures (from synthesis)

A `GuardCodeProvider` seam is common to all three; they differ only in where the
GUI runs.

- **Option A — Separate pinentry-style helper binary (RECOMMENDED).** A tiny
  `cluster-guard-prompt` binary owns iced/winit on its own OS main thread;
  `cluster-ctl` keeps its multi-thread tokio runtime untouched. It is the only
  option that satisfies *all* hard constraints at once: winit's main-thread/
  blocking requirement is physically isolated; the headless **panic** becomes an
  observable non-zero child exit (robust by construction, not best-effort
  `catch_unwind`); the heavy GUI closure lands only in the helper crate so MCP/CI/
  timer builds stay GUI-free; and it is a near-literal implementation of the
  pinentry model the brief asks to emulate. Cost: a second binary + a small
  argv/stdout protocol + runtime discoverability of the helper.
- **Option B — In-process iced on the reclaimed main thread.** Move the tokio
  runtime to a worker thread, reserve main for `iced::run` on demand, feed it via
  a channel. No second binary, simplest secret handling — but iced deps become
  **mandatory in the one binary** (bloating every headless consumer), headless
  safety relies on best-effort `catch_unwind` across the event-loop boundary, and
  it invasively reshapes startup ([lib.rs:543](../../../src/lib.rs#L543)).
- **Option C — Reuse the OS `pinentry`/`zenity`/`kdialog` program (no iced).**
  Zero new GUI dependency tree; inherits pinentry's display→TTY fallback. But adds
  a *runtime* dependency on a prompt program being installed (not guaranteed on
  NixOS-operator hosts), gives less control over Steam-Guard-specific messaging,
  and deviates from the stated "iced dialog" direction.

Regardless of option: the headless/AI contract is identical — under MCP/no-display
the GUI is never spawned; the provider selects the MCP-queue path or fails fast
with a typed "guard code required" error (distinct exit code), and a pre-supplied
`--code`/env short-circuits the provider for fully-unattended runs. Prefer the
**tiny-skia** software renderer to avoid a hard GPU dependency.

## Open Decisions For The User

These materially shape the implementation plan:

1. **Steamcmd location & code transport — SETTLED.** steamcmd stays in-VM; the
   no-code `+login user pass` bootstrap triggers the email/prompt and the obtained
   code is relayed into the **live** session via `tmux send-keys`, inline in one
   invocation. No positional `+login user pass code` (email codes are issued by the
   attempt). `--code`/env is an authenticator/automation convenience on the same
   send-keys path. (Moving steamcmd host-side is explicitly *not* pursued.)
2. **Is AI-driven login actually in scope,** or is login an operator-only host
   action that merely must never pop a window / block under MCP? If in scope,
   commit to the `cluster_login` + `cluster_submit_guard` MCP tool pair; if not,
   the headless contract simplifies to "fail fast with a typed guard-required
   error." (Note: an AI still cannot *obtain* an email/mobile code itself — that
   likely stays human-in-the-loop via a different channel, unless option 7 below.)
3. **Parallel login with multiple Guard-needing VMs:** serialize dialogs in a host
   queue, or restrict interactive Guard to single-target `steam login <vm>` (and
   keep `up`/`auto_login` non-interactive)? Decides whether Phase-02 parallel login
   must change.
4. **Should `up`'s silent `auto_login`** ([steam.rs:1237](../../../src/game/steam.rs#L1237))
   be allowed to raise a host dialog mid-cluster-bringup, or stay strictly
   non-interactive (Guard ⇒ actionable error)?
5. **Retire vs keep-as-fallback:** the standalone `steam guard --code` subcommand,
   the `cluster-steam-guard` Nix wrappers, and `interactive_login()` (VNC).
   Recommendation: keep all three as deep/headless fallbacks (consumer flakes
   depend on the wrapper; VNC may still be needed for GUI-only first-trust — open
   question).
6. **iced packaging:** separate helper crate/binary (recommended — sidesteps the
   question), compile-time Cargo feature gate (two package variants, headless build
   links zero GUI deps), or always-linked-runtime-gated (one variant, heavier
   closure for every consumer)?
7. **Fully-unattended login via stored TOTP `shared_secret`** (like
   `Weilbyte/steamcmd-2fa`)? This removes the human/dialog entirely for those
   accounts but adds a new secret to the credentials store (`VmCredentials` has no
   such field today, [credentials.rs:7-12](../../../src/core/credentials.rs#L7-L12)).
8. **Scope of auto-login:** only `up`/`steam login`, or also `warm`/`check`/the
   harness `ensure_steam` test path? `command_uses_credentials`
   ([lib.rs:259-267](../../../src/lib.rs#L259-L267)) loads creds only for
   Up/login/guard today, so the others *cannot* auto-login even if they detect a
   missing session — expanding this pulls the headless `warm` timer and MCP
   `cluster_test` into the Guard-fallback contract.
