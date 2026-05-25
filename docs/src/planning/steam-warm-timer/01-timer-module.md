# Phase 01 — Steam warm timer NixOS module

> **Recommended Codex model: GPT 5.5 medium**
>
> One substantive design call (where the timer module lives — library-scope vs project-scope) plus a NixOS unit definition. The runners-dir resolution is the non-trivial bit: `nix/module.nix` doesn't know it, so the timer must be wired at `mkTestCluster` time where it's already in scope. A `low` model would likely put the timer in `nix/module.nix` and discover the dependency inversion mid-implementation. `high` is unjustified — the unit shape is conventional systemd.

## Working tree

`/data/nvme0/can/Projects/steampipe`. No external repos. Downstream consumers (chessbender) will pick up the new opt-in via `nix flake update steampipe` and a `nixos-rebuild` cycle; no breaking change because the default is off.

## Goal

Add an opt-in NixOS module that wires a weekly `systemd.timer` + `systemd.service` around the existing `cluster-ctl steam warm` invocation. The module is returned from `mkTestCluster` (where `vmRunnersDir` is already in scope), and the project's host imports it alongside `steampipe.nixosModules.default`. Operators enable it with a single boolean flip; cadence, randomized delay, and SSH-key path are all configurable but have sane defaults.

Phase succeeds when `systemctl start steampipe-steam-warm.service` on a host that has imported the new module runs `cluster-ctl steam warm` end-to-end, advances `mtime` on every VM's refresh-token file, and `systemctl list-timers` shows a weekly entry with `Persistent=true`.

## Why this matters now

`steam-session-longevity` shipped the `steam warm` command but left scheduling to the operator. Without a default scheduling mechanism, the silent-decay failure mode the plan just fixed will quietly come back when an operator forgets to add their own cron entry. The NixOS module is the lightest-weight reliable scheduling primitive available — no daemon to write, no leader election, persistent across reboots, with native journal integration for debugging missed runs.

## Out of scope

- A persistent daemon. The work is one-shot every few days; a daemon adds nothing.
- Per-flavor timers. The plan ships scheduling for the default flavor only. Operators with multiple `[cluster.flavors.*]` can wire additional timers themselves following the same pattern.
- Notification on failure (email, Slack, etc.). The systemd journal is the signal; operators who want richer alerting wire it via `OnFailure=` in their own host config.
- Scheduling for `steam check` or other commands. Only `steam warm` is rotation-relevant.
- Cron / non-systemd alternatives. Operators not on systemd already have their own scheduler.

## Plan

1. **Decide where the module lives.** `mkTestCluster` already knows `vmRunnersDir`; `nix/module.nix` does not. The cleanest shape is a new `nix/lib/warm-timer.nix` that takes `{ runnersDir, vmCount, defaultUser, ... }` as arguments and returns a NixOS module. `mkTestCluster` calls it once and includes the result in its return as `nixosModule` (or `nixosModules.warm-timer`).

2. **Author `nix/lib/warm-timer.nix`.** Roughly:

   ```nix
   # nix/lib/warm-timer.nix
   {
     clusterCtl,
     vmRunnersDir,
     vmCount,
     defaultUser,
   }: {
     config,
     lib,
     pkgs,
     ...
   }: let
     cfg = config.services.steampipe-warm-timer;
   in {
     options.services.steampipe-warm-timer = {
       enable = lib.mkEnableOption "weekly cluster-ctl steam warm timer";

       user = lib.mkOption {
         type = lib.types.str;
         default = defaultUser;
         description = ''
           Host user that runs the warm command. Must own the SSH key
           configured in steampipe.toml (default: the tapOwner from the
           cluster module).
         '';
       };

       onCalendar = lib.mkOption {
         type = lib.types.str;
         default = "weekly";
         description = ''
           systemd OnCalendar= expression. Default `weekly` triggers
           once per week; tune to `daily` for high-paranoia, `monthly`
           for low-frequency clusters.
         '';
       };

       randomizedDelaySec = lib.mkOption {
         type = lib.types.str;
         default = "1h";
         description = "RandomizedDelaySec= to spread load across hosts.";
       };

       extraArgs = lib.mkOption {
         type = lib.types.listOf lib.types.str;
         default = [];
         description = ''
           Extra arguments appended to `cluster-ctl ... steam warm`.
           Useful for `--cluster <name>` when running in a non-default
           cluster context.
         '';
       };
     };

     config = lib.mkIf cfg.enable {
       systemd.timers.steampipe-steam-warm = {
         description = "Refresh Steam session tokens on cluster VMs";
         wantedBy = ["timers.target"];
         timerConfig = {
           OnCalendar = cfg.onCalendar;
           Persistent = true;
           RandomizedDelaySec = cfg.randomizedDelaySec;
         };
       };

       systemd.services.steampipe-steam-warm = {
         description = "cluster-ctl steam warm (one-shot)";
         # Wait until the bridge + NAT are up before trying to SSH into VMs.
         after = ["NetworkManager-wait-online.service" "network-online.target"];
         wants = ["NetworkManager-wait-online.service" "network-online.target"];
         serviceConfig = {
           Type = "oneshot";
           User = cfg.user;
           # cluster-ctl reads HOME for state dirs (XDG_STATE_HOME default).
           Environment = "HOME=${config.users.users.${cfg.user}.home or "/home/${cfg.user}"}";
           ExecStart =
             "${clusterCtl}/bin/cluster-ctl --vm-count ${toString vmCount} steam warm "
             + "--runners-dir ${vmRunnersDir} "
             + lib.concatStringsSep " " cfg.extraArgs;
         };
       };
     };
   }
   ```

3. **Wire it into `mkTestCluster` return.** At the bottom of `nix/lib/test-cluster.nix`, alongside `scripts`, add:

   ```nix
   warmTimerModule = import ./warm-timer.nix {
     inherit clusterCtl vmCount;
     vmRunnersDir = vmRunnersDir;
     defaultUser = vmUser;
   };
   ```

   So the project's flake imports it like:

   ```nix
   # In the consuming project's NixOS configuration.nix:
   { steampipeCluster, ... }: {
     imports = [
       steampipe.nixosModules.default
       steampipeCluster.warmTimerModule
     ];
     services.steampipe-warm-timer.enable = true;
   }
   ```

4. **Wire SSH-key access.** The service runs as `cfg.user` (defaults to the cluster `vmUser`/`tapOwner`). That user must already have the cluster_key SSH private key in `$HOME/.ssh/` or in the path declared in `steampipe.toml`. Add a sanity assertion:

   ```nix
   assertions = [{
     assertion = config.users.users ? ${cfg.user};
     message = "services.steampipe-warm-timer.user '${cfg.user}' must be a configured user; check that tapOwner is set.";
   }];
   ```

5. **Add `nix flake check` coverage.** If there is no current NixOS-module-eval check in `flake.nix`, the timer module is exercised by the existing chessbender consumer; verify locally with `nix eval`. For a self-contained check, add a `checks.${system}.warm-timer-eval` derivation that imports the module and forces it to a fixpoint:

   ```nix
   checks.${system}.warm-timer-eval = pkgs.runCommand "warm-timer-eval" {} ''
     ${pkgs.nix}/bin/nix-instantiate --eval --strict \
       -E 'import ${./nix/lib/warm-timer.nix} { ... }' >/dev/null
     touch $out
   '';
   ```

6. **Manual smoke test.**

   ```bash
   sudo nixos-rebuild test
   sudo systemctl list-timers | grep steampipe-steam-warm
   # Force-run the service:
   sudo systemctl start steampipe-steam-warm.service
   journalctl -u steampipe-steam-warm.service -b
   # mtime on every VM's local.vdf should now be within the last minute.
   ```

## Acceptance criteria

- [ ] `nix flake check` passes.
- [ ] A consumer that does `imports = [ steampipeCluster.warmTimerModule ]; services.steampipe-warm-timer.enable = true;` evaluates without warnings or assertions.
- [ ] `systemctl list-timers steampipe-steam-warm` shows a weekly entry with `Persistent=true` and the configured `RandomizedDelaySec`.
- [ ] `systemctl start steampipe-steam-warm.service` runs to success on a logged-in cluster and the journal contains `WARMED: vm-1 vm-2 ...`.
- [ ] After the manual run, `cluster-ctl steam accounts --warn-within-days 365` exits non-zero (proving the JWT expiry is < 365 days, i.e., the token is the expected ~200-day kind, not corrupted), and `RefreshAge` < 1 day.
- [ ] The module asserts that `cfg.user` is a configured user.
- [ ] `services.steampipe-warm-timer.enable` defaults to `false`.

## Files likely touched

| Path | Why |
|---|---|
| `nix/lib/warm-timer.nix` | New module file. Holds the options, timer, service, assertion. |
| `nix/lib/test-cluster.nix` | Import and re-export `warmTimerModule` from `mkTestCluster`. |
| `flake.nix` | (Optional) add a `checks.${system}.warm-timer-eval` derivation. |

## Pitfalls

- **`network-online.target` is not enough on its own.** NixOS uses `NetworkManager-wait-online.service` to gate `network-online.target`; including both in `after`/`wants` keeps the unit ordered correctly whether NetworkManager or systemd-networkd is in use.
- **User-environment quirks.** The systemd service inherits an almost-empty environment by default. `cluster-ctl` reads `HOME`, and the SSH key lookup may need `XDG_STATE_HOME` / `XDG_CONFIG_HOME` defaults. Explicitly set `HOME=` in `Environment=`; let the rest fall to systemd defaults — over-setting envs is the usual source of "works in shell, fails under systemd."
- **`steam warm` exits non-zero on any per-VM failure.** That's by design (it's how the journal surfaces real problems), but operators may misread a single-VM blip as a chronic timer problem. Document the expectation: a `failed` unit status means *at least one* VM had trouble, not necessarily that the timer pattern itself is broken.
- **Don't run as root.** SSH-key handling under root is awkward and the existing `cluster-ctl` flows assume the user owns the cluster_key. The default user is `tapOwner` for a reason.
- **VMs must already be configured.** The timer does not run `cluster-ctl net-up` or `cluster-ctl up`. Operators who tear down the cluster between test campaigns and rely on the timer to "wake it up" will be disappointed — the timer assumes the bridge is up and the VMs are reachable. Document this as a separate operator concern.
- **`Persistent=true` catches up missed runs.** Good in general, but means a host that has been off for two months will fire warm on next boot. Acceptable — the warming is cheap — but worth flagging in the docs.
- **`vmCount` is baked in.** The timer's `--vm-count` is whatever the project flake passed to `mkTestCluster` at the time the host config was built. If you change `vmCount` and rebuild, the timer picks up the new value. No runtime configurability here, which matches the rest of the cluster scripts.

## Reference

- The command being scheduled: [src/game/steam.rs:454](../../../../src/game/steam.rs#L454).
- The existing wrapper baking in `vmRunnersDir`: [nix/lib/test-cluster.nix:441](../../../../nix/lib/test-cluster.nix#L441).
- The host bridge module (where `tapOwner` is defined): [nix/module.nix:127-139](../../../../nix/module.nix#L127).
- Prior plan that established `steam warm`: closed `steam-session-longevity` Phase 05.
