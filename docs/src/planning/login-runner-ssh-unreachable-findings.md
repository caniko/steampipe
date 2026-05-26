# Login Runner SSH Unreachable Findings

## Goal And Trigger

`cluster-ctl steam login all` on atlas reports `Waiting for SSH... timeout
after 180s` for every login-runner VM, then `VM log unavailable: ...vm.log`.
The VMs are booting (`vm.log` reaches `Reached target Multi-User System` and
SSH Daemon starts) yet TCP SSH to `10.0.100.x` never succeeds.

## Root Cause

The login-runner derivations are built with an **empty authorized_keys list
for the in-VM `cluster` user**. The host-managed cluster pubkey at
`/var/lib/steampipe/ssh/cluster_key.pub` never makes it into the runner's
NixOS system closure, so sshd accepts the TCP connection and rejects every
key the host offers.

The mechanism that was supposed to embed the key is
[nix/module.nix:251-262](../../../nix/module.nix#L251-L262):

```nix
sshAuthorizedKey = lib.mkOption {
  default =
    if builtins.pathExists "${loginRunnerSshKey}.pub"
    then lib.removeSuffix "\n" (builtins.readFile "${loginRunnerSshKey}.pub")
    else "";
  ...
};
```

with `loginRunnerSshKey = "/var/lib/steampipe/ssh/cluster_key"`.

**Flake evaluation runs in `--pure-eval` mode**, and pure-eval makes
`builtins.pathExists` on absolute paths outside the Nix store return
`false`, even when the file exists. Verified locally:

```bash
$ nix eval --impure --expr 'builtins.pathExists "/var/lib/steampipe/ssh/cluster_key.pub"'
true
$ nix eval --expr 'builtins.pathExists "/var/lib/steampipe/ssh/cluster_key.pub"'
false
```

So every `nixos-rebuild switch` from canix (which uses flakes) silently
takes the `else ""` branch. The two-rebuild dance documented in
[docs/src/getting-started/installation.md:63-65](../getting-started/installation.md#L63-L65)
and in commit 4ee68f0 ("Drop runtime ssh-authorized virtiofs share from
login runners") cannot succeed under flake evaluation; no number of
rebuilds will ever bake the key.

## Evidence

- `/var/lib/steampipe/ssh/cluster_key.pub` exists, created 2026-05-25 11:21
  (gen 341), content
  `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIC49vWFN00fazQJUeuYP3rBLKDAO1qeIOLf5tYOTKNgb steampipe-loginrunners@`.
- `/run/current-system` was activated 2026-05-25 22:58 (gen 346), well after
  the key existed. Six rebuilds (gen 341-346) followed key creation.
- The live login-runner store path
  `/nix/store/yb2fq00hvw86gx7xgsscwq5m7h39ll4g-microvm-vm-1-microvm-run`
  points at NixOS system
  `/nix/store/67g62pxw32hqmxb2m49rfjvlsbp7j0jv-nixos-system-vm-1-...`.
  `grep -rln AAAAC3NzaC1 <that system>` returns no matches anywhere in the
  closure — no `authorized_keys`, no `authorized_keys.d/cluster`, nothing.
- `users-groups.json` for that system shows `cluster` with no `openssh`
  attribute at all — equivalent to `authorizedKeys.keys = []`.
- `/etc/steampipe/module.json` confirms the loaded host config:
  `loginRunnersDir = /etc/steampipe/login-runners`, `vmCount = 8`,
  `sshKey = /var/lib/steampipe/ssh/cluster_key`, all consistent.
- `vm-1/vm.log` shows the VM booting to multi-user, OpenSSH listening on
  vsock and TCP, then dying when `cluster-ctl` kills it on timeout. The
  systemd-ssh-generator banner "Try contacting this VM's SSH server via
  'ssh vsock%4' from host" is informational, not a fallback the runner
  uses.
- `40-eth0.network` in the VM system has the right static address
  `10.0.100.1/24` and gateway. Networking is not the problem.
- Canix flake input pins steampipe at commit `4ee68f0` — the commit that
  *introduced* the build-time-bake approach by removing the previous
  runtime virtiofs `AuthorizedKeysCommand` fallback. Before 4ee68f0 the
  key was provided at runtime through `sshAuthorizedKeysPath`/virtiofs;
  that mechanism was broken for a different reason (vhost-user socket not
  served by `microvm-run`) and got replaced with the pure-eval-incompatible
  default reader.

## Recommended Fix

### Immediate unblock (host-side, no code change)

In canix's `services.steampipe-cluster` config, set `loginRunners.sshAuthorizedKey`
explicitly so flake eval doesn't depend on `pathExists`:

```nix
services.steampipe-cluster.loginRunners = {
  enable = true;
  sshAuthorizedKey =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIC49vWFN00fazQJUeuYP3rBLKDAO1qeIOLf5tYOTKNgb steampipe-loginrunners@";
};
```

Then `sudo nixos-rebuild switch`. The key is the current contents of
`/var/lib/steampipe/ssh/cluster_key.pub`; if you later rotate it, paste the
new value in.

Alternatively, run `sudo nixos-rebuild switch --impure` once — `--impure`
disables `--pure-eval`, the default reader picks the file up, and the bake
happens. Subsequent pure rebuilds will keep working until something
invalidates the eval cache.

### Module-level fix (steampipe repo)

The module's auto-read default is dead code under flakes. Pick one:

1. **Drop the auto-read default**: change the option default to `""`,
   require operators to set `sshAuthorizedKey` explicitly (with a clear
   warning if `loginRunners.enable` is true and the key is empty). Removes
   the false promise of "rebuild twice and it works".
2. **Move the bake out of eval**: write the key into the runner closure
   via an activation script. Have the runner's `microvm-run` script read
   `/var/lib/steampipe/ssh/cluster_key.pub` at VM start and stage it into
   a tmpfs that the VM ingests on first boot (e.g., via a host-side
   `authorized_keys` file mounted as virtiofs **with a working
   virtiofsd**, or via cloud-init / sshd `AuthorizedKeysCommand` that
   shells out through vsock).
3. **Use systemd-ssh-generator's vsock path**: `cluster-ctl` already sees
   the VM advertise `ssh vsock%4` in the boot log. Switching `wait_ready`
   and the cluster login flow to AF_VSOCK SSH removes the TCP-authorized-key
   dependency entirely for the login phase.

Option 1 is the smallest change and matches the new "host emits the
keypair" mental model. Options 2-3 are larger.

## Optional Follow-Ups

- `print_vm_log_tail` reported `VM log unavailable` even though the file
  exists at `/home/can/.local/state/steampipe/default/vm-1/vm.log`
  (15 KiB, readable). Worth a quick look in
  [src/game/steam.rs:748-753](../../../src/game/steam.rs#L748-L753) — the
  read may be racing the next VM's start that overwrites the dir, or the
  path resolution differs from the actual write path in
  [src/core/backend.rs:305-306](../../../src/core/backend.rs#L305-L306).
- The host-generated pubkey hostname is empty
  (`steampipe-loginrunners@`). The activation script runs `hostname`
  inline in a context where it returns empty. Cosmetic, but worth
  tightening if the activation script is touched.

## Open Decisions

- Pick a module-level direction (option 1 / 2 / 3 above) before this can
  be considered fully fixed for fresh installs. The immediate unblock
  works for atlas today; new operators following installation.md will
  still hit this until the module changes.
