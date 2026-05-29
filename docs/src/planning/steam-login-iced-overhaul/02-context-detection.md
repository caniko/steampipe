# Phase 02 — Host context detection + interactivity flags + Stdin provider

> **Recommended Codex model: GPT 5.5 medium**
>
> Moderate complexity: add CLI flags, probe the host environment
> (`$WAYLAND_DISPLAY`/`$DISPLAY`/TTY/MCP), and implement the provider-selection
> policy plus a TTY-gated `Stdin` provider against the seam Phase 01 already
> defined. The design space is bounded and the dossier prescribes the rule; the
> main judgement is getting the detection precedence right so a GUI never pops in
> CI and a headless caller never blocks. No frontier reasoning, so `medium`, not
> `high`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase 01** — the
`GuardProvider` seam, `GuardPromptContext`, and the shared login callable must
already exist. Rebase on the current `src/game/steam.rs` before editing (Phase 01
owns the prior edit).

## Goal

`cluster-ctl` knows, deterministically, which Guard-code channel is usable in the
current context and selects the provider accordingly: a real interactive terminal
with a host display → (later) the GUI helper, falling back to `Stdin` on a TTY
without a display; a `--code`/env value → `PreSupplied`; anything headless / MCP /
`--non-interactive` → `FailFast`. A new `LoginContext` value captures the probe
result, and the shared login callable uses it to pick the provider. This phase
adds the detection, the `--non-interactive`/`--no-gui` global flags, and the
TTY-gated `Stdin` provider; the GUI branch is stubbed to fall back to `FailFast`
until Phase 04 wires `HelperGui`. (Note: regardless of which provider is selected,
the code it returns is submitted into the live steamcmd session via `send-keys`
per Phase 01 — selection only decides *where the code comes from*. `PreSupplied`
via `--code`/env is meaningful only for the on-demand mobile authenticator; an
email Guard code cannot be pre-supplied because it is issued by the login attempt
itself, so those accounts use the dialog/`Stdin` path.)

## Why this matters now

Phase 01 wired the provider seam but defaulted the non-`--code` case to
`FailFast`. There is no display/TTY/non-interactive detection anywhere in the
login path today — the only heuristic is the brittle EAGAIN-on-stdin check at
`src/game/steam.rs:1876-1882`, which cannot tell "no display" from "no TTY", and
there is no global flag to force non-interactive mode (`src/ui/cli.rs:104-145`
has no such flag; `DisplayMode` at `cli.rs:66-97` is a *VM compositor* selector,
unrelated). iced **panics** (not `Result`) when no display exists (dossier,
external research), so a correct pre-check is a hard prerequisite for Phase 04 —
without it, `up`/`steam login` in CI or under the `warm` systemd timer would crash
or hang. This phase is the gate that makes the GUI safe to add.

## Out of scope

- Spawning the iced helper or implementing `HelperGui` — Phase 04. Stub the GUI
  branch to fall through to the `Stdin`/`FailFast` selection.
- The MCP/warm/check fail-fast wiring in *other* callers — Phase 06 (this phase
  provides the `LoginContext` + detection those callers will reuse).
- Removing `read_guard_code_from_stdin` — keep it; refactor it into the `Stdin`
  provider. Its standalone use by `guard()` stays until Phase 07.

## Plan

1. **Add a `LoginContext` probe.** Add a small module/function (e.g.
   `src/game/guard.rs::detect_context()` or `src/core/`), returning:
   ```rust
   struct LoginContext {
       display: bool,        // $WAYLAND_DISPLAY or $DISPLAY non-empty
       stdin_tty: bool,      // std::io::stdin().is_terminal()
       non_interactive: bool,// --non-interactive flag OR detected MCP/no-tty automation
   }
   ```
   Use `std::io::IsTerminal` (already used at `src/game/visual/bless.rs:68`) for
   the TTY check. `display` = `env::var_os("WAYLAND_DISPLAY")` or `DISPLAY`
   non-empty.
2. **Add global CLI flags** in `src/ui/cli.rs` (top-level `Cli`, near the other
   globals at `cli.rs:104-145`):
   - `--non-interactive` — force the headless/fail-fast contract (never prompt,
     never open a window).
   - `--no-gui` — allow interactive stdin but never open the GUI window.
   Thread both into the shared login callable / `LoginContext`.
3. **Define the provider-selection policy** (the dossier-locked rule), evaluated
   in the shared login callable:
   1. If `--code` or `STEAMPIPE_GUARD_CODE` is set → `PreSupplied`.
   2. Else if `non_interactive` (flag set, or MCP context — see Phase 06's marker,
      stubbed here as the flag) → `FailFast`.
   3. Else if `display && !--no-gui` → **GUI** (Phase 04 wires `HelperGui`; until
      then this branch falls through to step 4).
   4. Else if `stdin_tty` → `Stdin`.
   5. Else → `FailFast`.
4. **Implement the `Stdin` provider** by moving `read_guard_code_from_stdin`
   (`steam.rs:958-963`) behind the `GuardProvider::Stdin` variant. It must only be
   *selected* when `stdin_tty` is true (per the policy), and must use the existing
   nonblocking-aware read so a non-TTY stdin yields `Unavailable` rather than
   blocking (reuse `read_stdin_line`/`is_nonblocking_stdin_error`,
   `steam.rs:1866-1882`). On invalid-code retry it re-prompts.
5. **Thread the context** from the shared login callable (Phase 01,
   `src/lib.rs`) into `automated_login` so the provider is chosen once per
   invocation and passed down through `par_each_vm`.
6. **Parallel-path policy:** when fanning out over multiple VMs (`login all`,
   `auto_login`), the interactive providers (Stdin now, GUI later) must be
   serialized — only one prompt at a time. For this phase, since GUI isn't wired,
   ensure the `Stdin` provider is wrapped so concurrent VM tasks don't interleave
   prompts (a shared async mutex around `request`). Phase 04 reuses this queue for
   the GUI helper.
7. Run `cargo build`, `cargo clippy`, and add unit tests for `detect_context`
   precedence and the selection policy (table-driven: each `(code?, flags,
   display, tty)` tuple → expected provider variant).

## Acceptance criteria

- [ ] `cluster-ctl --help` shows `--non-interactive` and `--no-gui` globals with
      accurate help text.
- [ ] A unit test asserts the provider-selection policy for every combination:
      `--code` → `PreSupplied`; `--non-interactive` → `FailFast`; display + no
      flags → GUI-branch (stubbed → falls to Stdin/FailFast); no display + TTY →
      `Stdin`; no display + no TTY → `FailFast`.
- [ ] With `--non-interactive` (or no TTY and no display), a GuardRequired login
      returns the typed `GuardCodeNeeded` outcome and **never** blocks on stdin —
      verified by a test feeding a non-TTY stdin and asserting prompt return
      `Unavailable`.
- [ ] `read_guard_code_from_stdin` is reachable only through the `Stdin` provider
      variant; the parallel path serializes interactive prompts via a shared async
      mutex (a test or code comment documents single-prompt-at-a-time).
- [ ] `cargo build`/`cargo clippy` clean; `cargo test` green.

## Files likely touched

- `src/ui/cli.rs` — `--non-interactive`/`--no-gui` global flags; help text.
- `src/game/guard.rs` (or wherever Phase 01 put the seam) — `LoginContext`,
  `detect_context`, selection policy, `Stdin` provider, prompt-serialization
  mutex.
- `src/game/steam.rs` — fold `read_guard_code_from_stdin` into the `Stdin`
  provider; thread `LoginContext`/provider through `automated_login`. *(Rebase on
  Phase 01.)*
- `src/lib.rs` — pass detected context + flags into the shared login callable.

## Pitfalls

- **Detecting "MCP context" properly is Phase 06's job.** Here, treat
  `--non-interactive` as the force-headless signal and leave a clearly-marked hook
  for Phase 06 to also set `non_interactive` when serving MCP. Don't try to sniff
  MCP from `steam.rs`. *Recovery:* a `LoginContext::force_non_interactive()`
  constructor Phase 06 can call.
- **`$DISPLAY` set but unusable.** A stale `$DISPLAY` can be present with no
  reachable server; the env probe says "display" but iced would still fail. That
  is acceptable here because Phase 04 isolates iced in a child process whose
  non-zero exit is caught — but **do not** add a `--gui` that bypasses the probe.
  *Recovery:* the helper-process boundary (Phase 04) is the real safety net; the
  env probe is the cheap first gate.
- **Blocking stdin in a pipe.** If `Stdin` is selected when stdin is a pipe (not a
  TTY), `read_line` blocks forever. The policy must gate `Stdin` on `stdin_tty`;
  the nonblocking read is a second defense. *Symptom:* `up` hangs in CI.
  *Recovery:* assert `stdin_tty` before selecting `Stdin`.
- **Flag plumbing through `par_each_vm`.** Keep the selected provider `Clone +
  Send + Sync` (Phase 01 constraint) so the parallel path compiles.

## Reference

- Dossier: incompatibility #10 (no display/TTY/non-interactive detection),
  #3 (stdin), Open Decisions #3/#4 (parallel-vs-modal, `up` behavior).
- Existing signals: `IsTerminal` use at `src/game/visual/bless.rs:68`; EAGAIN
  heuristic at `src/game/steam.rs:1876-1882`; CLI globals at
  `src/ui/cli.rs:104-145`.
- Depends on Phase 01 (provider seam). Feeds Phase 04 (GUI branch) and Phase 06
  (MCP force-headless).
