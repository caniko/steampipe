+++
title = "Test History"
description = "Viewing and managing test results"
weight = 20
+++

Results are automatically saved to `$XDG_STATE_HOME/steampipe/test_history.json`.

## Viewing history

```bash
# View all history
cluster-ctl --vm-count 7 history

# Show last 5 sessions
cluster-ctl --vm-count 7 history -n 5

# Clear history
cluster-ctl --vm-count 7 history --clear
```

## Output format

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

## Monitoring

### TUI dashboard

```bash
cluster-ctl --vm-count 7 watch
```

Full-screen terminal UI showing real-time status of all VMs. Controls: `q` quit, `r` refresh.

### Live log streaming

```bash
# Stream game logs from all VMs
cluster-ctl --vm-count 7 logs --follow

# Show last 50 lines initially
cluster-ctl --vm-count 7 logs --follow -n 50
```

Each VM gets a different color prefix. `Ctrl+C` to stop.

### Screenshots

```bash
cluster-ctl --vm-count 7 screenshot
cluster-ctl --vm-count 7 screenshot -o ./captures
```

Requires `grim` installed in the VMs and a Wayland compositor running.
