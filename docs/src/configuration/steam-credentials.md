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

## Headless token login

```bash
cluster-ctl steam login
```

By default, `steam login` skips VMs whose persisted host-side session is
already `OK`. VMs with `STALE`, `EXPIRED`, or `NO_TOKEN` status are logged in;
pass `--force` to re-login every selected VM regardless.

For each VM selected for login:

1. Reads the VM's Steam username and password from the configured credentials
   TOML or age-encrypted credentials file.
2. Boots or reuses the VM and starts Steam under sway.
3. Types the credentials into the real Steam GUI.
4. Prompts for Steam Guard through the host dialog, `--code`, or
   `STEAMPIPE_GUARD_CODE` when Steam requires a one-time code.
5. Treats the session as valid only after Steam writes GUI login evidence into
   `loginStateDir`.

Host-side Steam Vent/JWT artifacts are diagnostic only. They do not make
`steam accounts` report `OK`; a GUI-valid session requires `loginusers.vdf` plus
Steam GUI cache evidence produced by the VM client.

Target specific VMs:

```bash
# Login only vm-3
cluster-ctl steam login vm-3

# Login vm-3 through vm-7
cluster-ctl steam login vm-3 -+
```

The host writes the minimal Steam session state directly into the writable
login-state share. Because the share is the VM's real
`~/.local/share/Steam`, later Steam client writes survive `cluster-ctl down` /
`up` cycles instead of being copied out or regenerated at boot.

## Automated login

```bash
cluster-ctl --credentials secrets/steam-creds.toml steam login
```

Boots or reuses each selected VM, types credentials into the Steam GUI, and
writes the resulting GUI-valid session artifacts into `loginStateDir`.

By default, automated `steam login` fills the gaps only: sessions already
reported as `OK` are skipped, while non-GUI-valid sessions are driven through
the login flow. Pass `--force` to re-login every selected VM.

Automated `steam login` prioritizes correctness over speed. Steam Guard prompts
are serialized through the host dialog so the same prompt can show a rejected
code and request a retry.

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

This lifecycle is still used by fallback/manual login runners and by runtime
Steam operations that boot VMs; the headless token-mint login path itself does
not need to boot a login VM.

## Manual VNC fallback

If a host cannot use the credentials-backed mint flow, pass a login runner
directory and run the legacy interactive login path for one VM at a time:

```bash
cluster-ctl --vm-count 7 steam login --login-runners-dir ./result-login
```

The fallback boots the 4 GiB login VM, mounts
`loginStateDir/vm-N` at `/home/${vm_user}/.local/share/Steam`, starts
`sway` + `wayvnc`, and leaves the VM running for manual Steam GUI login over
VNC. Use `vncviewer 10.0.100.N:5900`, complete login + Steam Guard in the
window, then validate with `cluster-ctl steam accounts`.

## Steam Guard dialog helper

When Steam requires a Guard code and no `--code` or `STEAMPIPE_GUARD_CODE` is
provided, `cluster-ctl` launches the separate `cluster-guard-prompt` helper on
the host. The helper is a small iced GUI that prints the entered code to stdout
for the parent process; it does not write the code to logs or files.

The helper is packaged separately from `cluster-ctl` so iced, winit, Wayland,
X11, and renderer dependencies do not enter the default CLI closure. The
packaged helper wrapper and the dev shell both provide the GUI runtime
libraries that iced/winit loads dynamically on NixOS, including Wayland,
libxkbcommon, libGL/libglvnd, Vulkan loader, fontconfig/freetype, and X11
libraries.

If a display socket exists but those libraries are missing, the helper exits
`12` with an actionable error instead of panicking during winit event-loop
creation. A missing display also exits `12`; timeout exits `11`; cancel exits
`10`. Both iced renderers remain enabled (`wgpu` and `tiny-skia`). The resolved
host-dialog failure mode was missing Wayland client-library discovery, not a GPU
or renderer failure.

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

`steam warm [target]` no longer creates or refreshes GUI-valid sessions. Run
`cluster-ctl steam login <vm> --force` when `steam accounts` reports a VM as
not GUI-valid.

Targeting follows the same VM syntax as login:

```bash
cluster-ctl --vm-count 7 steam warm vm-3 --runners-dir ./result
cluster-ctl --vm-count 7 steam warm -+ vm-3 --runners-dir ./result
```

### Automated rotation

The warm-timer module is retained for compatibility but cannot satisfy the
Steam GUI client. Host-level warm timers should be disabled for GUI session
freshness; use explicit `steam login` instead.

Verify GUI session state with `cluster-ctl steam accounts`. JWT-only Steam Vent
state appears as not GUI-valid and should be repaired with
`cluster-ctl steam login <vm> --force`.

## Viewing account status

```bash
cluster-ctl --vm-count 7 steam accounts
```

Displays account state by reading the host `loginStateDir` directly. The table
includes:

- `Login`: whether `loginusers.vdf` contains a logged-in SteamID.
- `Account`: the Steam persona name, falling back to the configured username.
- `SteamID`: the 17-digit SteamID found in `loginusers.vdf`.
- `RefreshAge`: days since the GUI cache artifact was last modified.
- `ExpiresIn`: retained for legacy output; GUI-valid sessions do not derive
  status from a JWT expiry.
- `Status`: `OK`, `NO_TOKEN`, or `UNKNOWN`.

By default, `steam accounts` exits non-zero when known session evidence is not
GUI-valid:

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
  bare `pkill`.
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

## Non-NixOS manual fallback

On NixOS hosts, prefer enabling
`services.steampipe-cluster.loginRunners.enable = true` so the module publishes
`loginRunnersDir` in the discovered host config for manual VNC fallback.
Hosts that cannot use the NixOS module can still pass the login runner
directory explicitly:

```bash
cluster-ctl --vm-count 7 steam login --login-runners-dir ./result-login
```
