# Installation

## With Nix (recommended)

```bash
# Run directly from the flake
nix run codeberg:caniko/steampipe -- --help

# Or add to your flake inputs
{
  inputs.steampipe.url = "codeberg:caniko/steampipe";
  # Then use: steampipe.packages.${system}.default
}
```

## With Cargo

```bash
cargo install --path /path/to/steampipe
```

The binary is called `cluster-ctl`.

## Prerequisites

`cluster-ctl` is a host-side tool that orchestrates NixOS microVMs. You need:

- **Linux host** - tested on NixOS, should work on any distro with the
  dependencies below
- **NixOS microVM runners** - pre-built VM images with your game's runtime
  dependencies
- **SSH key pair** - the VMs must accept passwordless SSH
- **sudo** - required for `net-up`/`net-down` (bridge + NAT setup) and
  `netem` (traffic shaping)
- **Steam** - installed inside each VM image
- **nftables** (`nft`) - for NAT rules
- **Optional:** `grim` (Wayland screenshot tool) inside VMs for screenshot
  capture. `wayvnc` + `sway` for interactive Steam login.
