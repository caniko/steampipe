# cluster-ctl

A NixOS microVM cluster orchestrator for automated multiplayer game testing.

Originally developed as internal tooling for
[chessbender](https://store.steampowered.com/app/3171330/Regicide/) — an
online multiplayer chess game built with Rust and Steamworks — `cluster-ctl`
manages a fleet of lightweight NixOS VMs on a single host, handles Steam
authentication across all of them, deploys game binaries, and runs coordinated
end-to-end test sessions with heartbeat monitoring, failure capture, and
historical reporting.

While the defaults still reflect the chessbender setup, every game-specific
value is configurable via `steampipe.toml`, making `cluster-ctl` usable for
any Steam-based multiplayer game that needs automated multi-peer testing.

---

## Table of contents

1. [Prerequisites](#prerequisites)
2. [Installation](#installation)
3. [Quick start](#quick-start)
4. [Project configuration](#project-configuration)
5. [Networking](#networking)
6. [VM lifecycle](#vm-lifecycle)
7. [Steam setup](#steam-setup)
8. [Deploying your game](#deploying-your-game)
9. [Running the game](#running-the-game)
10. [End-to-end testing](#end-to-end-testing)
11. [Network simulation](#network-simulation)
12. [Live monitoring](#live-monitoring)
13. [Snapshots](#snapshots)
14. [Diagnostics](#diagnostics)
15. [Shell completions](#shell-completions)
16. [Multiple clusters](#multiple-clusters)
17. [Architecture overview](#architecture-overview)
18. [Full command reference](#full-command-reference)

---

## Prerequisites

`cluster-ctl` is a host-side tool that orchestrates NixOS microVMs. You need:

- **Linux host** (tested on NixOS, should work on any distro with the
  dependencies below)
- **NixOS microVM runners** — pre-built VM images with your game's runtime
  dependencies. In the chessbender project these live in
  `nix/test-cluster/` and are built via `nix build .#vm-runners`.
  Each VM gets a runner at `<runners-dir>/vm-N/bin/microvm-run`.
- **SSH key pair** — the VMs must accept passwordless SSH. Default location:
  `<project-root>/nix/test-cluster/cluster_key`
- **sudo** — required for `net-up`/`net-down` (bridge + NAT setup) and
  `netem` (traffic shaping)
- **Steam** — installed inside each VM image
- **nftables** (`nft`) — for NAT rules
- **Optional:** `grim` (Wayland screenshot tool) inside VMs for the
  `screenshot` and `--capture-on-failure` features. `wayvnc` + `sway` for
  interactive Steam login.

### How chessbender provides these

In the chessbender repository, the NixOS VM images are defined as a Nix flake
output. A simplified view of the relationship:

```
chessbender/
├── flake.nix                        # builds the game + VM images
├── nix/
│   └── test-cluster/
│       ├── default.nix              # NixOS module for VM configuration
│       ├── cluster_key              # SSH private key (baked into VMs)
│       └── cluster_key.pub          # matching public key
├── steampipe.toml                   # cluster-ctl configuration
├── vendor/
│   └── steamworks-rs/...            # Steam SDK
├── assets/                          # game assets deployed to VMs
├── steam_appid.txt                  # Steam application ID
└── tools/
    └── steampipe/                   # this crate (or used as a dependency)
```

The flake exposes runner packages:

```nix
# Simplified — each vm-N gets its own runner derivation
packages.vm-runners = buildMicroVMRunners {
  count = 7;
  sshKey = ./nix/test-cluster/cluster_key.pub;
  vmUser = "chessbender";
  # ... NixOS module with Steam, weston, game deps
};
```

After `nix build .#vm-runners`, you get a result directory with
`vm-1/bin/microvm-run` through `vm-7/bin/microvm-run`.

---

## Installation

### With Nix (recommended)

```bash
# Run directly from the flake
nix run github:user/steampipe -- --help

# Or add to your flake inputs
{
  inputs.steampipe.url = "github:user/steampipe";
  # ...
  # Then use: steampipe.packages.${system}.default
}
```

### With Cargo

```bash
cargo install --path /path/to/steampipe
```

The binary is called `cluster-ctl`.

---

## Quick start

This walks through a complete session — from zero to running an 8-player test.

### 1. Generate a config file

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

### 2. Set up networking

```bash
sudo cluster-ctl --vm-count 7 net-up
```

This creates:
- A bridge interface `br-cluster` at `10.0.100.254/24`
- TAP devices `tap-vm-1` through `tap-vm-7`
- nftables NAT rules so VMs can reach the internet

### 3. Start VMs

```bash
cluster-ctl --vm-count 7 up --runners-dir ./result
```

Each VM boots and `cluster-ctl` waits for SSH to become reachable.

### 4. Log into Steam on each VM

```bash
# Interactive mode — VNC into each VM to complete login + Steam Guard
cluster-ctl --vm-count 7 steam login --login-runners-dir ./result-login

# Or automated mode with a credentials file
cluster-ctl --vm-count 7 --credentials secrets/steam-creds.toml \
  steam login --login-runners-dir ./result-login
```

The credentials file format:

```toml
[vm.vm-1]
steam_user = "testaccount1"
steam_pass = "password1"
game_key = "XXXXX-XXXXX-XXXXX"   # optional

[vm.vm-2]
steam_user = "testaccount2"
steam_pass = "password2"
```

The file can also be age-encrypted (`.age` extension) — `cluster-ctl` will
decrypt with `rage`/`age` via ssh-agent automatically.

### 5. Verify Steam sessions

```bash
cluster-ctl --vm-count 7 steam check --runners-dir ./result
```

Boots each VM, verifies the Steam login is valid and Steam doesn't crash.

### 6. Deploy and test

```bash
# Deploy game binary + assets
cluster-ctl --vm-count 7 deploy

# Run an 8-player LAN test (host + 7 VMs)
cluster-ctl --vm-count 7 test --players 8 --network lan --max-runs 5
```

### 7. Tear down

```bash
cluster-ctl --vm-count 7 down
sudo cluster-ctl --vm-count 7 net-down
```

---

## Project configuration

`cluster-ctl` loads `steampipe.toml` from your project root (auto-detected by
walking up from CWD to find a `Cargo.toml` with `[workspace]`).

All fields are optional — unset values use the chessbender defaults.

```toml
# ─── Game identity ───
binary_name = "chessbender"        # name of the release binary
cargo_package = "chessbender"      # cargo -p package name
steam_appid_file = "steam_appid.txt"

# ─── VM environment ───
vm_user = "chessbender"            # SSH user inside VMs
remote_dir = "/home/chessbender/chessbender"  # deployment target
ssh_key = "nix/test-cluster/cluster_key"      # relative to project root
max_vms = 7                        # upper bound (1-7)

# ─── Deployment artifacts ───
steam_api_lib = "vendor/steamworks-rs/.../libsteam_api.so"
assets_dir = "assets"              # synced to VMs alongside the binary
log_file = "game.log"              # game log filename on VMs

# ─── Network ───
[network]
bridge = "br-cluster"
subnet = "10.0.100"                # vm-N gets .N within the pool
prefix = 24
host_ip = "10.0.100.254"
udp_port = 27100

# ─── Cluster identity (for multi-cluster setups) ───
[cluster]
name = "default"

# ─── Per-VM resource overrides ───
[[vm]]
index = 1
ram_mb = 4096
cpus = 2
```

### Configuration priority

Values are resolved in order: **CLI flags > steampipe.toml > built-in defaults**.

The `--project-root` flag overrides auto-detection. `--ssh-key` overrides the
config file value. `--cluster` overrides the `[cluster].name` field.

---

## Networking

### Setting up the cluster network

```bash
sudo cluster-ctl --vm-count 7 net-up
```

This creates a bridge topology:

```
 Host (10.0.100.254)
       │
  ┌────┴────┐  br-cluster
  │ Bridge  │
  ├─────────┤
  │tap-vm-1 │──── vm-1 (10.0.100.1)
  │tap-vm-2 │──── vm-2 (10.0.100.2)
  │   ...   │
  │tap-vm-7 │──── vm-7 (10.0.100.7)
  └─────────┘

  NAT: 10.0.100.0/24 → masquerade via default interface
```

Each VM's MAC address is deterministic (`52:54:00:cb:00:0N`).

### Tearing down

```bash
sudo cluster-ctl --vm-count 7 net-down
```

Removes all TAP devices, the bridge, and the nftables `cluster` table.

### Custom nft path

If `nft` isn't on PATH:

```bash
sudo cluster-ctl --vm-count 7 net-up --nft /run/current-system/sw/bin/nft
```

---

## VM lifecycle

### Starting VMs

```bash
cluster-ctl --vm-count 7 up --runners-dir ./result
```

`up` reserves `N` free slots from the pool and prints the actual leased set,
for example `Reserved: vm-2, vm-4`. Those are the VM names every follow-up
command operates on.

For each VM:
1. Stops any existing instance (PID file + orphan cleanup)
2. Launches `<runners-dir>/vm-N/bin/microvm-run`
3. Records PID in `$XDG_STATE_HOME/steampipe/vm-N.pid`
4. Waits for SSH readiness (30s timeout with exponential backoff)

### Stopping VMs

```bash
cluster-ctl --vm-count 7 down
```

Kills each VM by PID, cleans up orphaned `microvm@vm-N` processes, removes
leftover socket files.

### Restarting specific VMs

```bash
# Restart all VMs
cluster-ctl --vm-count 7 restart --runners-dir ./result

# Restart only vm-3
cluster-ctl --vm-count 7 restart vm-3 --runners-dir ./result
```

### Checking status

```bash
cluster-ctl --vm-count 7 status
```

```
VM       IP             Held By          PID    VM     SSH    Steam    Game
────────────────────────────────────────────────────────────────────────
vm-2     10.0.100.2     fixture          4242   UP     OK     OK       OK
vm-4     10.0.100.4     fixture          4249   UP     OK     OK       ─
```

`status` shows only the VMs currently claimed by this cluster. Unowned slots and
slots leased by other clusters are omitted.

All checks run in parallel.

---

## Steam setup

Steam must be authenticated on each VM before testing. `cluster-ctl` provides
two methods.

### Interactive login (VNC)

```bash
cluster-ctl --vm-count 7 steam login --login-runners-dir ./result-login
```

For each VM:
1. Boots the VM using a 4GB-RAM login image (larger for the GUI)
2. Starts `sway` (Wayland compositor) + `wayvnc` on port 5900
3. Launches Steam's GUI
4. You VNC in (`vncviewer 10.0.100.N:5900`) and complete login + Steam Guard

Interactive controls during login:
- `p` — paste text into the VM via `wtype`
- `Enter` — mark as done, shut down VM
- `Ctrl+C` — cancel

You can target specific VMs or continue from a starting point:

```bash
# Login only vm-3
cluster-ctl --vm-count 7 steam login vm-3 --login-runners-dir ./result-login

# Login vm-3 through vm-7
cluster-ctl --vm-count 7 steam login -+ vm-3 --login-runners-dir ./result-login
```

### Automated login (credentials file)

```bash
cluster-ctl --vm-count 7 \
  --credentials secrets/steam-creds.toml \
  steam login --login-runners-dir ./result-login
```

Runs `steam -login <user> <pass>` on each VM and waits for the connection log
to confirm success. Also activates game keys if provided.

> **Note:** Automated login may not work with accounts that have Steam Guard
> email/mobile confirmation required for new machines. Use the interactive
> method for the first login, then automated for refreshing sessions.

### Checking Steam health

```bash
cluster-ctl --vm-count 7 steam check --runners-dir ./result
```

Boots each VM one at a time, verifies:
- `loginusers.vdf` exists with a `PersonaName`
- Steam starts and survives for 12 seconds

### Viewing account status

```bash
cluster-ctl --vm-count 7 steam accounts
```

```
VM       Login  Account              SteamID
────────────────────────────────────────────────────────
vm-1     OK     player_one           76561198000000001
vm-2     OK     player_two           76561198000000002
vm-3     ─      ─                    ─

  2/3 VMs logged in (1 need login)
  Run `cluster-ctl steam login` to log in missing VMs
```

Also warns about duplicate accounts.

---

## Deploying your game

```bash
# Build and deploy
cluster-ctl --vm-count 7 deploy

# Deploy without rebuilding (binary already built)
cluster-ctl --vm-count 7 deploy --no-build

# Deploy with post-deploy checksum verification
cluster-ctl --vm-count 7 deploy --verify
```

Deployment syncs these files to each VM via `rsync`:
1. `target/release/<binary_name>` — the game binary
2. The Steam API shared library
3. `steam_appid.txt`
4. The `assets/` directory

All paths are configurable via `steampipe.toml`.

### Verification

`--verify` computes a SHA-256 checksum of the local binary and compares it
against `sha256sum` on each VM. Catches partial transfers or corruption.

```
==> Verifying deployments (binary SHA-256: a1b2c3d4e5f6...)
  vm-1: OK
  vm-2: OK
  vm-3: MISMATCH (remote: f6e5d4c3b2a1...)
```

---

## Running the game

### Start on all VMs

```bash
cluster-ctl --vm-count 7 run -- --mode lan --port 27100
```

Starts sway + Steam + your game binary on each VM. Trailing arguments are
passed to the game. (Weston is available as `--display weston` but experimental.)

### Stop the game

```bash
cluster-ctl --vm-count 7 stop-game
```

### Start only Steam (without the game)

```bash
cluster-ctl --vm-count 7 steam start
```

---

## End-to-end testing

The `test` command is the core workflow — it automates the full test cycle.

### Basic usage

```bash
# 8-player LAN test (host + 7 VMs), single run
cluster-ctl --vm-count 7 test --players 8 --network lan

# 10 consecutive runs over Steam networking, stop on first failure
cluster-ctl --vm-count 7 test \
  --players 4 \
  --network steam \
  --max-runs 10 \
  --stop-on-failure true
```

### What `test` does

1. **Build** — `cargo build --release -p <cargo_package>` (skip with
   `--build false`)
2. **Deploy** — rsync to all VMs (skip with `--deploy false`)
3. **Start Steam** — if `--network steam`, ensures Steam is running on all VMs
4. **Run loop** — for each iteration:
   - Kill any leftover game processes on VMs
   - Launch game on each VM via SSH (detached, logging to `game.log`)
   - Launch game locally (host player)
   - **Monitor** — poll heartbeat files (`game_progress_*.json`) every 2s
     (local) and 10s (VMs). If a heartbeat stalls for 30s, kill everything.
   - Wait for local process exit or timeout
   - Classify result: PASS (exit 0), FAIL (non-zero exit), TIMEOUT
   - Collect VM logs on failure
   - Optionally capture screenshots on failure
5. **Report** — summary with pass/fail/timeout counts
6. **Save** — results stored in test history

### Test configuration flags

| Flag | Default | Description |
|------|---------|-------------|
| `--network` | `lan` | Transport: `lan` or `steam` |
| `--players` | `8` | Player count (2-8, host + N-1 VMs) |
| `--vm-args` | — | Args for game on VMs |
| `--host-args` | — | Args for local (host) game |
| `--max-runs` | `1` | Number of test iterations |
| `--timeout` | `300` | Per-run timeout in seconds |
| `--shutdown-timeout` | `5` | Grace period before SIGKILL |
| `--stop-on-failure` | `true` | Stop after first failure |
| `--deploy` | `true` | Deploy before testing |
| `--build` | `true` | Build before deploying |
| `--filter-pattern` | (built-in) | Regex for output filtering |
| `--output-file` | — | Write full output to file |
| `--capture-on-failure` | `false` | Screenshot VMs on failure |

### Exit code labels

The test harness maps game exit codes to descriptive labels. These are
specific to chessbender but demonstrate the pattern:

| Code | Label |
|------|-------|
| 0 | SUCCESS |
| 10 | PHASE_WATCHDOG |
| 11 | DAG_VIOLATION |
| 12 | DESYNC |
| 13 | CONNECTION_LOST |
| 14 | INVARIANT_VIOLATION |
| 15 | COMMITMENT_FAILURE |
| 16 | FORMATION_WATCHDOG |
| 17 | MOVE_RESEND_WATCHDOG |
| 18 | PEER_READY_TIMEOUT |

### Heartbeat protocol

Your game should write periodic progress files to its working directory:

```json
// game_progress_player1.json
{ "timestamp_ms": 1710700000000 }
```

`cluster-ctl` watches these files on both the host and VMs. If the minimum
timestamp across all players stops advancing for 30 seconds, the test is
killed as stalled. This catches deadlocks and hangs that wouldn't trigger a
timeout.

### Test history

Results are automatically saved to `$XDG_STATE_HOME/steampipe/test_history.json`.

```bash
# View history
cluster-ctl --vm-count 7 history

# Show last 5 sessions
cluster-ctl --vm-count 7 history -n 5

# Clear history
cluster-ctl --vm-count 7 history --clear
```

```
Timestamp            Net     P    VMs  Pass   Fail   T/O    Result
──────────────────────────────────────────────────────────────────────
2026-03-17T14:30:00  lan     8    7    5      0      0      PASS
2026-03-17T15:00:00  steam   4    3    3      2      1      FAIL

  2 session(s), 10 run(s): 8 passed, 2 failed (80.0% pass rate)

  Failure breakdown:
    exit 12 (DESYNC): 1x
    exit -1 (TIMEOUT): 1x
```

---

## Network simulation

Simulate real-world network conditions on VM TAP devices using `tc netem`.
Requires sudo.

```bash
# Add 80ms latency with 10ms jitter to vm-3
sudo cluster-ctl --vm-count 7 netem vm-3 --latency 80 --jitter 10

# Add 2% packet loss to all VMs
sudo cluster-ctl --vm-count 7 netem all --loss 2

# Limit bandwidth to 1000 kbit/s on vm-1
sudo cluster-ctl --vm-count 7 netem vm-1 --rate 1000

# Combine everything
sudo cluster-ctl --vm-count 7 netem all --latency 50 --jitter 20 --loss 1 --rate 5000

# Check what's applied
cluster-ctl --vm-count 7 netem-show

# Clear all rules
sudo cluster-ctl --vm-count 7 netem-reset
```

This is powerful for testing:
- How your netcode handles packet loss
- Behavior under high-latency connections
- Bandwidth-constrained environments
- Combined degradation scenarios

---

## Live monitoring

### TUI dashboard

```bash
cluster-ctl --vm-count 7 watch
```

Opens a full-screen terminal UI showing real-time status of all VMs (running,
SSH, Steam, game). Refreshes every 5 seconds.

Controls:
- `q` — quit
- `r` — force refresh
- `Ctrl+C` — quit

Custom refresh interval:

```bash
cluster-ctl --vm-count 7 watch --interval 2
```

### Live log streaming

```bash
# Stream game logs from all VMs with color-coded prefixes
cluster-ctl --vm-count 7 logs --follow

# Show last 50 lines initially
cluster-ctl --vm-count 7 logs --follow -n 50
```

Output looks like:

```
[vm-1] Player connected: 76561198000000001
[vm-2] Player connected: 76561198000000002
[vm-1] Game started: 8 players
[vm-3] Received move: e2e4
```

Each VM gets a different color. `Ctrl+C` to stop.

### Collecting logs (one-shot)

```bash
# Collect to logs/cluster-<timestamp>/
cluster-ctl --vm-count 7 logs

# Collect to custom directory
cluster-ctl --vm-count 7 logs -o ./my-logs
```

### Screenshots

```bash
# Capture from all VMs
cluster-ctl --vm-count 7 screenshot

# Custom output directory
cluster-ctl --vm-count 7 screenshot -o ./captures
```

Requires `grim` installed in the VMs and sway running (weston is experimental).

---

## Snapshots

Save and restore VM state directories for fast iteration. Useful for skipping
the slow Steam login cycle.

```bash
# Save current state after Steam login
cluster-ctl --vm-count 7 snapshot-save after-login

# List snapshots
cluster-ctl --vm-count 7 snapshot-list

# Restore a snapshot
cluster-ctl --vm-count 7 down  # stop VMs first
cluster-ctl --vm-count 7 snapshot-restore after-login

# Delete a snapshot
cluster-ctl --vm-count 7 snapshot-delete old-snapshot
```

> **Warning:** Snapshots of running VMs may be inconsistent. Always stop VMs
> before restoring.

---

## Diagnostics

```bash
# Check cluster health
cluster-ctl --vm-count 7 doctor

# Auto-fix what can be fixed
cluster-ctl --vm-count 7 doctor --fix
```

Checks:
- State directory exists
- PID files reference live processes (removes stale ones with `--fix`)
- No orphaned `microvm@vm-N` processes (kills them with `--fix`)
- No leftover `.sock`/`.vsock` files (removes with `--fix`)
- Bridge interface exists
- TAP devices exist
- SSH key exists
- nftables cluster table exists

```
  [OK   ] state directory: /home/user/.local/state/steampipe
  [FIXED] pid:vm-1: removed stale PID file (was 12345)
  [OK   ] orphan:vm-1: no orphans
  [WARN ] sockets:vm-2: 1 stale socket file(s)
  [OK   ] bridge: br-cluster exists
  [OK   ] ssh key: /path/to/cluster_key
  [WARN ] nftables: no cluster table (NAT may not be configured)

==> 2 warning(s), 1 fixed — no errors
```

---

## Shell completions

```bash
# Bash
cluster-ctl completions bash > ~/.local/share/bash-completion/completions/cluster-ctl

# Zsh
cluster-ctl completions zsh > ~/.zfunc/_cluster-ctl

# Fish
cluster-ctl completions fish > ~/.config/fish/completions/cluster-ctl.fish
```

---

## Multiple clusters

Run independent clusters simultaneously with the `--cluster` flag:

```bash
# Start a "nightly" cluster
sudo cluster-ctl --vm-count 7 --cluster nightly net-up
cluster-ctl --vm-count 7 --cluster nightly up --runners-dir ./result

# Start a "feature-branch" cluster with 3 VMs
sudo cluster-ctl --vm-count 3 --cluster feature net-up
cluster-ctl --vm-count 3 --cluster feature up --runners-dir ./result-feature
```

Each cluster gets its own state directory
(`$XDG_STATE_HOME/steampipe/<cluster-name>/`) and can be managed
independently.

You can also set the default cluster name in `steampipe.toml`:

```toml
[cluster]
name = "dev"
```

---

## Architecture overview

### Typestate pattern

`ClusterConfig` uses a compile-time typestate to enforce that certain
operations (starting VMs, Steam login/check) can only happen after the
network bridge has been verified:

```rust
ClusterConfig<Unchecked>           // from ::from_vm_count()
    │
    ├── with_vm_ids(list_cluster_vms(...))? // existing-cluster commands
    ├── status(), down(), deploy()          // work on any state
    │
    └── reserve_n(...) + with_vm_ids(...)   // choose actual slots for `up`
        │
        └── validate_bridge()?              // checks bridge exists
            │
            └── ClusterConfig<BridgeReady>
                │
                └── up(), restart(), steam_login(), steam_check()
```

This is zero-cost — `PhantomData` is zero-sized, both states have identical
memory layout.

### Parallel operations

Most multi-VM operations use `par_each_vm()`, a helper that spawns one
tokio task per VM and collects results:

```rust
let results = par_each_vm(&config.vms, |vm| {
    let ssh = ssh.clone();
    async move {
        ssh.run(&vm.ip, "some command").await
    }
}).await?;
```

### State management

Runtime state is stored per cluster under `$XDG_STATE_HOME/steampipe/`
(typically `~/.local/state/steampipe/`):

```
steampipe/
└── nightly/
    ├── vm-2.pid            # PID files for the leased set
    ├── vm-4.pid
    ├── vm-2/
    │   ├── vm.log          # microVM stdout/stderr
    │   └── *.sock          # VM socket files (auto-cleaned)
    ├── vm-4/
    │   └── ...
    ├── test_history.json   # test result history
    └── snapshots/
        └── after-login/    # named snapshots
            ├── snapshot.json
            ├── vm-2/
            └── vm-4/
```

Snapshot directories key off the actual leased VM names, so a one-VM cluster
may legitimately store state under `vm-7/` rather than `vm-1/`.

### SSH transport

All VM communication goes through the system `ssh` binary (not libssh).
Connection options are hardcoded for speed and non-interactive use:

- `ConnectTimeout=2` — fast failure
- `StrictHostKeyChecking=no` — VMs are ephemeral
- `IdentitiesOnly=yes` — use only the specified key
- Exponential backoff on `wait_ready()` (500ms → 4s)

File transfers use `rsync -az --delete`.

---

## Full command reference

```
cluster-ctl [OPTIONS] <COMMAND>

Options:
  --project-root <PATH>    Project root (auto-detected)
  --ssh-key <PATH>         SSH key override
  --vm-count <N>           Number of VMs (required, 1-7)
  --credentials <PATH>     Steam credentials TOML file
  --cluster <NAME>         Cluster name for multi-cluster

VM Lifecycle:
  status                   Show VM status table
  up --runners-dir <DIR>   Start VMs
  down                     Stop all VMs
  restart [TARGET] --runners-dir <DIR>  Restart VMs

Networking:
  net-up [--nft PATH]      Create bridge + TAP + NAT (sudo)
  net-down [--nft PATH]    Tear down networking (sudo)

Deployment:
  deploy [--no-build] [--verify]  Build and deploy to VMs

Steam (grouped under `cluster-ctl steam <subcommand>`):
  steam login [TARGET] --login-runners-dir <DIR>   Interactive login
  steam guard <VM> [--code CODE]                   Submit Steam Guard code
  steam check [TARGET] --runners-dir <DIR>         Verify logins
  steam start [TARGET]                             Start sway + Steam (--display weston for experimental)
  steam accounts                                   Show Steam account status
  steam clean-logins [TARGET]                      Remove persisted login state

Game:
  run [-- ARGS...]         Start game on all VMs
  stop-game                Kill game on all VMs

Testing:
  test [OPTIONS]           Full E2E test pipeline
  history [-n N] [--clear] Test result history

Monitoring:
  logs [-o DIR] [-f] [-n N]  Collect or stream logs
  watch [-i SECS]          Live TUI dashboard
  screenshot [-o DIR]      Capture VM screenshots

Network Simulation:
  netem <TARGET> [--latency MS] [--jitter MS] [--loss %] [--rate KBIT]
  netem-show               Show current rules
  netem-reset [TARGET]     Clear rules

State Management:
  snapshot-save <NAME>     Save VM state
  snapshot-restore <NAME>  Restore VM state
  snapshot-list            List snapshots
  snapshot-delete <NAME>   Delete snapshot

Diagnostics:
  doctor [--fix]           Health check and repair

Setup:
  init                     Generate steampipe.toml
  completions <SHELL>      Generate shell completions
```

## CI

Woodpecker CI on Codeberg runs `cargo build`, `cargo test`, `cargo clippy`, and `cargo fmt --check` on every push and pull request. End-to-end tests requiring microVMs are excluded from CI and run locally.
