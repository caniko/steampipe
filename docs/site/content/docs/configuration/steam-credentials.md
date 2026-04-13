+++
title = "Steam Credentials"
description = "Managing Steam authentication across VMs"
weight = 20
+++

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

The file can also be age-encrypted (`.age` extension) — `cluster-ctl` will decrypt with `rage`/`age` via ssh-agent automatically.

## Interactive login (VNC)

```bash
cluster-ctl --vm-count 7 steam-login --login-runners-dir ./result-login
```

For each VM:
1. Boots the VM using a 4GB-RAM login image
2. Starts `sway` (Wayland compositor) + `wayvnc` on port 5900
3. Launches Steam's GUI
4. You VNC in (`vncviewer 10.0.100.N:5900`) and complete login + Steam Guard

Interactive controls during login:
- `p` — paste text into the VM via `wtype`
- `Enter` — mark as done, shut down VM
- `Ctrl+C` — cancel

Target specific VMs:

```bash
# Login only vm-3
cluster-ctl --vm-count 7 steam-login vm-3 --login-runners-dir ./result-login

# Login vm-3 through vm-7
cluster-ctl --vm-count 7 steam-login -+ vm-3 --login-runners-dir ./result-login
```

## Automated login

```bash
cluster-ctl --vm-count 7 \
  --credentials secrets/steam-creds.toml \
  steam-login --login-runners-dir ./result-login
```

Runs `steam -login <user> <pass>` on each VM and waits for confirmation. Also activates game keys if provided.

> **Note:** Automated login may not work with accounts that have Steam Guard email/mobile confirmation required for new machines. Use the interactive method for the first login, then automated for refreshing sessions.

## Checking Steam health

```bash
cluster-ctl --vm-count 7 steam-check --runners-dir ./result
```

Boots each VM one at a time and verifies:
- `loginusers.vdf` exists with a `PersonaName`
- Steam starts and survives for 12 seconds

## Viewing account status

```bash
cluster-ctl --vm-count 7 accounts
```

Displays logged-in accounts, SteamIDs, and warns about duplicate accounts.
