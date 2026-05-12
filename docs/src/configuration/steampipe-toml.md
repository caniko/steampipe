# steampipe.toml

`cluster-ctl` loads `steampipe.toml` from your project root (auto-detected by
walking up from CWD to find a `Cargo.toml` with `[workspace]`).

All fields are optional - unset values use built-in defaults.

## Full reference

```toml
# Game identity
binary_name = "chessbender"        # name of the release binary
cargo_package = "chessbender"      # cargo -p package name
steam_appid_file = "steam_appid.txt"

# VM environment
vm_user = "chessbender"            # SSH user inside VMs
remote_dir = "/home/chessbender/chessbender"  # deployment target
ssh_key = "nix/test-cluster/cluster_key"      # relative to project root
max_vms = 7                        # upper bound (1-7)

# Deployment artifacts
steam_api_lib = "vendor/steamworks-rs/.../libsteam_api.so"
assets_dir = "assets"              # synced to VMs alongside the binary
log_file = "game.log"              # game log filename on VMs

# Network
[network]
bridge = "br-cluster"
subnet = "10.0.100"                # vm-N gets .N within the pool
prefix = 24
host_ip = "10.0.100.254"
udp_port = 27100

# Cluster identity (for multi-cluster setups)
[cluster]
name = "default"
hypervisor = "crosvm"              # default; cloud-hypervisor / qemu /
                                   # firecracker / stratovirt are experimental
graphics = true                    # virtio-gpu (mesa/DRI in guest)

# Per-VM resource overrides
[[vm]]
index = 1
ram_mb = 4096
cpus = 2
```

## Configuration priority

Values are resolved in order: **CLI flags > steampipe.toml > built-in defaults**.

- `--project-root` overrides auto-detection
- `--ssh-key` overrides the config file value
- `--cluster` overrides `[cluster].name`

## Supported display + screenshot-backend matrix

The compositor and screenshot backend are not independent. `cluster-ctl` rejects
incompatible combinations at startup with a pointer to the right pair.

| `--display`   | `--screenshot-backend grim` | `--screenshot-backend vnc` | Notes                                                      |
| ------------- | --------------------------- | -------------------------- | ---------------------------------------------------------- |
| `headless`    | n/a                         | n/a                        | No compositor; visual testing disabled.                    |
| `sway`        | ✅ supported (default)      | ✅ supported               | Default for non-GPU runs. Captures via the Wayland socket. |
| `sway-gpu`    | ❌ rejected                 | ✅ supported               | GPU display modes have no host-visible Wayland socket; use VNC. |
| `weston`      | ❌ rejected                 | ❌ rejected                | Experimental — the visual harness has no Weston readiness probe. |
| `weston-gpu`  | ❌ rejected                 | ❌ rejected                | Experimental — same reason as `weston`.                    |

Visual scenes (`[visual]` block in `steampipe.toml`, `cluster-ctl visual ...`)
and the fixture golden regenerator require **sway**: readiness checks call
`swaymsg -t get_outputs/get_tree`, and `wayvnc` only attaches to a wlroots
compositor.
