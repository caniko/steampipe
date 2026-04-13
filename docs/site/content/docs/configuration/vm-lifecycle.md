+++
title = "VM Lifecycle"
description = "Starting, stopping, and managing VMs"
weight = 30
+++

## Starting VMs

```bash
cluster-ctl --vm-count 7 up --runners-dir ./result
```

For each VM:
1. Stops any existing instance (PID file + orphan cleanup)
2. Launches `<runners-dir>/vm-N/bin/microvm-run`
3. Records PID in `$XDG_STATE_HOME/steampipe/vm-N.pid`
4. Waits for SSH readiness (30s timeout with exponential backoff)

## Stopping VMs

```bash
cluster-ctl --vm-count 7 down
```

Kills each VM by PID, cleans up orphaned `microvm@vm-N` processes, removes leftover socket files.

## Restarting

```bash
# Restart all VMs
cluster-ctl --vm-count 7 restart --runners-dir ./result

# Restart only vm-3
cluster-ctl --vm-count 7 restart vm-3 --runners-dir ./result
```

## Status

```bash
cluster-ctl --vm-count 7 status
```

```
VM       IP             VM     SSH    Steam  Game
────────────────────────────────────────────────────────
vm-1     10.0.100.1     UP     OK     OK     OK
vm-2     10.0.100.2     UP     OK     OK     ─
vm-3     10.0.100.3     ─      ─      ─      ─
```

All checks run in parallel.

## Snapshots

Save and restore VM state directories for fast iteration:

```bash
# Save current state after Steam login
cluster-ctl --vm-count 7 snapshot-save after-login

# List snapshots
cluster-ctl --vm-count 7 snapshot-list

# Restore a snapshot (stop VMs first)
cluster-ctl --vm-count 7 down
cluster-ctl --vm-count 7 snapshot-restore after-login

# Delete a snapshot
cluster-ctl --vm-count 7 snapshot-delete old-snapshot
```

> **Warning:** Snapshots of running VMs may be inconsistent. Always stop VMs before restoring.

## Diagnostics

```bash
# Check cluster health
cluster-ctl --vm-count 7 doctor

# Auto-fix what can be fixed
cluster-ctl --vm-count 7 doctor --fix
```

Checks state directory, PID files, orphaned processes, socket files, bridge interface, TAP devices, SSH key, and nftables rules.
