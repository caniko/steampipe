# Stage-1 smoke status

Captured on 2026-05-11 from `/data/nvme0/can/Projects/solo/game-dev/regicide`
using:

```sh
nix run --override-input steampipe /data/nvme0/can/Projects/steampipe .#cluster-1v1-uifull-stage1-up
nix run --override-input steampipe /data/nvme0/can/Projects/steampipe .#cluster-1v1-uifull-stage1-test -- --max-runs 1 --timeout 600
```

Result: VM booted, SSH reached ready, Weston reached ready, and the host and VM
connected. The game smoke failed after the VM-side client panicked in Bevy
surface creation:

```text
Fallback system failed to choose present mode. Mode: Fifo, Options: []
```

The same failure reproduced with the scripted-initrd `uifull` flavor:

```sh
nix run --override-input steampipe /data/nvme0/can/Projects/steampipe .#cluster-1v1-uifull-up
nix run --override-input steampipe /data/nvme0/can/Projects/steampipe .#cluster-1v1-uifull-test -- --max-runs 1 --timeout 600
```

That makes the UI-full game-smoke failure non-diagnostic for the stage-1
migration until the downstream rendering runtime issue is fixed.

The default headless `cluster-1v1-test` also failed in the downstream runtime,
after successful VM boot and deployment, with:

```text
NO_PROGRESS_TIMEOUT (local: 602s without semantic progress > 600s)
Unable to find a GPU!
```

Conclusion: keep the crosvm systemd-stage-1 default off. The E2 scaffold and
single-VM boot are validated, but E5 is blocked until the regicide smoke matrix
is green.
