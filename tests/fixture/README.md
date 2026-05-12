# In-tree cluster fixture

This fixture is a minimal, in-repo consumer of `steampipe.lib.mkTestCluster`.
It exists to boot a known-good graphics-enabled microVM from this repository
alone, without depending on a downstream game checkout, Steam credentials, or
deployment payloads.

What it is:

- one-VM cluster smoke bed for `cluster-ctl`
- reserves one free VM from the fixture pool (`vm-1` through `vm-8`)
- `uifull` flavor for crosvm-based UI parity checks

What it is not:

- no game build or deploy target
- no Steam login flow
- not a replacement for downstream scene scripting or golden capture

## Boot

Bring the bridge up first:

```sh
sudo nix run ./tests/fixture#cluster-fixture-net-up
```

Boot the default fixture VM:

```sh
nix run ./tests/fixture#cluster-1v1-up
```

Boot the higher-memory UI-full flavor instead:

```sh
nix run ./tests/fixture#cluster-1v1-uifull-up
```

Check status with the repo binary:

```sh
cargo run --release -- --project-root tests/fixture --vm-count 1 status
```

`up` prints the actual lease, for example `Reserved: vm-4`. `status` then shows
the same slot and IP, which may vary run to run depending on what other
clusters currently hold.

SSH directly:

```sh
VM_IP="$(cargo run --release -- --project-root tests/fixture --vm-count 1 status \
  | awk '/^vm-[0-9]+/ { print $2; exit }')"
ssh -i tests/fixture/cluster_key \
  -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null \
  "fixture@$VM_IP"
```

## Teardown

```sh
cargo run --release -- --project-root tests/fixture --vm-count 1 down
sudo cargo run --release -- --project-root tests/fixture --vm-count 1 net-down
```

Cluster state lives under `$XDG_STATE_HOME/steampipe/fixture/` by default.
Runner store paths are baked into the flake apps; inspect them with
`nix build ./tests/fixture#cluster-1v1-up.program --no-link --print-out-paths`.

## Workloads

The fixture VM now carries three deterministic graphical workloads plus three
guest-side control commands:

- `workload-run NAME`
- `workload-stop`
- `workload-current`

Supported workload names:

- `foot-banner`:
  title/app-id `workload-foot-banner`, fullscreen `foot`, fixed ASCII banner
- `vkcube-frozen`:
  `vkcube` on Wayland at `1280x720`, relaunched in a tight loop with `--c 1`
  because the one-frame surface is destroyed on exit
- `wgpu-checker`:
  title `workload-wgpu-checker`, fixed `16x16` checkerboard rendered through
  `wgpu`/Wayland/Vulkan at `1280x720`

Once the fixture bridge and VM are up:

```sh
VM_IP="$(cargo run --release -- --project-root tests/fixture --vm-count 1 status \
  | awk '/^vm-[0-9]+/ { print $2; exit }')"

ssh -i tests/fixture/cluster_key \
  -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null \
  "fixture@$VM_IP" 'workload-run foot-banner'

ssh -i tests/fixture/cluster_key \
  -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null \
  "fixture@$VM_IP" 'workload-current'

ssh -i tests/fixture/cluster_key \
  -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null \
  "fixture@$VM_IP" 'export XDG_RUNTIME_DIR=/tmp/runtime-fixture WAYLAND_DISPLAY=wayland-1; swaymsg -t get_tree | grep -E "\"(title|app_id)\""'

ssh -i tests/fixture/cluster_key \
  -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null \
  "fixture@$VM_IP" 'workload-stop'
```

Smoke each workload:

```sh
VM_IP="$(cargo run --release -- --project-root tests/fixture --vm-count 1 status \
  | awk '/^vm-[0-9]+/ { print $2; exit }')"
ssh -i tests/fixture/cluster_key "fixture@$VM_IP" 'workload-run foot-banner'
ssh -i tests/fixture/cluster_key "fixture@$VM_IP" 'workload-run vkcube-frozen'
ssh -i tests/fixture/cluster_key "fixture@$VM_IP" 'workload-run wgpu-checker'
```

Determinism check for `foot-banner` on the same boot:

```sh
VM_IP="$(cargo run --release -- --project-root tests/fixture --vm-count 1 status \
  | awk '/^vm-[0-9]+/ { print $2; exit }')"
ssh -i tests/fixture/cluster_key "fixture@$VM_IP" '
  export XDG_RUNTIME_DIR=/tmp/runtime-fixture WAYLAND_DISPLAY=wayland-1
  workload-run foot-banner
  grim /tmp/foot-a.png
  grim /tmp/foot-b.png
  pngcrush -q -ow -rem alla /tmp/foot-a.png
  pngcrush -q -ow -rem alla /tmp/foot-b.png
  sha256sum /tmp/foot-a.png /tmp/foot-b.png
'
```

The workload metadata that matters for later scene scripting:

- `foot-banner`: title `workload-foot-banner`, app-id `workload-foot-banner`,
  resolution `1280x720`
- `vkcube-frozen`: app-id `vkcube`, resolution `1280x720`
- `wgpu-checker`: title `workload-wgpu-checker`, resolution `1280x720`

Expected image hashes are intentionally not recorded here until they are
captured from a privileged host run that can actually boot the fixture VM and
verify the pixels end to end.

## Golden images

Golden images live under `tests/fixture/golden/` and exist to anchor later
visual assertions against known-good frames from this fixture VM. They are not
edited in place.

Regenerate candidates only:

```sh
cargo run --features fixture-tools -- fixture golden regenerate
```

That command writes fresh captures to `tests/fixture/golden/.candidate/`, never
directly over the committed PNGs. Review every candidate visually before
promotion:

- confirm the file opens cleanly
- confirm the image is exactly `1280x720`
- confirm the expected workload content is actually visible
- confirm there is no cursor or partial-frame artifact in the capture

After review, promote the candidate PNGs manually, update
`tests/fixture/golden/MANIFEST.toml` with the new sha256 values and capture
metadata, and then validate the blessed set:

```sh
cargo run --features fixture-tools -- fixture golden verify
```

If a golden legitimately changes, the required workflow is:

1. run `cargo run --features fixture-tools -- fixture golden regenerate`
2. review the new candidate images by eye
3. replace the committed PNGs deliberately
4. update `MANIFEST.toml`
5. run `cargo run --features fixture-tools -- fixture golden verify`

Direct edits to the committed PNGs are forbidden because they bypass both the
capture path and the review step.

The legacy `tests/fixture/golden/regenerate.sh` and
`tests/fixture/golden/golden-verify.sh` entrypoints are compatibility shims
around the same feature-gated Rust commands.

## Flavors

- `default`: cloud-hypervisor, `microvm.graphics.enable = true`, base graphics
  tooling from `cluster-vm-base` and `vm-graphics`
- `uifull`: crosvm, 4 GiB RAM, Vulkan-first virtio-gpu path, scripted initrd
  (`use_systemd_stage1 = false`) to match the current downstream UI-full setup

## Test key disclaimer

`cluster_key` is committed intentionally. It authorizes SSH only to ephemeral
VMs on the host-private fixture bridge and carries no real-world access. The
flake wrappers copy it into a temporary runtime project root with mode `0600`
so OpenSSH will accept it.
