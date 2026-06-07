# Plan: Steam login overhaul (iced host dialog + auto-steamcmd)

> **Recommended Codex model for plan-set orchestration: GPT 5.5 high**
>
> Coordinating this set requires holding the whole control-plane refactor in
> view at once: a typed login outcome, a provider seam, a new GUI helper
> binary, a state-detection convergence fix, and a retirement — all converging
> on `src/game/steam.rs`, which nearly every phase touches. The dependency graph
> has real serialization hazards (one shared file) and one verification gate
> (Phase 05 needs a real Steam account on the atlas host). A weaker orchestrator
> would under-serialize the `steam.rs` edits or green-light the retirement
> (Phase 07) before the convergence fix is proven, shipping an infinite
> auto-login loop. It is orchestration, not frontier novelty, so `high` rather
> than `max`.

## Scope and current state

Replace the two-phase Steam Guard flow (`login` bails on Guard → leaves the VM
running → operator runs a separate `steam guard --code` later) with a
single-invocation flow: detect a missing/expired session, run `steamcmd`
automatically, and when a one-time code is needed raise a **host-side** iced
dialog (pinentry-style), feed the code into the in-VM `steamcmd` in one shot, and
finish. Headless / AI / MCP contexts **fail fast** with a typed "guard code
needed" error — the GUI is operator-only and there is **no** MCP login tool pair.

Full evidence, incompatibility matrix, and the architecture rationale are in the
research dossier:
[steam-login-iced-overhaul-research.md](../steam-login-iced-overhaul-research.md).

### Decisions locked before planning (from the dossier's Open Decisions)

| Decision | Resolution |
|---|---|
| #1 steamcmd location & code transport | **Keep steamcmd in-VM**; keep the no-code `steamcmd +login <user> <pass>` bootstrap (this attempt is what makes Steam *issue* the code) and submit the obtained code into the **live** session via `tmux send-keys` — **inline, in one invocation** instead of via a separate `steam guard` process. A positional `+login user pass <code>` is **not** used: email Steam Guard mails the code in response to the attempt, so it cannot be known beforehand; positional only ever fits the on-demand mobile authenticator. |
| #2 AI/headless in scope? | **Fail fast** with a typed "guard code needed" error. GUI is operator-only. **No** `cluster_login`/`cluster_submit_guard` MCP tools. |
| #3 parallel vs single modal | One host dialog at a time: serialize Guard prompts via a host-side queue; never run two iced helpers concurrently. |
| #4 `up` silent auto_login raising a dialog | The dialog is raised whenever the context is interactive **and** a host display is present **and** not MCP/`--non-interactive`; otherwise fail fast. This rule is uniform across `up`, `steam login`, and `steam login all`. |
| #6 iced packaging | **Option A** — a separate `cluster-guard-prompt` helper binary owns iced/winit; `cluster-ctl` stays GUI-free. tiny-skia software renderer. |
| topology | iced renders **host-side**; the code is captured on the host and relayed into the VM's **live** steamcmd prompt via `tmux send-keys`. |

> **Code-delivery reality (drives the mechanism above).** Email Steam Guard — the
> always-available default — issues the one-time code *in response to the login
> attempt* (Steam mails it when steamcmd tries to log in from an unrecognized
> machine), so you cannot have a code to "pass in" beforehand. The mobile
> authenticator (sometimes available) is on-demand. Therefore the canonical flow
> is **attempt (no code) → Steam issues the code → operator enters it → relay into
> the live prompt → continue**. `--code`/`STEAMPIPE_GUARD_CODE` is a *convenience
> for the authenticator/automation case only*, fed through the same send-keys path
> after the attempt — never a positional argument that skips the attempt.

### Out of scope for the whole set

- Fully-unattended login via a stored TOTP `shared_secret` (dossier Open
  Decision #7) — adds a new secret to `VmCredentials`; deferred.
- Expanding auto-login into `warm`/`check`/the harness `ensure_steam` path
  beyond surfacing the typed fail-fast error (dossier Open Decision #8).
- Any MCP tool that initiates or completes a Guard login (decision #2).
- Moving `steamcmd` execution to the host (decision #1).

## Phase table

| Phase | File | Depends on | Touches (primary) | Can parallel with | Blocking? |
|---|---|---|---|---|---|
| 01 — Typed login outcome + shared callable + provider seam + inline Guard submit | [01-login-control-plane.md](./01-login-control-plane.md) | — | `src/game/steam.rs`, `src/lib.rs` | 03 | **Yes** (spine) |
| 02 — Host context detection + interactivity flags + Stdin provider | [02-context-detection.md](./02-context-detection.md) | 01 | `src/ui/cli.rs`, `src/game/steam.rs`, `src/lib.rs` | 03, 05 | Yes (gates 04/06) |
| 03 — `cluster-guard-prompt` iced helper binary + Nix packaging | [03-iced-helper-binary.md](./03-iced-helper-binary.md) | — | new crate/bin, `flake.nix`, `nix/flake/packages.nix` | 01, 02, 05 | Yes (gates 04) |
| 04 — Wire HelperGui provider (spawn helper) + concurrency queue | [04-helper-provider-wiring.md](./04-helper-provider-wiring.md) | 01, 02, 03 | `src/game/steam.rs` | 05 | No |
| 05 — State-sync / session-detection convergence fix | [05-state-sync-convergence.md](./05-state-sync-convergence.md) | 01 | `src/game/steam.rs`, `src/game/accounts.rs` | 02, 03, 04 | **Yes** (gates 07) |
| 06 — Headless/MCP fail-fast contract across callers | [06-headless-failfast-contract.md](./06-headless-failfast-contract.md) | 01, 02 | `src/mcp.rs`, `src/harness/runner.rs`, `src/game/steam.rs` | 04, 05 | No |
| 07 — Retire interactive VNC login + gate login-runner GUI packages | [07-retire-vnc-login.md](./07-retire-vnc-login.md) | 01–06 green | `src/game/steam.rs`, `src/ui/cli.rs`, `nix/cluster-vm-base.nix`, `nix/lib/test-cluster.nix` | — | No |
| 08 — Docs, CLI help, tests, plan retirement | [08-docs-and-tests/README.md](./08-docs-and-tests/README.md) | 01–07 | `docs/`, `README.md`, `tests/`, `src/ui/cli.rs` | — | No |

> **Serialization hazard — `src/game/steam.rs`.** Phases 01, 02, 04, 05, 06, 07
> all edit this file. They are sequenced so each lands on top of the prior; an
> agent starting a later phase must `git pull`/rebase and re-read the current
> `steam.rs` before editing. Where two phases touch *disjoint functions* in
> `steam.rs` (05 touches the sync/detector; 06 touches one guidance string),
> they may overlap with a rebase, but treat `steam.rs` as a mutex by default.

## Parallelism layer (execution waves)

- **Wave 0 (from current tree):** **Phase 01** (control-plane spine, `steam.rs` +
  `lib.rs`) and **Phase 03** (standalone helper binary + Nix, disjoint files) run
  fully in parallel. This is the main parallelism win — the GUI helper is built
  against a fixed argv/stdout protocol and needs nothing from the core refactor.
- **Wave 1 (after 01 accepted):** **Phase 02** (context detection + flags +
  Stdin provider) and **Phase 05** (state-sync convergence) start. Both rebase on
  01's `steam.rs`. 02 and 05 touch disjoint `steam.rs` functions and can overlap
  with a rebase; 03 may still be finishing in parallel.
- **Wave 2 (after 01+02+03 accepted):** **Phase 04** (wire HelperGui provider)
  and **Phase 06** (fail-fast contract). 04 depends on the helper binary (03) and
  the provider seam + detection (01, 02); 06 depends on 01+02. Both touch
  `steam.rs` — land 04 first, then 06 rebases (06's `steam.rs` change is a single
  guidance string at `steam.rs:444`).
- **Wave 3 (after 05 verified on a real host + 01–06 green):** **Phase 07**
  retires the VNC login path and gates the login-runner GUI packages. Gated on
  Phase 05's host verification proving steamcmd-only login establishes machine
  trust — do not start until that evidence exists.
- **Wave 4 (after 01–07 green):** **Phase 08** docs + tests. The two sub-layers
  (docs vs tests/CLI-help) are disjoint and fan out in parallel; the
  tests sub-layer needs the final code behavior, so it follows the code phases.
- **Plan exhausted** after Wave 4.

## Whole-set acceptance criteria

- [ ] `cluster-ctl steam login <vm>` on a session-less VM, in an interactive
      terminal with a host display, raises the `cluster-guard-prompt` window,
      accepts the code, and completes the login in **one** invocation (no second
      `steam guard` command needed).
- [ ] The same login under `--non-interactive`, with no `$DISPLAY`/
      `$WAYLAND_DISPLAY`, or via MCP, exits non-zero with a typed, machine-
      distinguishable "guard code needed" error and **never** opens a window or
      blocks on stdin.
- [ ] `cluster-ctl steam login <vm> --code <CODE>` (authenticator/automation case)
      completes the login in the **same** invocation — the no-code bootstrap
      triggers the prompt and the supplied code is relayed via `send-keys` into the
      live session — with no display present and no separate `steam guard` step.
- [ ] After a steamcmd-only login, `cluster-ctl steam accounts` reports the VM as
      `Ok` (the refresh-token JWT is found by `classify_session_status`) — i.e.
      auto-login converges and does not re-trigger on the next `up`.
- [ ] `cluster-ctl` builds and runs with **zero** GUI/`buildInputs` in its crane
      closure; only `cluster-guard-prompt` links iced/winit/tiny-skia.
- [ ] `interactive_login()` (VNC + host stdin REPL) is removed; `steam guard
      --code` and the `cluster-steam-guard` wrapper remain as documented
      headless fallbacks.
- [ ] `cargo build`, `cargo clippy` (workspace lints in `Cargo.toml`), and
      `cargo test` (incl. `tests/cli_steam_standalone.rs`) are green; the new
      typed-outcome and fail-fast paths have unit/integration coverage.
- [ ] `docs/src/configuration/steam-credentials.md` and README steam sections
      describe the new default flow and the headless contract; the
      shipped lifecycle behavior remains documented in stable docs.

## Global constraints (apply to every phase)

- **Secrets:** the Steam password and the Guard code are secrets. Every new
  channel (provider, helper pipe, event/result type, error messages, log tails)
  must route through `redact_sensitive` (`src/game/steam.rs:1919`) and the Guard
  code must be zeroized after use. Never log the code; the steamcmd LOG tail
  surfaced on failure can contain it.
- **Headless never panics or blocks:** iced panics (not `Result`) with no
  display; the helper-process boundary and an explicit display/TTY/MCP pre-check
  are the only safe gates. No code path may pop a window or block on stdin in a
  non-interactive context.
- **MCP stdout is sacred:** MCP transport is stdio JSON-RPC; never `println` to
  stdout from an MCP-reachable path (`src/mcp.rs` keeps `verbose: false`).
- **Don't regress shipped work:** parallel login + skip-bounce-when-reachable
  (Phase 02 of the prior plan), fill-the-gaps detection, and the per-VM virtiofsd
  lifecycle are done and durable — preserve them.
- **`vnc` crate stays:** it is the screenshot backend (`src/game/capture.rs`),
  not login infra. Retiring VNC *login* must not touch it.

## Reference

- Research dossier (evidence + incompatibility matrix + options):
  [steam-login-iced-overhaul-research.md](../steam-login-iced-overhaul-research.md).
- Stable docs for the shipped lifecycle behavior this builds on:
  [Steam Credentials](../../configuration/steam-credentials.md) and
  [Global host config](../../configuration/host-config.md).
- Run each phase in a fresh session pointed at its file. Prompt `verify` when
  done to audit acceptance criteria.
