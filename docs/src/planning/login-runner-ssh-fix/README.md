# Plan: Login Runner SSH Unreachable Fix

> **Recommended Codex model for orchestration: GPT 5.5 medium**
>
> The plan-set orchestration only needs to track five short phases with a
> single sequencing edge (P3 depends on P2). No frontier reasoning, no
> cross-org coordination — `medium` is the right tier for the parent
> session that fans these out.

## Scope and current state

`cluster-ctl steam login all` on atlas (and on any flake-evaluating host)
times out because login-runner VMs are built with an **empty
`authorized_keys`** for the `cluster` user. The module's default
`sshAuthorizedKey` reader uses `builtins.pathExists` on
`/var/lib/steampipe/ssh/cluster_key.pub`, which always returns `false`
under flake `--pure-eval`. Verified at
[docs/src/planning/login-runner-ssh-unreachable-findings.md](../login-runner-ssh-unreachable-findings.md);
the dossier has the full evidence trail.

The active atlas host has been rebuilt six times since the key was
generated (gens 341→346), every rebuild silently baked an empty key.

## Phases

| Phase | File | Depends on | Touches | Tier | Blocking? |
|-------|------|------------|---------|------|-----------|
| 01 | [01-canix-explicit-key-unblock.md](./01-canix-explicit-key-unblock.md) | — | canix `root/hosts/atlas/features/steampipe.nix` | 5.5 low | unblocks atlas immediately |
| 02 | [02-drop-pathexists-default-and-assert.md](./02-drop-pathexists-default-and-assert.md) | — | steampipe `nix/module.nix`, `docs/src/getting-started/installation.md` | 5.5 medium | fixes the bug for everyone |
| 03 | [03-flake-check-empty-key-regression.md](./03-flake-check-empty-key-regression.md) | 02 | steampipe `flake.nix` | 5.5 low | depends on P2 |
| 04 | [04-vm-log-tail-unavailable-fix.md](./04-vm-log-tail-unavailable-fix.md) | — | steampipe `src/game/steam.rs`, `src/core/backend.rs` | 5.5 medium | independent |
| 05 | [05-cluster-key-hostname-cosmetic.md](./05-cluster-key-hostname-cosmetic.md) | — | steampipe `nix/module.nix` (activation script) | 5.5 low | independent |

## Dependency table

| Phase | Depends on | Touches files | Can parallel with |
|-------|------------|---------------|-------------------|
| 01 | — | canix `features/steampipe.nix` | 02, 03, 04, 05 |
| 02 | — | steampipe `nix/module.nix`, `docs/...installation.md` | 01, 04, 05 |
| 03 | 02 | steampipe `flake.nix` | 04, 05 (after P2 lands) |
| 04 | — | steampipe `src/game/steam.rs`, `src/core/backend.rs` | 01, 02, 03, 05 |
| 05 | — | steampipe `nix/module.nix` (activation script) | 01, 03, 04 |

P2 and P5 both edit `nix/module.nix` but in non-overlapping regions
(P2: option default + assertions; P5: activation script body). Run P2
first or rebase P5 after P2 lands.

## Parallelism layer

**Wave 0 (start immediately, no prerequisites):**

- **P1** — drop the immediate-unblock commit into canix. Trivial, can be
  done in parallel with everything else. Once it merges and atlas is
  rebuilt, the user is unblocked on `cluster-ctl steam login` regardless
  of how long P2-P5 take.
- **P2** — module-level fix in steampipe. Independent of P1.
- **P4** — `vm.log` tail bug. Independent Rust work, no overlap with the
  Nix changes.
- **P5** — cosmetic hostname fix. Touches `nix/module.nix` but a
  different region than P2; rebase if P2 lands first.

**Wave 1 (after P2):**

- **P3** — flake check that fails when `loginRunners.enable = true` and
  the assertion path isn't covered. Needs the new assertion behavior to
  exist before the check can be authored.

Plan exhausts at the end of Wave 1.

## Whole-set acceptance criteria

- [ ] `cluster-ctl steam login vm-1` on atlas succeeds (or at minimum
      reaches the Steam login step) without paste-the-key workarounds.
- [ ] A fresh install following `docs/src/getting-started/installation.md`
      reaches "first login works" without falling into the
      pure-eval-silently-empty trap.
- [ ] `nix flake check` covers the loginRunners-enable-without-key case.
- [ ] `print_vm_log_tail` no longer prints "VM log unavailable" when the
      file actually exists at the same path it just claimed.
- [ ] The generated `cluster_key.pub` comment has a non-empty hostname
      suffix.

## Global constraints

- **Branch hygiene**: all phases land on `trunk`. P1 is in `canix`
  (`/data/nvme0/can/Projects/canix`); P2-P5 are in `steampipe`
  (`/data/nvme0/can/Projects/steampipe`).
- **No virtiofs revival**: commit 4ee68f0 documents why the previous
  virtiofs-share approach was broken. Do not reintroduce
  `microvm-virtiofsd@<vm>-<tag>.service`-dependent shares as a fix for
  this bug.
- **Honest UX**: the module already promises "rebuild once and it
  works" in `installation.md`. P2 either keeps that promise via a
  different mechanism, or it removes the promise and tells operators
  exactly what to do instead.

## Reference

- Originating findings:
  [docs/src/planning/login-runner-ssh-unreachable-findings.md](../login-runner-ssh-unreachable-findings.md)
- Background commit: 4ee68f0 "Drop runtime ssh-authorized virtiofs share
  from login runners" — the commit that introduced the broken
  `pathExists` default.
- Background commit: 2e765f4 "Finish the NixOS-module install path" —
  current trunk head; documents the install path the plan must keep
  honest.
