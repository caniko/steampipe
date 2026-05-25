# Installation

`cluster-ctl` ships as a Rust binary and a NixOS module. The NixOS module is
the recommended install on NixOS hosts because it installs the binary and owns
the host networking, host config, runner paths, and optional warm timer. The
bare binary is the fallback for non-NixOS hosts.

## Recommended: NixOS module

1. Add the flake input to your host flake:

   ```nix
   {
     inputs.steampipe.url = "codeberg:caniko/steampipe";
   }
   ```

2. Import the host module in your NixOS system:

   ```nix
   {
     inputs.steampipe.url = "codeberg:caniko/steampipe";

     outputs = {
       self,
       nixpkgs,
       steampipe,
       ...
     }: {
       nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
         system = "x86_64-linux";
         modules = [
           steampipe.nixosModules.default
           ({
             services.steampipe-cluster = {
               enable = true;
               tapOwner = "yourhostuser";
               tapOwnerUid = 1000;

               # Bootstrap first; enable after pasting sshAuthorizedKey below.
               loginRunners.enable = false;

               # Runtime VM runners also require sshAuthorizedKey before enabling.
               runners.enable = false;
             };
           })
         ];
       };
     };
   }
   ```

3. Replace `yourhostuser` and `tapOwnerUid` with the host user that will run
   `cluster-ctl`. The UID should match the in-VM user, normally `1000`.

4. First rebuild — bootstrap the cluster keypair. In your initial config, set
   `loginRunners.enable = false;` and `runners.enable = false;`, then:

   ```bash
   sudo nixos-rebuild switch --flake .#myhost
   ```

   The activation script creates the cluster keypair at
   `/var/lib/steampipe/ssh/cluster_key{,.pub}`.

5. Paste the pubkey into the host config. Read the generated pubkey:

   ```bash
   sudo cat /var/lib/steampipe/ssh/cluster_key.pub
   ```

   Add it to your config:

   ```nix
   services.steampipe-cluster.loginRunners = {
     enable = true;
     sshAuthorizedKey = "ssh-ed25519 AAAA... steampipe-loginrunners@host";
   };
   # And the same for runners.sshAuthorizedKey if you enabled runners.
   ```

6. Rebuild with the key baked in:

   ```bash
   sudo nixos-rebuild switch --flake .#myhost
   ```

   The pubkey now lives in every login-runner's authorized_keys.

Why two rebuilds? Flake evaluation runs in pure-eval mode, which forbids
reading `/var/lib/*` from disk. The first rebuild creates the key on the host;
the second rebuild bakes the key you just read into the VM closure.

7. Check the installed binary and host config:

   ```bash
   cluster-ctl --help
   cluster-ctl steam info
   ```

8. If `runners.enable = true;`, start the cluster or warm/check Steam from any
   directory:

   ```bash
   cluster-ctl up
   cluster-ctl steam check
   cluster-ctl steam warm
   ```

`nixos-rebuild switch` gives you:

- `cluster-ctl` on `PATH` at `/run/current-system/sw/bin/cluster-ctl`.
- Bash, zsh, and fish shell completions unless
  `services.steampipe-cluster.completions.enable = false;`.
- Bridge `br-cluster`, TAP devices, and NAT rules through NetworkManager.
- `/etc/steampipe/module.json`, so `cluster-ctl` works from any directory.
- `/etc/steampipe/login-runners/` when `loginRunners.enable = true;`.
- `/etc/steampipe/runners/` when `runners.enable = true;`.

To pin a different binary derivation, set `services.steampipe-cluster.package`.
See [Global host config](../configuration/host-config.md) for the full option
surface.

### Optional: weekly warm timer

Import the warm timer module next to the host module:

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

The warm timer requires `runners.enable = true;` because it relies on
`/etc/steampipe/module.json` to resolve `/etc/steampipe/runners`.

## Bare binary

For non-NixOS hosts:

```bash
nix run codeberg:caniko/steampipe -- --help
nix profile install codeberg:caniko/steampipe
```

With Cargo:

```bash
cargo install --path /path/to/steampipe
```

The bare binary does not configure the host. You are responsible for the host
config at `~/.config/steampipe/host.json`, networking with
`sudo cluster-ctl net-up`, and passing `--runners-dir <path>` for VM lifecycle
and Steam commands. The NixOS module does that host integration for you.

## Naming

Three names refer to the same toolchain:

- `steampipe` is the flake and repository name. Use it as
  `inputs.steampipe.url = "codeberg:caniko/steampipe";`.
- `cluster-ctl` is the binary name. On a module-managed host,
  `which cluster-ctl` returns `/run/current-system/sw/bin/cluster-ctl`.
- `services.steampipe-cluster` is the NixOS module option namespace. The
  `-cluster` suffix is a remnant of the original test-cluster naming.

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
