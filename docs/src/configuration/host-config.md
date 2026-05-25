# Global host config

## Concept

The global host config is the machine-local source of truth for a Steampipe
cluster. It exists so `cluster-ctl steam ...` can work without a project root:
NixOS hosts can publish the config from the module, and non-NixOS hosts can
write the same JSON by hand.

The important property is symmetry. The CLI does not care who wrote the file;
it only cares that the file matches the schema below and is discoverable via the
standard precedence list.

## Schema

`schemaVersion` is currently `2`. `cluster-ctl` accepts `1` and `2` for one
release cycle; version `1` prints an upgrade warning. The full JSON shape is:

```json
{
  "schemaVersion": 2,
  "vmCount": 7,
  "bridge": "br-cluster",
  "subnet": "10.0.100",
  "prefix": 24,
  "hostIp": "10.0.100.254",
  "tapOwner": "can",
  "runnersDir": "/etc/steampipe/runners",
  "loginRunnersDir": "/etc/steampipe/login-runners",
  "loginStateDir": "/var/lib/steampipe/logins",
  "credentialsPath": "/etc/steampipe/credentials.toml",
  "sshKey": "/var/lib/steampipe/ssh/cluster_key",
  "vmUser": "cluster",
  "accounts": [
    { "vm": "vm-1", "steamUser": "testaccount1" },
    { "vm": "vm-2", "steamUser": "testaccount2" }
  ]
}
```

Field reference:

- `schemaVersion` (`u32`): schema discriminator. `cluster-ctl` currently
  accepts `1` and `2`.
- `vmCount` (`u8`): total VM pool size for the host config.
- `bridge` (`string`): bridge name used by the VM network.
- `subnet` (`string`): IPv4 subnet prefix without the final octet, such as
  `10.0.100`.
- `prefix` (`u8`): network prefix length, usually `24`.
- `hostIp` (`string`): host-side IP on the cluster network.
- `tapOwner` (`string | null`): optional owner for TAP devices.
- `runnersDir` (`path string | null`): optional host-level directory
  containing regular per-VM `microvm-run` wrappers. When present,
  `cluster-ctl steam check` and `cluster-ctl steam warm` do not require
  `--runners-dir` outside a project root. Populated by the NixOS module when
  `services.steampipe-cluster.runners.enable = true`.
- `loginRunnersDir` (`path string | null`): optional host-level directory
  containing per-VM `microvm-run` wrappers for the interactive Steam login
  wizard. When present, `cluster-ctl steam login` does not require
  `--login-runners-dir` outside a project root. Populated by the NixOS module
  when `services.steampipe-cluster.loginRunners.enable = true`.
- `loginStateDir` (`path string`): directory containing per-VM Steam login
  state. For schema `2`, `loginStateDir/vm-N` is mounted writable inside the
  VM at `/home/${vm_user}/.local/share/Steam`.
- `credentialsPath` (`path string | null`): optional path to the credentials
  TOML or age-encrypted file.
- `sshKey` (`path string | null`): optional path to the SSH private key used to
  enter cluster VMs. When present, `cluster-ctl` uses it as the default
  `--ssh-key` outside a project root.
- `vmUser` (`string | null`): optional in-guest username that owns Steam state
  and runs the game binary. Defaults to `"cluster"`.
- `accounts` (`array`): configured Steam usernames, one per VM.
- `accounts[].vm` (`string`): VM name, such as `vm-3`.
- `accounts[].steamUser` (`string`): Steam username for that VM.

`runnersDir`, `loginRunnersDir`, `sshKey`, and `vmUser` are additive optional
fields. They do not require a schema version bump, and readers that do not know
about them ignore them. The NixOS module emits `runnersDir` only when
`services.steampipe-cluster.runners.enable = true`, emits `loginRunnersDir`
only when `services.steampipe-cluster.loginRunners.enable = true`, and emits
`sshKey` when either host-level runner kind is enabled.

## `loginStateDir`

The NixOS module persists Steam session state per VM under this directory
(default `/var/lib/steampipe/logins`, one subdirectory per `vm-N`). Each
subdirectory is mounted virtiofs-writable at
`/home/${vm_user}/.local/share/Steam` inside the VM, so every Steam write -
including per-boot refresh-token rotation - persists to the host between VM
lifecycles.

The directory layout under `loginStateDir/vm-N` is the Steam state tree itself,
not a wrapper around it. For example, the host sees paths such as
`loginStateDir/vm-1/config/loginusers.vdf` and
`loginStateDir/vm-1/config/<steamID>/local.vdf`.

### UID constraint

`tapOwner` on the host must resolve to the same numeric UID as the in-VM
`vm_user`. Both default to UID `1000`. If you set `tapOwner` to a host user
with a different UID, the in-VM Steam process cannot write to the share
(`EPERM` on saves), and sessions will fail to persist. The NixOS module warns
at evaluation time if `tapOwner` is `null` because that leaves `loginStateDir`
owned by root.

### Schema version

The host config emits `schemaVersion = 2` as of the writable-share persistence
change. Versions `1` and `2` both parse, with a warning printed on `1`; the
`1` to `2` change is behavioral. The mount model changed to one writable share
at `/home/${vm_user}/.local/share/Steam`; no schema field was added or removed.
The later `runnersDir`, `loginRunnersDir`, `sshKey`, and `vmUser` fields are
also additive; the NixOS module emits them without a schema version bump. A
future `cluster-ctl` release will drop `1` support. Run
`nixos-rebuild switch` with the updated module to regenerate schema-2
`/etc/steampipe/module.json`.

### For downstream consumers

Projects that consume `steampipe` as a flake input, such as chessbender, should
run `nix flake update steampipe` and then `nixos-rebuild switch` to pick up
schema-2 emission. No project-side code changes are required; the
`steampipe.toml` and `steampipe-cluster.*` interface is unchanged. If a project
pins to a pre-bump commit, it continues to receive schema-1 `module.json` and
the previous mount semantics, but it will not benefit from session persistence.

## `steampipe-warm-timer`

`inputs.steampipe.nixosModules.warmTimer` exposes an opt-in NixOS module that
wraps `cluster-ctl steam warm` in a systemd timer and oneshot service, so
refresh tokens rotate ahead of expiry without a manual cron entry.

Import it next to the host module and enable runtime runners explicitly:

```nix
{
  imports = [
    inputs.steampipe.nixosModules.default
    inputs.steampipe.nixosModules.warmTimer
  ];

  services.steampipe-cluster = {
    enable = true;
    tapOwner = "can";
    tapOwnerUid = 1000;
    runners.enable = true;
  };

  services.steampipe-warm-timer.enable = true;
}
```

The host-level timer requires `services.steampipe-cluster.runners.enable =
true`; evaluation fails if it is missing. The service invokes `cluster-ctl
steam warm` without `--runners-dir`, so `cluster-ctl` resolves
`/etc/steampipe/runners` from `/etc/steampipe/module.json`.

Options:

- `enable` (`bool`, default `false`): turn the timer on.
- `user` (`str`, default = `services.steampipe-cluster.tapOwner`): host user
  that runs the warm command. This user must own the cluster SSH key and map to
  the same numeric user that owns Steam login state, normally the host
  `tapOwner`.
- `sshKey` (`str`, default = `/var/lib/steampipe/ssh/cluster_key`): SSH private
  key path passed to `cluster-ctl --ssh-key`.
- `onCalendar` (`str`, default `"weekly"`): systemd `OnCalendar=` expression.
  Tune to `"daily"` for high-paranoia clusters or `"monthly"` for clusters
  that test rarely.
- `randomizedDelaySec` (`str`, default `"1h"`): systemd `RandomizedDelaySec=`
  jitter to spread load across hosts that run the same calendar entry.
- `extraArgs` (`list of str`, default `[]`): extra arguments appended to
  `cluster-ctl steam warm`, for example `[ "--cluster" "nightly" ]` when
  warming a non-default cluster context.

Project-flake consumers can still use `(mkTestCluster {...}).warmTimerModule`
when they intentionally want the timer bound to that project-built cluster
instead of the host-level `/etc/steampipe/module.json` configuration.
For the short host-level setup, see
[Optional: weekly warm timer](../getting-started/installation.md#optional-weekly-warm-timer).

### Operational notes

- The timer assumes the bridge is up and the cluster VMs are reachable. It does
  not run `net-up` or `up`; pair it with the NetworkManager-managed bridge from
  `services.steampipe-cluster.enable = true`.
- `Persistent=true` is set on the timer, so a host that was off during the
  scheduled window catches up on next boot.
- A failed `steampipe-steam-warm.service` unit means at least one VM had trouble
  during warming, not necessarily that the timer pattern is broken. Read the
  journal before re-triggering.

## Discovery precedence

`cluster-ctl` checks the following locations in order:

```text
1. --host-config <path>
2. $XDG_CONFIG_HOME/steampipe/host.json
3. /etc/steampipe/host.json
4. /etc/steampipe/module.json    (legacy NixOS-module path)
```

If nothing is found, Steam-group commands keep their existing project-root
behaviour.

## NixOS module walkthrough

```nix
{
  services.steampipe-cluster = {
    enable = true;
    tapOwner = "can";
    tapOwnerUid = 1000;
    accounts = {
      vm-1 = { steamUser = "alice"; steamPass = config.age.secrets.alice-pw.path; };
      vm-2 = { steamUser = "bob"; steamPass = config.age.secrets.bob-pw.path; };
    };

    # Runtime runners support steam check/warm/up outside a project root.
    runners.enable = true;
    # Login runners support steam login outside a project root.
    loginRunners.enable = true;
  };
}
```

When `services.steampipe-cluster.runners.enable = true`, the module publishes
`/etc/steampipe/runners` as a link-farm containing one `vm-N/bin/microvm-run`
wrapper per configured VM. It also emits
`runnersDir = "/etc/steampipe/runners"` and
`sshKey = "/var/lib/steampipe/ssh/cluster_key"` in
`/etc/steampipe/module.json`, so `cluster-ctl steam check`,
`cluster-ctl steam warm`, and `cluster-ctl up` can run from outside a project
root. This option is off by default because enabling seven runtime VM runners
adds roughly 7 GiB of VM closure paths to the host system closure.

When `services.steampipe-cluster.loginRunners.enable = true`, the module
publishes `/etc/steampipe/login-runners` as a link-farm containing one
`vm-N/bin/microvm-run` wrapper per configured VM. It also emits
`loginRunnersDir = "/etc/steampipe/login-runners"` and
`sshKey = "/var/lib/steampipe/ssh/cluster_key"` in
`/etc/steampipe/module.json`, so `cluster-ctl steam login` can run from
outside a project root.

The module installs `cluster-ctl` into the system profile by default through
`services.steampipe-cluster.package`. Override that option to use a fork, a
debug build, or a locally patched derivation:

```nix
{
  services.steampipe-cluster.package = inputs.steampipe.packages.${pkgs.system}.default;
}
```

Shell completions for bash, zsh, and fish are installed into the standard NixOS
completion directories by default. Disable them when completions are managed
out of band:

```nix
{
  services.steampipe-cluster.completions.enable = false;
}
```

The module owns the login-runner SSH keypair under `/var/lib/steampipe/ssh/`.
The directory is mode `0700`, the private key is mode `0600`, and the public
key is mode `0644`. `tapOwner` owns the files; login runners require
`tapOwner` so the same unprivileged user that runs `cluster-ctl` can read the
private key without `sudo`.

Login VMs default to 4096 MiB each because the Steam GUI path is memory-hungry.
Reduce this only if the login wizard works reliably on your host:

```nix
{
  services.steampipe-cluster.loginRunners.memory = 3072;
}
```

From `$HOME`, `cluster-ctl steam info` should render the discovered module and
its selected row in the trace:

```text
Host config source: /etc/steampipe/module.json (legacy NixOS-module path; schemaVersion 2)

Discovery trace:
  1. --host-config flag                                (not set)
  2. ~/.config/steampipe/host.json                     (not found)
  3. /etc/steampipe/host.json                          (not found)
  4. /etc/steampipe/module.json                        (selected)
```

The login-runner field should also show that it came from the same module
config:

```text
Resolved values:
  loginRunnersDir  /etc/steampipe/login-runners        /etc/steampipe/module.json
```

## Hand-written walkthrough

1. Create `~/.config/steampipe/host.json`:

   ```json
   { "schemaVersion": 2, "vmCount": 7, "bridge": "br-cluster",
     "subnet": "10.0.100", "prefix": 24, "hostIp": "10.0.100.254",
     "tapOwner": null, "loginStateDir": "/var/lib/steampipe/logins",
     "credentialsPath": null, "accounts": [] }
   ```

2. Verify it with `cluster-ctl steam info`.
3. Optional: create a credentials TOML anywhere convenient and set
   `credentialsPath` to point at it.

## Troubleshooting

- If `steam info` shows `(not found)` for every row, check `XDG_CONFIG_HOME`
  or `HOME`, confirm the file exists, and make sure you picked the right file
  name (`host.json` versus `module.json`).
- If you see an unsupported `schemaVersion` error, regenerate the host config
  from the NixOS module or set `schemaVersion` to one of the versions listed in
  the error.
- If `--host-config /tmp/foo.json` errors while reading the file, check the
  path and permissions with `ls -l /tmp/foo.json`.
