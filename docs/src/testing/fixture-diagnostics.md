# Fixture diagnostics

The fixture diagnostic ladder probes the in-tree fixture cluster's graphics and
capture path. It exists for contributors who need to decide whether a failure is
in the VM boot path, sway readiness, GPU/Vulkan setup, grim capture, wayvnc
capture, or the committed golden image.

Run it from the repository root:

```bash
just diag-fixture
just diag-fixture-keep
just diag-fixture --workload vkcube-frozen
just diag-fixture --no-vnc
```

Output is written under `tests/fixture/diagnose/_run-<UTC-timestamp>/`.
`summary.md` is the human-readable report, and `verdict.json` is the
machine-readable result. `verdict.json.leased_vm` records the VM slot and IP
that were used.

## Verdicts

| Verdict | Exit | Contributor action |
| --- | ---: | --- |
| `GREEN` | 0 | The fixture graphics stack, grim capture, VNC capture, and golden comparison all passed. |
| `VNC_CLIENT_BUG` | 11 | Grim matches the golden while VNC differs. Inspect `04-vnc.*` artifacts and the VNC capture path. |
| `COMPOSITOR_NOT_READY` | 12 | Sway did not expose an active output or the workload window was absent. Inspect `02-compositor.*` and the guest compositor logs. |
| `GPU_BROKEN` | 13 | DRI, `virtio_gpu`, Vulkan, `vkcube`, or `wayland-info` failed. Inspect `05-gpu.*` first. |
| `COMPOSITOR_REGRESSION` | 14 | Grim and VNC both differ from the golden. The workload, Mesa stack, compositor, or golden fixture changed. |
| `MIXED` | 15 | Multiple buckets failed. Start with the earliest failed step. |
| `WAYVNC_LOG_WARN` | 16 | Capture passed, but `wayvnc.log` contains a known warning signature. Inspect `06-wayvnc-log.*`. |
| `NO_GOLDEN` | 17 | Capture succeeded but no committed golden exists. Regenerate the fixture golden before treating this as a failure. |
| `CLASSIFIER_BROKEN` | 99 | The classifier crashed. The step artifacts are still useful. |

## Steps

1. `01-boot.sh` verifies bridge presence, SSH reachability, and workload
   startup.
2. `02-compositor.sh` checks sway IPC for at least one active output and a
   workload window in the tree.
3. `03-grim.sh` captures with `grim` inside the VM and compares to the golden.
4. `04-vnc.sh` captures through `cluster-ctl screenshot --screenshot-backend
   vnc` and compares to the golden.
5. `05-gpu.sh` checks DRI nodes, `virtio_gpu`, `vulkaninfo --summary`,
   `vkcube --c 1`, and `wayland-info`.
6. `06-wayvnc-log.sh` scans `wayvnc.log` for known error signatures.
7. `07-classify.py` reads the step exit files and emits `summary.md` plus
   `verdict.json`.

## Reproduction notes

The verdict mapping is only useful when each non-green bucket is reachable on a
healthy fixture:

| Verdict | How to provoke |
| --- | --- |
| `COMPOSITOR_NOT_READY` | Read `verdict.json.leased_vm.ip`, then stop the workload inside that VM before invoking the compositor step. |
| `GPU_BROKEN` | Inside the VM, remove or break `virtio_gpu`; this may freeze sway. |
| `NO_GOLDEN` | Move `tests/fixture/golden/foot-banner-1280x720.png` out of the way and re-run. |
| `COMPOSITOR_REGRESSION` | Replace the golden PNG with an unrelated image. |
| `VNC_CLIENT_BUG` | Hard to provoke artificially; use the `03-grim.*` and `04-vnc.*` artifacts to isolate capture-side drift. |

Steps 3 and 4 normalize PNGs through RGBA bytes before hashing. This avoids PNG
metadata noise, but the comparison is still exact-pixel equality and can drift
across Mesa, compositor, or font changes. When that happens, inspect the
captured image before blessing a new golden.
