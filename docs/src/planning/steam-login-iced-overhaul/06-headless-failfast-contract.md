# Phase 06 — Headless/MCP fail-fast contract across callers

> **Recommended Codex model: GPT 5.5 medium**
>
> Moderate complexity: propagate the typed `GuardCodeNeeded` outcome and the
> `LoginContext` force-headless signal across the MCP server, the harness
> `ensure_steam` path, the `warm` timer, and the `check()` guidance string, so
> every display-less caller fails fast deterministically instead of popping a
> window or blocking. Mostly wiring + messaging against types Phases 01/02 already
> defined; the judgement is ensuring no MCP path can ever reach the GUI/stdin
> branch. Bounded scope, so `medium`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phases 01 and 02** (typed
outcome + `LoginContext`/force-headless hook). Touches `src/mcp.rs`,
`src/harness/runner.rs`, and one guidance string in `src/game/steam.rs` — rebase
on the prior `steam.rs` edits (land after Phase 04's `steam.rs` change; 06's
`steam.rs` edit is a single string at `steam.rs:444`).

## Goal

Every display-less / automated caller — the MCP server, `nix_run`, the harness
Steam-test path, and the `warm` systemd timer — surfaces the typed "guard code
needed" error deterministically: non-zero/structured result, a clear message, and
**never** a window or a stdin block. The MCP context forces the headless provider
selection so no MCP-reachable code path can select GUI or Stdin. User-facing
guidance that referenced the old two-phase flow is corrected.

## Why this matters now

Decision #2 is "fail fast, GUI operator-only, no MCP login tool." The plumbing for
that contract lives across several callers the core refactor doesn't touch:

- The MCP server exposes no login/guard tool and `nix_run` pipes no stdin
  (`src/mcp.rs:289-761`, `mcp.rs:312-313`); an MCP-invoked `up`/login must fail
  fast, not hang. MCP stdout is JSON-RPC — a stray `println` corrupts it.
- The harness Steam path assumes a logged-in session and bails with a generic
  error, no session check (`src/harness/runner.rs:914-933`); reachable from MCP
  `cluster_test` (`mcp.rs:546`).
- The `warm` one-shot systemd service runs with no `$DISPLAY`
  (`nix/lib/warm-timer.nix:86-108`) — a second display-less caller.
- `check()` still prints "To fix not-logged-in: run nix run .#cluster-steam-login"
  (`src/game/steam.rs:444`), tied to the redesigned UX.

## Out of scope

- Adding any MCP tool that *initiates or completes* a Guard login (decision #2 —
  explicitly not building `cluster_login`/`cluster_submit_guard`).
- Adding auto-login *into* `warm`/`check`/the harness path (Open Decision #8
  deferred) — this phase only ensures they surface the typed error if/when a
  missing session is encountered, not that they log in.
- Docs rewrite — Phase 08 (this phase only fixes the inline `check()` string and
  any error-message text it emits).

## Plan

1. **Force headless under MCP.** Where the MCP server builds config / dispatches
   (`src/mcp.rs`), ensure any login-capable path it can reach constructs a
   `LoginContext` with `non_interactive = true` (use the Phase 02
   `force_non_interactive()` hook). Since decision #2 means MCP has **no** login
   tool, the practical surface is: anything `cluster_test`/`nix_run` triggers that
   could reach `automated_login` must never select GUI/Stdin. Add an explicit
   force-headless at the MCP entry so it's defense-in-depth even if a future tool
   is added.
2. **Harness Steam path** (`runner.rs:914-933`): when `ensure_steam` (or a session
   check you add) finds a missing/expired session, return the typed
   `GuardCodeNeeded`-equivalent error with an actionable message ("vm-N needs a
   Steam Guard code; run `cluster-ctl steam login vm-N` on an operator host with a
   display, or pass `--code`"), rather than the generic "Steam setup failed". Do
   **not** attempt to log in here (Open Decision #8 deferred). Keep it non-blocking
   and window-free.
3. **`--steam-target-id` conflation** (`runner.rs:982-983`): `local_steam_id()`
   reads the **host operator's** `~/.local/share/Steam` (`accounts.rs:205-212`),
   not a per-VM dir. Leave the behavior but add a code comment + ensure it doesn't
   silently couple to the per-VM session redesign; surface a clear error if the
   host operator has no Steam login when `--steam-target-id` is needed.
4. **`warm` timer:** confirm `warm()` (`steam.rs:456-518`) never reaches an
   interactive provider (it doesn't log in today). If Phase 01's refactor made any
   shared path reachable, ensure the timer context is `non_interactive`. Add a
   one-line note to the warm-timer service description or docs that an expired
   session there fails fast with the typed error (operator must re-login
   interactively).
5. **Fix the `check()` guidance string** (`steam.rs:444`) and any sibling
   messages to reflect the new single-invocation flow: "run `cluster-ctl steam
   login <vm>` (raises a host dialog for the Steam Guard code; on a headless host
   pass `--code` or set `STEAMPIPE_GUARD_CODE`)". Remove implications of the
   separate `steam guard` step from happy-path guidance (keep it only as the
   documented fallback in Phase 08).
6. **MCP message hygiene:** ensure all new error text routed through MCP goes via
   the existing `CallToolResult`/`strip_ansi` mechanism and never `println` to
   stdout (`mcp.rs` keeps `verbose:false`); the typed error becomes a returned
   `Content::text`, not a panic or a stdout write.
7. Test: a unit/integration test that an MCP-context login attempt with a
   simulated GuardRequired returns a structured "guard code needed" result and the
   provider selected is `FailFast` (assert no GUI/Stdin selection under
   `non_interactive`). Reuse Phase 02's selection-policy test harness.

## Acceptance criteria

- [ ] An MCP-reachable login path with a GuardRequired condition returns a
      structured, machine-distinguishable "guard code needed" result and selects
      the `FailFast` provider — verified by a test asserting provider selection
      under a forced-MCP/`non_interactive` context.
- [ ] No MCP-reachable code path can select `HelperGui` or `Stdin` (defense-in-
      depth force-headless at the MCP entry); no `println!` to stdout is added to
      any MCP path.
- [ ] The harness Steam path (`runner.rs`) emits an actionable "needs a Steam
      Guard code" message on a missing/expired session instead of the generic
      "Steam setup failed", without blocking or opening a window.
- [ ] `check()` guidance string at `src/game/steam.rs:444` reflects the new
      single-invocation flow (raises dialog / `--code` for headless); `rg "steam
      guard <vm> --code"` no longer appears in happy-path guidance.
- [ ] `--steam-target-id` surfaces a clear error when the host operator has no
      Steam login, with a comment documenting the host-vs-VM distinction.
- [ ] `cargo build`/`clippy`/`test` green.

## Files likely touched

- `src/mcp.rs` — force-headless context at entry; ensure error text returns via
  `CallToolResult`.
- `src/harness/runner.rs` — typed "guard needed" message in the Steam path;
  `--steam-target-id` guard + comment.
- `src/game/steam.rs` — `check()` guidance string at `steam.rs:444` (+ any
  sibling messages). *(Single-string edit; rebase after Phase 04.)*
- `nix/lib/warm-timer.nix` — optional description note (no behavior change).

## Pitfalls

- **Corrupting MCP stdout.** Any `println!`/`eprintln!`-to-stdout in an
  MCP-reachable path breaks JSON-RPC framing. *Recovery:* return text via
  `CallToolResult::success(vec![Content::text(...)])`; log only to stderr.
- **Accidentally enabling a login in `warm`/harness.** Open Decision #8 defers
  auto-login in those paths; only surface the typed error, don't call
  `automated_login` there. *Symptom:* a `warm` timer trying to log in headless and
  failing confusingly. *Recovery:* keep `warm`/harness read-only w.r.t. login.
- **Force-headless not actually forced.** If the MCP path constructs
  `LoginContext` via the normal env probe, a graphical operator host running the
  MCP server could detect a display and (wrongly) try the GUI. *Recovery:*
  unconditionally set `non_interactive` for MCP, independent of `$DISPLAY`.

## Reference

- Dossier: decision #2 (fail fast, no MCP tool); incompatibility #8 (no MCP
  login surface); critic uncovered call-sites (harness `ensure_steam`,
  `--steam-target-id`, warm timer); `check()` string at `steam.rs:444`.
- Depends on Phases 01 (typed outcome) + 02 (`LoginContext`/force-headless).
