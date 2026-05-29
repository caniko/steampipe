# Quick Start

This walks through a complete session - from zero to running an 8-player test.

## 1. Generate a config file

```bash
cd /path/to/your-game
cluster-ctl init
```

This creates `steampipe.toml`. Edit it to match your project:

```toml
binary_name = "my-game"
cargo_package = "my-game"
vm_user = "player"
remote_dir = "/home/player/game"
ssh_key = "nix/test-cluster/cluster_key"
steam_api_lib = "vendor/steam/lib/linux64/libsteam_api.so"
steam_appid_file = "steam_appid.txt"
assets_dir = "assets"
log_file = "game.log"

[network]
bridge = "br-cluster"
subnet = "10.0.100"
host_ip = "10.0.100.254"
udp_port = 27100
```

### Operating from any directory

If your host has `/etc/steampipe/{host,module}.json` (NixOS module) or
`~/.config/steampipe/host.json` (hand-written), the Steam-group subcommands
work from any directory:

```bash
cluster-ctl steam info
cluster-ctl steam check
```

See [Global host config](../configuration/host-config.md) for the schema and
discovery details.

## 2. Set up networking

```bash
sudo cluster-ctl --vm-count 7 net-up
```

This creates a bridge interface, TAP devices for each VM, and nftables NAT
rules.

## 3. Start VMs

```bash
cluster-ctl --vm-count 7 up --runners-dir ./result
```

Each VM boots and `cluster-ctl` waits for SSH to become reachable.

## 4. Log into Steam

### Recommended: NixOS host (canix)

Set `services.steampipe-cluster.loginRunners.enable = true` and run
`nixos-rebuild switch`. See also: [NixOS module walkthrough](../configuration/host-config.md#nixos-module-walkthrough).
Then from any directory:

```bash
cluster-ctl steam login           # all VMs (default)
cluster-ctl steam login vm-3      # single VM
cluster-ctl steam login vm-3 -+   # vm-3..vm-N
```

No flags needed: the host config provides the VM count, SSH key, credentials
path, login state directory, and fallback login-runner directory. With
credentials configured, login mints SteamClient refresh tokens on the host and
writes them into `loginStateDir`; no login VM or VNC session is booted.

### Fallback: project flake (non-NixOS hosts)

When the host is not using the NixOS module, supply the credentials file and
login-state directory explicitly. Supply `--login-runners-dir` only for the
manual VNC fallback.

```bash
cluster-ctl --vm-count 7 --credentials secrets/steam-creds.toml \
  steam login --login-state-dir /var/lib/steampipe/logins

# Manual fallback: boot login VMs and complete Steam login over VNC.
cluster-ctl --vm-count 7 steam login \
  --login-runners-dir ./result-login
```

## 5. Deploy and test

```bash
# Deploy game binary + assets
cluster-ctl --vm-count 7 deploy

# Run an 8-player LAN test (host + 7 VMs)
cluster-ctl --vm-count 7 test --players 8 --network lan --max-runs 5
```

## 6. Tear down

```bash
cluster-ctl --vm-count 7 down
sudo cluster-ctl --vm-count 7 net-down
```
