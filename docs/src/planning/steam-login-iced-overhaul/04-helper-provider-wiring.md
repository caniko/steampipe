# Phase 04 — Wire the HelperGui provider (spawn helper) + concurrency queue

> **Status: shipped (2026-05-29).** `GuardProvider::HelperGui` spawns the helper
> via `tokio::process::Command`, maps exit codes (0→`Code`, 10/11→`Cancelled`,
> 12/other/spawn-error→`Unavailable`), trims/validates stdout, redacts + zeroizes
> the code, and serializes concurrent prompts via a shared `AsyncMutex`. Selection
> routes display + interactive + not-`--no-gui` to `HelperGui`. A **dev fallback**
> was added so a bare `cargo run -- steam login …` (which doesn't build the sibling
> crate) still shows the dialog, and the installed `cluster-ctl` wrapper now puts
> the helper package on `PATH` — see Execution Notes.

> **Recommended Codex model: GPT 5.5 high**
>
> Complex integration with two consequential correctness concerns: the OTP and
> password transit a process boundary (must be redacted, never logged, zeroized),
> and concurrent VM logins must serialize onto a single host dialog (a queue/
> mutex, or a deadlock/double-window bug). It is a sub-agent-role wiring task, but
> a subtle mistake here leaks a secret or hangs `login all`, so `high` rather than
> `medium`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phases 01, 02, and 03**: the
`GuardProvider` seam + selection policy (01/02) and the `cluster-guard-prompt`
binary + its argv/stdout/exit-code protocol (03) must exist. Rebase on the current
`src/game/steam.rs`.

## Goal

The `GuardProvider::HelperGui` variant spawns `cluster-guard-prompt` via
`tokio::process::Command`, passes the prompt context as argv, reads the entered
code from the child's stdout, and maps exit codes to `Code`/`Cancelled`/
`Unavailable`. The provider-selection policy (Phase 02 step 3) now routes the
"interactive + display + not `--no-gui`" case to `HelperGui` instead of falling
through. Concurrent VM logins that each need a code are serialized so only one
helper window is ever open at a time. The code is redacted everywhere and zeroized
after it is relayed into the live session (the provider only *obtains* the code;
the submit mechanism is `tmux send-keys` from Phase 01, not a positional login).

## Why this matters now

Phases 01–03 produced a seam, a selection policy with a stubbed GUI branch, and a
working helper binary. This phase closes the loop so an operator on a graphical
host gets the actual pinentry-style window during `cluster-ctl steam login <vm>`
and the login finishes in one invocation — the headline behavior of the whole
overhaul. It must reconcile the single-modal helper with the parallel
`par_each_vm` login fan-out (dossier incompatibility #9 / Open Decision #3).

## Out of scope

- Changing the helper binary (Phase 03 owns it). If the protocol needs a tweak,
  note it and coordinate, but prefer adapting the provider to the existing
  protocol.
- The MCP/warm/check fail-fast paths — Phase 06.
- Retiring stdin/VNC fallbacks — Phase 07.

## Plan

1. **Locate the helper at runtime.** Resolve `cluster-guard-prompt` by: (a) an env
   override `STEAMPIPE_GUARD_PROMPT_BIN`; (b) alongside the current exe
   (`std::env::current_exe()` dir); (c) `PATH`. If unresolved, the provider
   returns `Unavailable` (→ fall back to stdin if a TTY, else fail fast) — never
   hang. The Nix wrapper (Phase 03) should put the helper on `PATH`/next to
   `cluster-ctl` for the packaged case.
2. **Implement `HelperGui::request`** using `tokio::process::Command`:
   - argv from `GuardPromptContext`: `--vm`, `--user`, `--reason`,
     `--invalid-retry` (when re-prompting), `--timeout-secs`.
   - `stdin(Stdio::null())`, `stdout(Stdio::piped())`, inherit stderr (helper keeps
     stderr clean per Phase 03).
   - Await the child; map exit code: `0` → `Code(trimmed_stdout)`; `10`/`11` →
     `Cancelled`; `12`/spawn-error/any other → `Unavailable`.
   - Trim a single trailing newline from stdout; reject empty stdout on exit 0 as
     `Cancelled`.
3. **Wire selection.** In the Phase 02 policy, replace the stubbed GUI branch
   (step 3.3) with `HelperGui` when `display && !--no-gui && !non_interactive`.
4. **Serialize prompts (single modal at a time).** Reuse the async mutex Phase 02
   introduced for interactive prompts so that across `par_each_vm`
   (`steam.rs:656-689`, `auto_login` `1228-1254`) at most one helper process runs
   at once. The mutex wraps `request`; other VM tasks await their turn. Document
   that VMs whose login needs no code proceed fully in parallel — only the
   code-needing ones queue on the dialog.
5. **Secret handling across the boundary.** The code returned from the helper is a
   secret: add it to the `redact_sensitive` slices for the `send-keys` submit and
   the completion-wait output (Phase 01's live-session submit path; ensure the
   helper-sourced code flows through the same redaction), and `zeroize` the
   `String` after the code is relayed into the live session. Never log the code; never
   echo the child's stdout.
6. **Timeout coordination.** Pass a `--timeout-secs` to the helper that is well
   under the steamcmd in-VM wait windows but generous for a human (e.g. 120 s).
   Ensure a helper timeout (exit 11 → `Cancelled`) cleanly aborts the login with
   the typed `GuardCodeNeeded` outcome rather than leaving a half-open steamcmd
   session — kill the tmux session on abort.
7. Test: a unit test for the exit-code → outcome mapping (using a fake/stub
   "helper" script via `STEAMPIPE_GUARD_PROMPT_BIN` pointing at a shell script that
   echoes a code / exits 10 / exits 12); a test that the parallel path serializes
   (two concurrent `request` calls do not overlap — assert via a sentinel file or
   timing). Manual end-to-end on a graphical host: `cluster-ctl steam login <vm>`
   raises the window and completes.

## Acceptance criteria

- [ ] On a graphical host, `cluster-ctl steam login <vm>` against a session-less
      VM opens the `cluster-guard-prompt` window, accepts the code, and completes
      the login in one invocation (no `steam guard` follow-up).
- [ ] A unit test drives `HelperGui` against a stub binary (via
      `STEAMPIPE_GUARD_PROMPT_BIN`) and asserts: exit 0 + code → `Code`; exit 10 →
      `Cancelled`; exit 12 / missing binary → `Unavailable`.
- [ ] With multiple code-needing VMs in a `login all`, only one helper process
      runs at a time (serialized); code-not-needed VMs still complete in parallel —
      verified by a serialization test.
- [ ] If the helper binary is unresolved, the provider returns `Unavailable` and
      the flow falls back (stdin on a TTY, else typed fail-fast) — it never hangs.
- [ ] The helper-sourced code is redacted in all surfaced output and zeroized
      after it is relayed into the live session; `rg` shows no logging of child
      stdout.
- [ ] `cargo build`/`clippy`/`test` green.

## Files likely touched

- `src/game/guard.rs` (or the seam module) — `HelperGui` variant, exit-code
  mapping, helper resolution, prompt-serialization mutex (shared with Stdin).
- `src/game/steam.rs` — selection wiring into `automated_login`; zeroize after
  submit. *(Rebase on Phases 01/02; land before Phase 06's `steam.rs` edit.)*
- `Cargo.toml` — `zeroize` dep if not already present (cluster-ctl crate; this is
  not a GUI dep).
- Tests under `src/game/` or `tests/`.

## Pitfalls

- **Double window / deadlock under `par_each_vm`.** Forgetting the serialization
  mutex opens N windows; a poorly-scoped mutex held across the whole login (not
  just the prompt) serializes everything and kills parallelism. *Recovery:* hold
  the mutex only around `request`, not the steamcmd cascade.
- **Helper inherits a bad env.** If the child inherits a stale `$DISPLAY`, it
  exits 12 (clean) — good. But if you pass secrets via env, they leak to the
  process table. *Recovery:* pass only non-secret context via argv; never pass the
  code in (the helper produces it, not consumes it).
- **Leaving a steamcmd session open on cancel/timeout.** If the operator cancels,
  the no-code tmux session may still be waiting at the `Steam Guard code:` prompt.
  *Recovery:* on `Cancelled`/`Unavailable`, `tmux kill-session` and stop/reset the
  VM per the existing cleanup, then return `GuardCodeNeeded`.
- **Trailing newline / whitespace in the code.** stdout from the helper includes a
  newline; an untrimmed code fails steamcmd. *Recovery:* trim exactly one trailing
  `\n` (and reject internal whitespace).

## Reference

- Dossier: Option A recommendation; incompatibilities #1/#9; Open Decision #3.
- Helper protocol + exit codes: Phase 03 crate README.
- Provider seam + selection: Phases 01/02.
- tokio `Command` with piped stdout; `zeroize` for the code buffer.

## Execution Notes

2026-05-29:

- Shipped: `request_guard_code_from_helper` spawns the helper via
  `tokio::process::Command` (`stdin(null)`, `stdout(piped)`, `stderr(inherit)`),
  passing `--vm/--user/--reason/--timeout-secs` (+`--invalid-retry`); exit-code
  mapping and stdout trim/whitespace-reject in `guard_code_from_helper_stdout`;
  per-prompt serialization via `AsyncMutex` in `GuardProvider::{Stdin,HelperGui}`;
  resolution order env→sibling-of-exe→`$PATH` in `resolve_guard_prompt_binary`.
- **Dev fallback added.** `cargo run -- steam login …` builds only the
  `cluster-ctl` package, so `target/<profile>/cluster-guard-prompt` may be absent
  and the dialog silently wouldn't appear. Added `guard_prompt_command()` +
  `cargo_workspace_root_from_current_exe()`: when no helper binary is resolved but
  the running exe sits under a cargo `target/<profile>/` tree, the provider runs
  the helper via `cargo run --quiet --package cluster-guard-prompt --` from the
  workspace root (first call compiles it, then the window appears). In a stripped
  deploy with no helper and no cargo tree it returns `Unavailable` and falls back
  to stdin/fail-fast — never an accidental cargo invocation in production.
- Verified: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (475 unit + 13 integration) green; helper built beside
  `cluster-ctl`; headless helper exits 12 cleanly. The rendered window itself is
  testable directly with
  `cargo run -p cluster-guard-prompt -- --vm vm-1 --user <u> --reason "first login"`.
- **Graceful degradation added.** `HelperGui` now carries a `stdin_fallback` flag
  (the TTY state at selection time). When the dialog can't be shown
  (`Unavailable`: no usable display / helper missing / unexpected exit) it falls
  back to a terminal prompt instead of dead-ending in `GuardCodeNeeded`; a
  user-initiated **Cancel** is respected and does not fall through. Unexpected
  helper exits and spawn errors are now surfaced on stderr. This was prompted by a
  real failure where the dialog selected but couldn't open (Wayland lib not on the
  runtime path — see Phase 03) and the run dead-ended with no explanation.
- **Installed helper discovery done.** The default Nix `cluster-ctl` wrapper now
  prefixes `PATH` with the `cluster-guard-prompt` package output. Development
  runs from Cargo still prefer `cargo run --quiet --package cluster-guard-prompt`
  to avoid stale sibling helpers; installed runs resolve the wrapped helper from
  `PATH` and inherit its baked GUI runtime environment.
