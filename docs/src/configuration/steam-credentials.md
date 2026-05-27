# Steam Credentials

> [Global host config](./host-config.md) covers the broader discovery model.
> If you only need the credentials default, set `credentialsPath` there and
> `cluster-ctl` will use it as the `--credentials` fallback.

## Credentials file format

```toml
[vm.vm-1]
steam_user = "testaccount1"
steam_pass = "password1"
game_key = "XXXXX-XXXXX-XXXXX"   # optional

[vm.vm-2]
steam_user = "testaccount2"
steam_pass = "password2"
```

The file can also be age-encrypted (`.age` extension) - `cluster-ctl` will
decrypt with `rage`/`age` via ssh-agent automatically.

## Interactive login (VNC)

```bash
cluster-ctl steam login
```

By default, `steam login` skips VMs whose persisted host-side session is
already `OK`. VMs with `STALE`, `EXPIRED`, or `NO_TOKEN` status are logged in;
pass `--force` to re-login every selected VM regardless.

For each VM selected for login:

1. Boots the VM using a 4GB-RAM login image.
2. Mounts that VM's persistent Steam state share from
   `loginStateDir/vm-N` at `/home/${vm_user}/.local/share/Steam`.
3. Starts `sway` (Wayland compositor) + `wayvnc` on port 5900.
4. Launches Steam's GUI.
5. You VNC in (`vncviewer 10.0.100.N:5900`) and complete login + Steam Guard.

Interactive controls during login:

- `p` - paste text into the VM via `wtype`
- `Enter` - mark as done, gracefully stop Steam, then shut down the VM
- `Ctrl+C` - cancel

Target specific VMs:

```bash
# Login only vm-3
cluster-ctl steam login vm-3

# Login vm-3 through vm-7
cluster-ctl steam login vm-3 -+
```

Steam writes its normal Linux client state directly into the writable virtiofs
share. That includes `config/loginusers.vdf`, refresh tokens under
`config/<steamID>/local.vdf` or `config/config.vdf`, logs, userdata, and
`ssfn*` machine-trust files. Because the share is the VM's real
`~/.local/share/Steam`, those writes survive `cluster-ctl down` / `up` cycles
instead of being copied out or regenerated at boot.

## Automated login

```bash
cluster-ctl --credentials secrets/steam-creds.toml steam login
```

Runs `steam -login <user> <pass>` on each VM selected for login and waits for
confirmation. Also activates game keys if provided.

By default, automated `steam login` also fills the gaps only: sessions already
reported as `OK` are skipped, while `STALE`, `EXPIRED`, and `NO_TOKEN` sessions
are refreshed through the login flow. Pass `--force` to re-login every selected
VM.

Use interactive login for a VM's first trust-establishing login when Steam
Guard requires confirmation. After that, the persistent share preserves the
client's machine-trust and refresh-token state across boots, so subsequent
automated logins and health checks do not start from a new machine identity.

Automated `steam login` runs every selected VM in parallel under the
host-memory admission gate, and skips the VM bounce when a target is
already up and reachable over SSH. `--force` re-logs every selected VM in
place — it does **not** force a fresh boot. Interactive login (no
credentials TOML) stays serial: VNC and prompt streams are one-at-a-time
by construction.

## Login VM lifecycle

Login-class runners declare a writable virtiofs share for
`/var/lib/steampipe/logins/vm-N` so Steam's client state lives on the host
and survives `down`/`up`. `cluster-ctl` spawns the per-VM virtiofsd helper
(`bin/virtiofsd-run` inside the runner farm) before booting the QEMU VM,
waits for each declared `*-virtiofs-*.sock` to appear under the VM's state
dir, and only then exec's `bin/microvm-run`. `cluster-ctl down` kills both
children in order (virtiofsd first, then the VM) and removes the
`<vm>.virtiofsd.pid` slot alongside the existing `<vm>.pid`.

The operator user (`services.steampipe-cluster.tapOwner`) must be in the
`kvm` group so virtiofsd's `--socket-group=kvm` chgrp call succeeds. The
NixOS module emits a `nixos-rebuild` warning if it isn't. If the warning
fires, add the user to `kvm`:

```nix
users.users."${tapOwner}".extraGroups = [ "kvm" ];
```

See
[planning/headless-steam-login-research.md](../planning/headless-steam-login-research.md)
for the root-cause analysis that motivated this lifecycle.

## Checking Steam health

```bash
cluster-ctl --vm-count 7 steam check --runners-dir ./result
```

Boots each VM one at a time and verifies:

- `loginusers.vdf` exists with a `PersonaName`.
- Steam starts and reaches the logged-on state.
- The persisted state is usable without re-running `steam login`.

After a successful login, `cluster-ctl steam check` should continue to pass
after a `cluster-ctl down` followed by a fresh `cluster-ctl up`, because the
Steam state is persisted in `loginStateDir/vm-N`.

## Warming sessions

```bash
cluster-ctl --vm-count 7 steam warm --runners-dir ./result
```

`steam warm [target]` boots the selected VM or VMs, waits until Steam is logged
on, then gracefully stops Steam and the VM. This gives Steam a chance to rotate
its refresh token and write the new token back into the persistent share.

Run this on a steady cadence for idle clusters. A weekly cron or equivalent
timer is the suggested default; it is frequent enough to keep the token chain
fresh without turning warm-up into a daily operational dependency.

Targeting follows the same VM syntax as login:

```bash
cluster-ctl --vm-count 7 steam warm vm-3 --runners-dir ./result
cluster-ctl --vm-count 7 steam warm -+ vm-3 --runners-dir ./result
```

### Automated rotation

Add the warm-timer module to your host configuration to refresh tokens on a
weekly schedule without manual invocation. Host-level warm timers use the
runner directory published by `services.steampipe-cluster.runners.enable`, so
import the timer module next to the host module and enable runtime runners:

```nix
{
  imports = [
    inputs.steampipe.nixosModules.default
    inputs.steampipe.nixosModules.warmTimer
  ];

  services.steampipe-cluster = {
    enable = true;
    tapOwner = "yourhostuser";
    tapOwnerUid = 1000;
    runners.enable = true;
  };

  services.steampipe-warm-timer.enable = true;
}
```

Defaults: weekly cadence, 1-hour randomized delay, the module `tapOwner`, and
`/var/lib/steampipe/ssh/cluster_key`. Override with `onCalendar`,
`randomizedDelaySec`, `user`, `sshKey`, and `extraArgs`. The full options list
is in [Global host config](./host-config.md#steampipe-warm-timer).

Project-flake consumers can still use `(mkTestCluster {...}).warmTimerModule`
when the timer should be bound to that project-built runner set instead of the
host-level `/etc/steampipe/runners` configuration.

Verify the timer is doing its job by running `cluster-ctl steam accounts` and
checking that `RefreshAge` stays under your `--warn-within-days` threshold.
`ExpiresIn` should remain comfortably above the warning window; if either value
drifts, inspect `journalctl -u steampipe-steam-warm.service` for the failed
warm run.

## Viewing account status

```bash
cluster-ctl --vm-count 7 steam accounts
```

Displays account state by reading the host `loginStateDir` directly. The table
includes:

- `Login`: whether `loginusers.vdf` contains a logged-in SteamID.
- `Account`: the Steam persona name, falling back to the configured username.
- `SteamID`: the 17-digit SteamID found in `loginusers.vdf`.
- `RefreshAge`: days since the refresh-token file was last modified.
- `ExpiresIn`: days until the refresh token's JWT `exp` timestamp.
- `Status`: `OK`, `STALE`, `EXPIRED`, or `NO_TOKEN`.

By default, `steam accounts` exits non-zero if any known refresh token expires
within 30 days:

```bash
cluster-ctl --vm-count 7 steam accounts --warn-within-days 30
```

Raise or lower `--warn-within-days N` for stricter or looser pre-test gates.
For example, `--warn-within-days 365` intentionally fails for tokens with the
usual roughly 200-day expiry horizon, while `--warn-within-days 7` only fails
near expiry.

## Session model: why this works

Two non-obvious invariants are worth knowing if you ever touch the Steam-session
code paths:

- **Refresh tokens rotate on use, not on a fixed schedule.** Steam issues a
  fresh JWT every time the client successfully authenticates; the old token
  becomes invalid as soon as the new one is issued. The roughly 200-day `exp`
  claim is a hard ceiling, but in practice sessions decay because some prior
  rotation was not flushed to disk before the VM died. This is why every "Steam
  exited a successful session" path uses `graceful_stop_steam` (sends
  `steam -shutdown`, polls `pgrep`, then SIGTERM, then SIGKILL) rather than a
  bare `pkill`, and why `cluster-ctl steam warm` exists to refresh tokens on a
  schedule that has nothing to do with test cadence.
- **`loginStateDir/vm-N` writes are serialized by the VM lease.** The
  per-`vm-N` virtiofs share is shared across `--cluster` names — two clusters
  configured for the same `vm-N` slot would write to the same host directory.
  The lease in `src/vm/lease.rs` guarantees at most one cluster claims a slot
  at a time, so the share itself needs no further locking. If you ever
  introduce a second persistence path that bypasses the lease (snapshot
  restore, manual rsync), it must respect the same single-writer invariant.

The aggressive recovery path in `ensure_steam_script` (SIGTERM-then-SIGKILL
without `steam -shutdown`) is intentional and must not be unified with
`graceful_stop_steam`: it fires when Steam is detected as wedged and waiting
for a graceful exit would only delay the recovery.

## Non-NixOS fallback

On NixOS hosts, prefer enabling
`services.steampipe-cluster.loginRunners.enable = true` so the module publishes
`loginRunnersDir` in the discovered host config. Hosts that cannot use the
NixOS module can still pass the login runner directory explicitly:

```bash
cluster-ctl --vm-count 7 steam login --login-runners-dir ./result-login
```
