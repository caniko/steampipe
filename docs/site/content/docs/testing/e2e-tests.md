+++
title = "E2E Tests"
description = "Running automated end-to-end test sessions"
weight = 10
+++

The `test` command automates the full test cycle: build, deploy, run, monitor, and report.

## Basic usage

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

## What `test` does

1. **Build** — `cargo build --release -p <cargo_package>` (skip with `--build false`)
2. **Deploy** — rsync to all VMs (skip with `--deploy false`)
3. **Start Steam** — if `--network steam`, ensures Steam is running on all VMs
4. **Run loop** — for each iteration:
   - Kill leftover game processes on VMs
   - Launch game on each VM via SSH (detached, logging to `game.log`)
   - Launch game locally (host player)
   - Monitor heartbeat files every 2s (local) and 10s (VMs)
   - Wait for local process exit or timeout
   - Classify result: PASS, FAIL, or TIMEOUT
   - Collect VM logs and optionally capture screenshots on failure
5. **Report** — summary with pass/fail/timeout counts
6. **Save** — results stored in test history

## Configuration flags

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
| `--capture-on-failure` | `false` | Screenshot VMs on failure |

## Heartbeat protocol

Your game should write periodic progress files to its working directory:

```json
{ "timestamp_ms": 1710700000000 }
```

`cluster-ctl` watches these files on both the host and VMs. If the minimum timestamp across all players stops advancing for 30 seconds, the test is killed as stalled. This catches deadlocks and hangs that wouldn't trigger a timeout.

## Deploying

```bash
# Build and deploy
cluster-ctl --vm-count 7 deploy

# Deploy without rebuilding
cluster-ctl --vm-count 7 deploy --no-build

# Deploy with checksum verification
cluster-ctl --vm-count 7 deploy --verify
```

Deployment syncs the game binary, Steam API library, `steam_appid.txt`, and assets directory to each VM via `rsync`.
