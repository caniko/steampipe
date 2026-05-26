# VM Lifecycle

## Starting VMs

```bash
cluster-ctl --vm-count 7 up --runners-dir ./result
```

For each VM:

1. Stops any existing instance (PID file + orphan cleanup)
2. Waits on the host memory admission gate
3. Launches `<runners-dir>/vm-N/bin/microvm-run`
4. Records PID in `$XDG_STATE_HOME/steampipe/vm-N.pid`
5. Waits for SSH readiness (30s timeout with exponential backoff)

The admission gate is silent under normal load. See
[VM resources](./vm-resources.md) for the memory contract, admission status
command, and guest-side ballooning behavior.

## Stopping VMs

```bash
cluster-ctl --vm-count 7 down
```

Kills each VM by PID, cleans up orphaned `microvm@vm-N` processes, removes
leftover socket files.

## Restarting

```bash
# Restart all VMs
cluster-ctl --vm-count 7 restart --runners-dir ./result

# Restart only vm-3
cluster-ctl --vm-count 7 restart vm-3 --runners-dir ./result
```

## Resetting VM Home Images

```bash
cluster-ctl --vm-count 7 reset vm-1 --home
```

Deletes the VM home image so the next boot recreates it from the runner's
current default size. See [VM resources](./vm-resources.md) for disk sizing and
the reset guard.

## Status

```bash
cluster-ctl --vm-count 7 status
```

`up` reserves `N` free slots from the pool and prints the actual leased set,
for example `Reserved: vm-2, vm-4`. Follow-up commands use that leased set,
not a synthetic `vm-1..vm-N` prefix.

```
VM       IP             Held By          PID    VM     SSH    Steam    Game
────────────────────────────────────────────────────────────────────────
vm-2     10.0.100.2     fixture          4242   UP     OK     OK       OK
vm-4     10.0.100.4     fixture          4249   UP     OK     OK       -
```

`status` shows only the VMs currently claimed by this cluster. Unowned slots and
slots leased by other clusters are omitted.

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

> **Warning:** Snapshots of running VMs may be inconsistent. Always stop VMs
> before restoring.

## Diagnostics

```bash
# Check cluster health
cluster-ctl --vm-count 7 doctor

# Auto-fix what can be fixed
cluster-ctl --vm-count 7 doctor --fix
```

Checks state directory, PID files, orphaned processes, socket files, bridge
interface, TAP devices, SSH key, and nftables rules.
