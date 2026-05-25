# Plan: Steam warm timer

> **Recommended Codex model for plan-set orchestration: GPT 5.5 medium**
>
> Small follow-up to the `steam-session-longevity` plan that added `cluster-ctl steam warm`. The work is one substantive Nix change plus a docs pass; no design judgement beyond picking where the opt-in lives. Medium effort is enough — `high` is wasted on a two-phase plan whose dependency graph is linear.

## Scope

Add an opt-in systemd timer that runs `cluster-ctl steam warm` on a weekly cadence so refresh tokens rotate ahead of expiry without the operator having to remember a manual ritual. The previous plan shipped the command but left scheduling to the operator (open decision #3 in the design dossier); this plan closes that decision toward "opt-in NixOS timer."

The timer is opt-in because some operators may already orchestrate via external cron, GitHub Actions, or a separate scheduler — auto-enabling would double-fire.

## Current state summary

- `cluster-ctl steam warm [target] --runners-dir <DIR>` exists at [src/game/steam.rs:454](../../../../src/game/steam.rs#L454) and boots each VM, lets Steam come up, gracefully shuts down. Verified by `steam accounts` mtime advancing.
- The Nix wrapper `cluster-steam-warm` at [nix/lib/test-cluster.nix:441](../../../../nix/lib/test-cluster.nix#L441) bakes in the runners-dir for the default flavor.
- No systemd timer or service is shipped today. Operators who want scheduling must wire it themselves.
- `nix/module.nix` configures bridge + TAPs + credentials generation but knows nothing about runners-dir (that's a project-level Nix output, not a steampipe-module concern).

## Phase table

| Phase | File | Depends on | Touches | Can parallel with | Blocking? |
|---|---|---|---|---|---|
| 01 | [01-timer-module.md](./01-timer-module.md) | — | `nix/lib/test-cluster.nix`, optional new `nix/lib/warm-timer.nix` | — | unblocks 02 |
| 02 | [02-docs.md](./02-docs.md) | 01 | `docs/src/configuration/steam-credentials.md`, `docs/src/configuration/host-config.md` | — | closes plan |

## Parallelism layer

**Wave 0 — start immediately**

- **Phase 01** (timer module) — adds an opt-in NixOS module returned from `mkTestCluster` that wires a systemd timer and oneshot service around the existing `cluster-steam-warm` wrapper. The runners-dir is baked from the library scope, so the operator's host config only needs to flip a bool.

**Wave 1 — after 01 lands**

- **Phase 02** (docs) — documents the opt-in flag, default cadence, and how to tune the warn-within-days gate to monitor that the timer is doing its job.

Linear dependency. No file-level conflicts.

## Whole-set acceptance criteria

- `nix flake check` is clean.
- A project flake that opts in via the new flag picks up a `systemd.timers.steampipe-steam-warm` unit (or similarly named) with `OnCalendar=weekly` and a `Persistent=true` so missed runs catch up after a reboot.
- The service runs as `cfg.tapOwner` (so the SSH key it uses to reach VMs is the same key the operator already provisioned).
- Manually triggering the unit (`systemctl start steampipe-steam-warm.service`) advances `mtime` on every VM's refresh-token file.
- Docs name the opt-in flag and describe how to confirm the timer is working via `steam accounts`.

## Global constraints

- **Opt-in, not opt-out.** Defaulting the timer on would conflict with operators who run their own scheduler. The flag must default to `false`.
- **Runners-dir is library-scoped.** `nix/module.nix` (the cluster networking module) cannot know the runners-dir; it is a per-project Nix output. The timer must be wired at the `mkTestCluster` level where the runners-dir already exists.
- **Single-cluster only for now.** If the project has multiple `[cluster.flavors.*]`, the timer warms only the default flavor. Adding per-flavor timers is a future enhancement; this plan ships the simplest useful shape.
- **No retry-on-failure storm.** If `steam warm` fails for one VM, the timer reports failure but does not auto-retry within the same window. The next weekly run is the retry.

## Reference

- Prior plan (closed): the research and design dossiers at [steam-session-longevity-research.md](../steam-session-longevity-research.md) and [steam-session-longevity-design.md](../steam-session-longevity-design.md).
- Existing wrapper: [nix/lib/test-cluster.nix:441](../../../../nix/lib/test-cluster.nix#L441).
- Existing module: [nix/module.nix](../../../../nix/module.nix).
