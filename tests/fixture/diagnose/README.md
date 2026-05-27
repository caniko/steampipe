# Fixture diagnostic ladder

Six-step probe of the in-tree fixture cluster's graphics path, classifying
failures into the buckets the visual-testing plan addresses.

## Quick start

The fixture shares `br-cluster` and the same `tap-vm-*` pool as the production
regicide cluster (declared by `services.steampipe-cluster` on atlas).
They cannot run simultaneously — bring regicide down first if active:

```bash
nix run /data/nvme0/can/Projects/solo/game-dev/regicide#cluster-tournament-down
```

If `br-cluster` is missing, deploy canix host config:

```bash
cd /data/nvme0/can/Projects/canix && canix deploy switch atlas
```

The diagnostic itself does not need `sudo` when `br-cluster` already exists.
Network setup remains a host prerequisite; run the fixture `net-up` wrapper
from an interactive sudo-capable shell only when the bridge is absent.

Then from the steampipe repo root:

```bash
just diag-fixture                       # foot-banner workload, teardown on exit
just diag-fixture-keep                  # same, but leave VM running
just diag-fixture --workload vkcube-frozen
just diag-fixture --no-vnc              # skip step 4 (when VNC is known broken)
```

Output lands in `tests/fixture/diagnose/_run-<UTC-timestamp>/` —
gitignored by default. The two files you care about:

- `summary.md` — human-readable verdict + per-step status.
- `verdict.json` — machine-readable, includes `leased_vm` plus a pointer to
  the relevant stable docs section for whichever bucket tripped.

## Verdicts

| Verdict | Exit | Meaning |
|---|---|---|
| `GREEN` | 0 | Everything works. |
| `VNC_CLIENT_BUG` | 11 | grim matches golden, VNC does not; inspect the VNC capture path. |
| `COMPOSITOR_NOT_READY` | 12 | Sway not painted or workload window absent. |
| `GPU_BROKEN` | 13 | DRI / virtio_gpu / Vulkan / vkcube failed. |
| `COMPOSITOR_REGRESSION` | 14 | Both captures diverge from golden — workload or compositor changed. |
| `MIXED` | 15 | Multiple categories tripped; see step results. |
| `WAYVNC_LOG_WARN` | 16 | Everything else OK, but wayvnc.log shows a known error signature. |
| `NO_GOLDEN` | 17 | Captures succeeded, no golden to compare to (run `golden/regenerate.sh`). |
| `CLASSIFIER_BROKEN` | 99 | The classifier crashed; step results are usable raw. |

## Verdict sanity-check (reproduction recipes)

The ladder's verdict mapping is only useful if each non-GREEN bucket is
actually reachable. To verify on a healthy fixture:

| Verdict | How to provoke |
|---|---|
| `COMPOSITOR_NOT_READY` | The leased IP varies. Read `verdict.json.leased_vm.ip`, then `ssh fixture@<that-ip> workload-stop` before invoking. |
| `GPU_BROKEN` | (Inside VM) `sudo modprobe -r virtio_gpu` — note this may freeze sway. |
| `NO_GOLDEN` | Remove `tests/fixture/golden/foot-banner-1280x720.png` and re-run. |
| `COMPOSITOR_REGRESSION` | Replace the golden PNG with an unrelated image. |
| `VNC_CLIENT_BUG` | Hardest to provoke artificially; compare the `03-grim.*` and `04-vnc.*` artifacts to isolate capture-side drift. |

## Steps

1. **`01-boot.sh`** — verify bridge present, SSH reachable, workload running.
2. **`02-compositor.sh`** — sway IPC: at least one active output, workload window in tree.
3. **`03-grim.sh`** — capture with `grim` inside the VM; compare to golden.
4. **`04-vnc.sh`** — capture via `cluster-ctl screenshot --screenshot-backend vnc`; compare to golden.
5. **`05-gpu.sh`** — DRI nodes + `virtio_gpu` module + `vulkaninfo --summary` + `vkcube --c 1` + `wayland-info`.
6. **`06-wayvnc-log.sh`** — tail `wayvnc.log`, search for known error signatures.

Each step writes a `<name>.exit` file containing its exit code, plus raw
artefacts under the same prefix (`.log`, `.json`, `.png`, `.txt`).
`07-classify.py` reads the `.exit` files and emits the verdict.
`01-boot.sh` also records the active slot in `leased_vm.id`, `leased_vm.name`,
and `leased_vm.ip`, and `verdict.json` mirrors that metadata.

## Hash comparison

Step 3 and 4 normalise PNG content via PIL (`Image.open(...).convert("RGBA").tobytes()`)
and SHA-256 the raw bytes. This bypasses PNG metadata variance (creation
timestamp, encoder version) but is *exact-pixel* equality, which is
fragile across mesa version bumps. Until vnc-visual-testing Phase E
lands its SSIM comparator, expect occasional `COMPOSITOR_REGRESSION`
false positives — re-bless the golden if the diff is visually
acceptable.

## Example run

`example-run/` contains a committed sample of a healthy `GREEN` run.
That directory is *not* gitignored (only `_run-*` is), so it serves as
a contract for what good output looks like.
