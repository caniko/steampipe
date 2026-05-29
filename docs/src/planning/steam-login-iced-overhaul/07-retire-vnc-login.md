# Phase 07 — Retire interactive VNC login + gate login-runner GUI packages

> **Recommended Codex model: GPT 5.5 high**
>
> Complex, semi-irreversible deletion spanning Rust and Nix, gated on a real-host
> assumption: that steamcmd-only login (Phase 01) plus the convergence fix (Phase
> 05) fully replaces the in-guest VNC login, including first-machine-trust
> establishment. If that assumption is wrong, removing `interactive_login()` and
> the in-VM GUI packages strands accounts that can only be logged in via the
> guest GUI. Careful sequencing and a verification gate matter more than raw
> output, so `high`. Not `max` — it is bounded and reversible via git, and Phase
> 05 de-risks the core assumption.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phases 01–06 being green AND
Phase 05's host verification proving steamcmd-only login establishes machine
trust** (first login on a fresh VM succeeds via steamcmd + a Guard code, with no
in-guest GUI step). Do not start until that evidence exists. Touches
`src/game/steam.rs`, `src/ui/cli.rs`, `nix/cluster-vm-base.nix`,
`nix/lib/test-cluster.nix`.

## Goal

The in-guest VNC login path is removed and the login-runner VM image no longer
provisions GUI-login-only packages by default, while the documented headless
fallbacks (`steam guard --code`, the `cluster-steam-guard` Nix wrapper) remain.
`cluster-ctl steam login` is uniformly the automated steamcmd + host-dialog flow;
there is no creds-less interactive branch. The `vnc` crate, `sway`/`grim`, and the
screenshot/game-run paths are untouched.

## Why this matters now

`interactive_login()` (`src/game/steam.rs:1787-1864`) surfaces Steam Guard
*inside the guest GUI* over VNC — exactly the model the operator rejected ("iced
on our side, not on the VM"). It is the sole reason `login` keeps a creds-less
serial branch (`steam.rs:1113-1119`), it dead-ends to the removed `LeftRunning`
outcome, and it pulls a chain of VM-image dependencies (wayvnc, wl-clipboard,
wtype, the in-VM Steam GUI, 4 GiB login-runner sizing) that exist only for
GUI-login. Once Phases 01–06 make the host-dialog flow the default and Phase 05
proves convergence, this path is dead weight (dossier incompatibilities #4, #12).
Retiring it removes the serial-login constraint and shrinks the login-runner
image.

## Out of scope

- Removing `guard()` / `steam guard --code` / the `cluster-steam-guard` wrappers —
  these stay as the headless escape hatch (decision: keep-as-fallback).
- Touching the `vnc` crate, `src/game/capture.rs`, `sway`/`grim`, or the
  screenshot/game-run compositor paths.
- Fully unattended TOTP login (deferred).

## Plan

1. **Gate, don't guess — start with a verification step.** Confirm in this
   phase's notes that Phase 05's host run established first-machine-trust via
   steamcmd + a Guard code with no in-guest GUI. If that evidence is absent, stop
   and keep `interactive_login()` as an explicitly-opt-in fallback behind a
   `--vnc-fallback` flag instead of removing it. Proceed with removal only if the
   evidence exists.
2. **Remove `interactive_login()`** and its now-unused helpers: the function
   (`steam.rs:1787-1864`), the host stdin REPL, `wtype` paste, and
   `read_stdin_line`/`is_nonblocking_stdin_error` if no longer used (the `Stdin`
   provider from Phase 02 may still use a read helper — keep what it needs).
3. **Remove the creds-less serial branch.** In `login_single_vm`
   (`steam.rs:1053-1119`) and the serial loop in `login()`
   (`steam.rs:691-731`), drop the `creds.is_none()` → interactive path. Login now
   requires credentials (or `--code`/dialog); update the CLI help for
   `SteamAction::Login` (`cli.rs:614-617`) which currently says "Interactive Steam
   login wizard (one VM at a time with VNC)". Make `login` uniformly use the
   parallel/automated path.
4. **Gate the in-VM GUI-login packages in Nix.** In `nix/cluster-vm-base.nix`
   (wayvnc/wl-clipboard/wtype at `:91-129`, `programs.steam.enable` at `:76`),
   gate the GUI-login-only packages (wayvnc-for-login, wl-clipboard, wtype, the
   in-VM Steam GUI) off the default login path. Keep anything the
   screenshot/game-run paths need (sway, grim, and wayvnc *for screenshots* via
   `capture.rs`). If wayvnc is shared between login and screenshots, keep it but
   remove the login-specific usage; do not break `ScreenshotBackend::Vnc`.
5. **Login-runner teardown.** The per-login `pkill wayvnc/sway` lines
   (`steam.rs:949`, `1042`, `1141`) become no-ops for steamcmd-only login; keep
   `pkill tmux` (steamcmd runs in tmux). Simplify the teardown to what the
   steamcmd flow needs.
6. **Optionally downsize the login runner.** The 4 GiB login-runner sizing
   (`nix/lib/test-cluster.nix:195`, `nix/module.nix:295`, justified by "Steam GUI
   needs ~4 GiB") may be reducible now that login is steamcmd-only. Treat as
   optional: measure steamcmd-only login memory on the host first; only lower the
   size with evidence. If unmeasured, leave sizing as-is and note it.
7. **Keep fallbacks intact and documented.** Verify `steam guard --code`,
   `cluster-steam-guard`, `cluster-1v1-steam-guard`, and
   `cluster-steam-login-persist` (`test-cluster.nix:390-427`) still build and
   function. (Doc rewrite is Phase 08; here just don't break them.)
8. Run `cargo build`/`clippy`/`test`; `nix flake check` / build the affected
   runner outputs; confirm `ScreenshotBackend::Vnc` still works.

## Acceptance criteria

- [ ] First-machine-trust evidence from Phase 05 is cited in this phase's notes;
      if absent, `interactive_login()` is retained behind `--vnc-fallback` rather
      than removed (and the rest of the acceptance criteria adjust accordingly).
- [ ] `interactive_login()` and the creds-less serial login branch are removed;
      `rg "interactive_login|VNC into|wtype" src` returns nothing in the login
      path (screenshot/capture `vnc` usage remains).
- [ ] `SteamAction::Login` help no longer says "with VNC"; `cluster-ctl steam
      login` requires creds or `--code`/dialog and uses the automated path.
- [ ] `nix/cluster-vm-base.nix` no longer installs wayvnc/wl-clipboard/wtype/the
      in-VM Steam GUI for the default login path; `ScreenshotBackend::Vnc` and the
      game-run compositor paths still build and function.
- [ ] `steam guard --code`, `cluster-steam-guard`, and `cluster-steam-login-persist`
      still build and run (fallbacks preserved).
- [ ] `cargo build`/`clippy`/`test` green; affected `nix` runner outputs build.

## Files likely touched

- `src/game/steam.rs` — remove `interactive_login`, creds-less branch, dead stdin
  helpers; simplify teardown. *(Land last among `steam.rs` editors.)*
- `src/ui/cli.rs` — `SteamAction::Login` help text; remove VNC framing.
- `nix/cluster-vm-base.nix` — gate GUI-login packages off the default login path.
- `nix/lib/test-cluster.nix`, `nix/module.nix` — optional login-runner downsizing
  (evidence-gated).

## Risk profile

- Removing `interactive_login()` strands any account that can only be logged in
  via the in-guest GUI (e.g. a first-trust flow steamcmd can't satisfy).
- Over-aggressive Nix gating breaks `ScreenshotBackend::Vnc` (shared wayvnc) or
  the game-run compositor.
- Downsizing the login runner below steamcmd's real footprint causes OOM/boot
  failures that look like unrelated SSH timeouts.

## Strategy

Commit ladder, each independently revertable:
1. Rust: remove `interactive_login` + creds-less branch + dead helpers (revert =
   `git revert`; low cost).
2. Nix: gate GUI-login packages (revert restores packages; medium cost — requires
   runner rebuild).
3. (Optional) Nix: downsize login runner (separate commit so it can be reverted
   without touching the package gating).

## Rollback drill

Before the Nix gating commit, confirm you can rebuild the prior runner:
`nix build .#<login-runner-output>` on the current tree, note the store path, and
verify `git revert <nix-commit> && nix build .#<login-runner-output>` reproduces a
working runner within ~10 min. Practice once before committing step 2.

## Failure modes and recoveries

- **F1 — `ScreenshotBackend::Vnc` broken after Nix gating.** Symptom: screenshot
  capture fails with wayvnc missing. Cause: removed wayvnc wholesale. Recovery:
  restore wayvnc in the image; only remove the *login* usage, not the package.
- **F2 — First login fails on a fresh VM (no machine trust).** Symptom: steamcmd
  login keeps requesting a code / refuses without GUI confirmation. Cause: account
  needs in-guest GUI first-trust. Recovery: re-introduce `interactive_login`
  behind `--vnc-fallback`; document it as the deep fallback (this is why step 1 is
  a hard gate).
- **F3 — Login runner OOMs after downsizing.** Symptom: SSH timeout, vm.log shows
  OOM. Cause: steamcmd footprint underestimated. Recovery: revert the sizing
  commit (kept separate for this reason).

## Reference

- Dossier: incompatibilities #4 (interactive VNC) and #12 (in-VM GUI packages);
  Open Decision #5 (retire-vs-fallback); first-trust open question.
- Code: `interactive_login` `src/game/steam.rs:1787-1864`; serial branch
  `steam.rs:1113-1119`; teardown `steam.rs:949/1042/1141`; Nix
  `nix/cluster-vm-base.nix:76-129`, sizing `nix/lib/test-cluster.nix:195`.
- Gated by Phase 05 host verification; precedes Phase 08 docs.
