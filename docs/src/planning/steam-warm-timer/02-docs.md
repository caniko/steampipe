# Phase 02 — Documentation for the warm timer

> **Recommended Codex model: GPT 5.5 low**
>
> Documentation update covering one new opt-in flag and a short operator-guidance paragraph. No design decisions, no source code. `low` matches.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Depends on Phase 01 — wait for the module to land before describing it.

## Goal

Document the new `services.steampipe-warm-timer.enable` option in `host-config.md`, add an "Automated rotation" subsection to `steam-credentials.md` showing the opt-in snippet, and note the relationship between the timer and the `RefreshAge` / `ExpiresIn` columns surfaced by `steam accounts`.

Phase succeeds when `mdbook build docs/` is clean and a reader following only the docs can opt in to the timer without consulting the phase doc.

## Why this matters now

Without docs, the opt-in is invisible. The previous plan's docs describe `cluster-ctl steam warm` as a manual command; this phase closes the loop by showing operators how to take their hands off the wheel.

## Out of scope

- Tutorial-style worked example. Reference docs stay reference docs.
- A new top-level docs section. The Configuration section already has both target pages.
- Documenting the spike findings. The design dossier already absorbed them.
- Notification / monitoring guidance. The systemd journal is the signal; operators who want richer alerting wire it themselves.

## Plan

1. **Extend [docs/src/configuration/steam-credentials.md](../../../../docs/src/configuration/steam-credentials.md).** After the existing `steam warm` section, add:

   ```markdown
   ### Automated rotation

   Add the warm-timer module to your host configuration to refresh
   tokens on a weekly schedule without manual invocation. The opt-in
   uses the runners-dir that was baked into `mkTestCluster`, so the
   only flag you need to flip is `enable`:

   ```nix
   { steampipeCluster, ... }: {
     imports = [ steampipeCluster.warmTimerModule ];
     services.steampipe-warm-timer.enable = true;
   }
   ```

   Defaults: weekly cadence, 1-hour randomized delay, runs as the
   cluster `tapOwner`. Override with `onCalendar`, `randomizedDelaySec`,
   and `user`. The full options list is in
   [Global host config](./host-config.md#steampipe-warm-timer).

   Verify the timer is doing its job by running
   `cluster-ctl steam accounts` and checking that `RefreshAge` stays
   under your `--warn-within-days` threshold. If it doesn't,
   `journalctl -u steampipe-steam-warm.service` will tell you why.
   ```

2. **Extend [docs/src/configuration/host-config.md](../../../../docs/src/configuration/host-config.md).** After the existing `loginStateDir` section, add a new `## steampipe-warm-timer` section that lists every option:

   ```markdown
   ## `steampipe-warm-timer`

   Opt-in NixOS module exposed from `mkTestCluster` as
   `warmTimerModule`. Wraps `cluster-ctl steam warm` in a systemd
   timer + oneshot service so refresh tokens rotate ahead of expiry.

   - `enable` (`bool`, default `false`): turn the timer on.
   - `user` (`str`, default = `tapOwner`): host user that runs the
     warm command; must own the cluster SSH key.
   - `onCalendar` (`str`, default `"weekly"`): systemd `OnCalendar=`
     expression. Tune to `"daily"` for high-paranoia clusters or
     `"monthly"` for clusters that test rarely.
   - `randomizedDelaySec` (`str`, default `"1h"`): jitter to spread
     load across hosts that run the same calendar entry.
   - `extraArgs` (`list of str`, default `[]`): extra arguments
     appended to `cluster-ctl ... steam warm`. Useful for
     `["--cluster" "nightly"]` when warming a non-default cluster
     flavor.

   ### Operational notes

   - The timer assumes the bridge is up and the cluster VMs are
     reachable. It does *not* run `net-up` or `up`; pair it with the
     existing NetworkManager-managed bridge from
     `services.steampipe-cluster.enable = true`.
   - `Persistent=true` is set on the timer, so a host that was off
     during the scheduled window catches up on next boot.
   - A `failed` unit status means at least one VM had trouble during
     warming, not necessarily that the timer pattern is broken.
     Read the journal before re-triggering.
   ```

3. **Verify the build.**

   ```bash
   mdbook build docs/
   # Confirm exit code 0 and no broken-link warnings.
   ```

## Acceptance criteria

- [ ] `mdbook build docs/` exits 0.
- [ ] `docs/src/configuration/steam-credentials.md` contains an `### Automated rotation` subsection.
- [ ] `docs/src/configuration/host-config.md` contains a `## steampipe-warm-timer` section listing all five options.
- [ ] All cross-links between the two pages resolve (mdbook would warn otherwise).

## Files likely touched

| Path | Why |
|---|---|
| `docs/src/configuration/steam-credentials.md` | New "Automated rotation" subsection under the existing `steam warm` content. |
| `docs/src/configuration/host-config.md` | New `## steampipe-warm-timer` reference section. |

## Pitfalls

- **The Nix snippet must compile in the consumer's flake.** Reviewers reading the docs should be able to copy-paste it into their `nixosConfigurations.<host>.modules` list. Test against the actual consumer (chessbender) before merging.
- **Don't duplicate the option descriptions.** The module's `description` fields are the source of truth; the docs should *summarize* and link, not maintain a parallel reference.
- **Avoid promising the unit name.** The systemd unit is named `steampipe-steam-warm.service`; that's what the docs reference. If Phase 01 renamed it, update the docs in lockstep.

## Reference

- Phase 01 (this plan): [01-timer-module.md](./01-timer-module.md).
- The command being documented: [docs/src/configuration/steam-credentials.md](../../../../docs/src/configuration/steam-credentials.md).
- The host-config reference page: [docs/src/configuration/host-config.md](../../../../docs/src/configuration/host-config.md).
