# Phase 04 — Wire mint into `steam login` (replace steamcmd+GUI engine)

> **Recommended Codex model: GPT 5.5 high**
>
> This swaps the login engine: `automated_login`'s steamcmd-bootstrap +
> GUI-validation cascade is replaced by mint→write, host-side, across the parallel
> `par_each_vm` path, preserving fill-the-gaps, the typed outcome, and the
> headless contract. Central control-flow surgery on the most-edited file with
> several callers and exit-code semantics → `high`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phases 01, 02, 03.** Edits
`src/game/steam.rs` (the login engine) and `src/lib.rs` (dispatch). Rebase on
current `steam.rs`.

## Goal

`cluster-ctl steam login vm-N` performs the headless mint end-to-end: detect a
missing/expired session → `mint_session` (Phase 02) via the selected
`GuardProvider` → `write_session` (Phase 03) into the VM's `loginStateDir` share →
report `Completed`; on no code channel, `GuardCodeNeeded`; on error, `Failed`. No
in-VM steamcmd bootstrap and no in-VM Steam GUI are started for login. Works for a
single VM and across the parallel multi-VM path, preserving fill-the-gaps and the
typed exit codes.

## Why this matters now

Phases 02/03 produced the engine and writer; this is where login actually switches
to them. After this phase, `steam login` succeeds headlessly and `accounts`
reports `Ok` — the user-visible goal.

## Out of scope

- Deleting the old steamcmd/GUI helper functions — Phase 05 (this phase routes
  around them; leave them compiling so the diff is reviewable).
- Game-run Steam startup, docs rewrite — Phase 05.

## Plan

1. **Replace the login body for the credentialed path.** In `automated_login`
   (and the single-VM `login_single_vm_automated`), replace the
   `steamcmd_bootstrap_command` → `wait_for_steamcmd_bootstrap` →
   `complete_guard_login_with_provider` → `finish_steam_login` cascade with:
   `mint_session(creds, vm.name, login_reason, provider)` → on `Minted`,
   `write_session(login_state_dir, vm.name, &session)` → `LoginOutcome::Completed`;
   on `GuardCodeNeeded`, return the typed outcome. The mint is **host-side**: it
   does not SSH into the VM; it writes directly to the host's
   `loginStateDir/vm-N/` share (resolve `login_state_dir` from the NixOS module /
   `--login-state-dir`, as `steam login` already does).
2. **Boot/reachability:** the host-side mint needs no running VM. Drop the
   VM boot/SSH-wait for the login path (or keep it only if `login_state_dir`
   resolution requires it — it should not). Removing the per-VM boot is a major
   simplification; confirm `loginStateDir` is host-writable without the VM up
   (it is — it's the virtiofs share dir on the host).
3. **Preserve fill-the-gaps:** keep the `classify_session_status` skip-if-`Ok`
   (unless `--force`) before minting. `--force` re-mints and overwrites.
4. **Parallel path:** keep `par_each_vm` for `steam login all`; the provider's
   prompt serialization (existing async mutex) already ensures one dialog at a
   time across concurrent mints.
5. **Typed outcome + exit codes:** map `Completed`/`GuardCodeNeeded`/`Failed` as
   today (distinct exit for guard-needed). Update `auto_login` (the `up` path) to
   use the mint engine too (silent provider → fail-fast on guard, as now).
6. **Tests:** unit-test the orchestration with a stub mint (success →
   write+Completed; GuardCodeNeeded passthrough) using a temp `loginStateDir`;
   assert the artifacts land and the outcome maps correctly.

## Acceptance criteria

- [ ] `cluster-ctl steam login vm-1` (interactive, with dialog or `--code`) mints
      + writes the session to `/var/lib/steampipe/logins/vm-1/` and reports success
      in one invocation, **without** starting any in-VM steamcmd or Steam GUI
      (verify: no `steampipe-steamcmd-login` tmux session created, no
      `steam`/`steamwebhelper` processes spawned on the VM).
- [ ] `cluster-ctl steam accounts` then reports `vm-1 … Ok`; a follow-up
      `steam login` skips it (fill-the-gaps) and `--force` re-mints.
- [ ] Headless/`--non-interactive`/MCP returns the typed `GuardCodeNeeded`
      fail-fast (distinct exit), no hang/window; `--code` works unattended.
- [ ] `steam login all` mints multiple VMs with the parallel path, serializing the
      dialog to one prompt at a time.
- [ ] `cargo build`/`clippy`/`test` green.

## Files likely touched
- `src/game/steam.rs` — `automated_login`, `login_single_vm_automated`,
  `login_single_vm`, `login_vm_attempt`, `auto_login` rewired to mint+write;
  `LoginOutcome` mapping. *(Owns this wave's `steam.rs` edit.)*
- `src/lib.rs` — login dispatch unchanged in shape but confirm `login_state_dir`
  resolution is passed through; exit-code mapping intact.

## Pitfalls
- **Don't SSH/boot the VM for login anymore.** The old path booted the VM and ran
  steamcmd in it; the mint is host-side. Leaving the boot in wastes time and
  reintroduces VM dependency. *Recovery:* remove the boot/`wait_ready` from the
  login path; keep it only for commands that genuinely need a running VM.
- **`login_state_dir` may be `None`** (no module / unreadable) — the mint needs a
  target dir. Fail with a clear error (can't write the session) rather than
  silently succeeding. *Recovery:* require a resolvable `loginStateDir` for the
  mint path.
- **Leave old helpers compiling.** Removing `finish_steam_login`/steamcmd helpers
  here would balloon the diff and risk Phase 05's retirement; just stop calling
  them. *Recovery:* dead-code `#[allow]` if clippy complains, to be removed in 05.
- **Outcome/exit regressions.** Keep the distinct `GuardCodeNeeded` exit so MCP/CI
  still distinguish "needs code" from "broke".

## Reference
- Engine + writer: [02-mint-engine.md](./02-mint-engine.md), [03-artifact-writer.md](./03-artifact-writer.md).
- Current login flow being replaced: `src/game/steam.rs` (`automated_login`,
  `finish_steam_login`, `complete_guard_login_with_provider`); dispatch
  `src/lib.rs` `run_steam_login_command`.
- Followed by Phase 05 (retire old internals + live verify).
