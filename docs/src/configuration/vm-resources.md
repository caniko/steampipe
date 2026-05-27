# VM resources

This page documents how steampipe sizes per-VM memory and disk envelopes, the
host-side primitives that keep them honest, and the operator tools for
inspection and recovery.

## Memory contract

Each VM is sized with:

| VM class | Hypervisor class | mem (MiB) | initial balloon (MiB) | balloon | deflate-on-OOM |
| --- | --- | ---: | ---: | --- | --- |
| Login (graphical, Steam GUI) | QEMU | 4096 | 0 | yes | yes |
| Login (cloud-hypervisor escape hatch) | cloud-hypervisor | 4096 | 1024 | yes | yes |
| Runtime (headless) | cloud-hypervisor | 2048 | 512 | yes | yes |

The pinned microvm.nix QEMU runner rejects non-zero
`microvm.initialBalloonMem`, so QEMU login runners enable virtio-balloon and
`deflateOnOOM` without an initial claim. cloud-hypervisor supports an initial
balloon size, so runtime runners start with a 512 MiB claim and any
cloud-hypervisor login runner starts with a 1024 MiB claim.

The `crosvm` escape hatch does not receive this memory contract. Its runner in
the pinned microvm.nix does not support `initialBalloonMem`, and crosvm remains
deprecated for the graphical path.

## Why Ballooning

Balloon + `deflateOnOOM` lets the host reclaim idle pages while giving memory
back to the guest under pressure before the guest OOM-killer fires. This keeps
the reliability guarantee while avoiding the extra failure surface of
virtio-mem hotplug.

## Host Requirement

Enable `hardware.ksm.enable = true;` on the host in canix. KSM deduplicates
identical pages across similar guests, including Mesa, libc, and kernel image
pages. Without KSM the cluster still runs, but resident memory is higher than
the budget assumes.

## Disk

- Login-runner home image: 2 GiB. Holds residual home config and login state.
- Runtime-runner home image: 8 GiB. Holds game content the harness deploys.

Existing home images do not shrink retroactively. Use the reset helper to
recreate old images at the current runner default size.

## Operator Tools

### `cluster-ctl reset <vm> --home`

Removes the on-disk home image for `<vm>` so the next boot starts fresh. The
command refuses to remove the image while the VM slot has a live lease.

Example:

```bash
cluster-ctl --vm-count 7 reset vm-1 --home
```

This deletes
`$XDG_STATE_HOME/steampipe/<cluster>/vm-1/vm-1-home.img`; the next boot
recreates it from the runner's current default size.

### `cluster-ctl admission status`

Prints the host-side memory admission gate's current sample plus configured
high/low watermarks. The gate reads `/proc/meminfo` before starting a VM,
blocks new admissions when available host memory falls below the high
watermark, and resumes when it rises above the low watermark.

Example:

```bash
cluster-ctl admission status
```

If `/proc/meminfo` cannot be read, the gate disengages by design and VM startup
continues without throttling. The gate is defense in depth, not a hard ceiling.

## Failure Modes

- **Guest OOM**: balloon deflates first; if memory pressure persists, the
  guest's OOM-killer fires. This is visible in `vm.log` as
  `Out of memory: Killed process ...`.
- **Host OOM**: parallel VM startup can overshoot host memory and cause the
  host OOM-killer to claim a hypervisor process. The memory-admission gate is
  the primary defense.
- **KSM disabled**: the budget's host-reclaim assumption is overstated. The
  cluster uses more resident memory than projected.

## See Also

- [VM lifecycle](./vm-lifecycle.md)
- [Graphics and games inside VMs](./graphics-and-games.md)
