# Graphics and games inside VMs

This page documents which graphical workloads work inside steampipe VMs, which
ones are deliberately software-rendered, and where game rendering is still a
known limitation.

## Summary

| VM class | What runs | GPU needed? | Default hypervisor |
| --- | --- | --- | --- |
| `services.steampipe-cluster.loginRunners` | Steam login wizard (CEF/GTK) | No, uses llvmpipe | QEMU without virtio-gpu |
| `services.steampipe-cluster.runners` | `cluster-ctl steam warm` (`DisplayMode::Headless`) | No, no compositor | cloud-hypervisor |
| `mkTestCluster` headless flavours | Project test binary, server, simulation, or headless game | No | cloud-hypervisor |
| `mkTestCluster` graphical flavours | Project test binary with rendered output, UI, visual fixtures, or games | Yes | crosvm, deprecated |

## Host-module classes: software rendering

The host-module classes are the NixOS module paths:

- `services.steampipe-cluster.loginRunners`
- `services.steampipe-cluster.runners`

They do not run games. Login runners exist for the Steam login wizard, and
runtime runners exist for warm/check flows. Both paths are optimized for
reliability rather than GPU throughput.

The Steam login wizard is CEF + GTK. It renders on Mesa llvmpipe without a GPU
device. `sway` runs with `WLR_BACKENDS=headless`, so there is no DRM device and
no virtio-gpu. `wayvnc` captures the headless output through wlroots
screencopy.

This is intentional after the 2026-05-26 hypervisor restructure. The QEMU
virgl/Venus stack proved unstable on AMD Mesa under real Steam GUI load; the
probe rejected `-display none` with GL devices before guest boot, and the
`egl-headless` Venus fallback segfaulted QEMU when the next render workload
started.
Removing the graphics device from host-module classes removes that crash
surface.

The trade-off is frame rate. The Steam login UI may render at about 5-10 FPS
instead of 60 FPS. That is acceptable for a one-time login wizard and keeps the
path predictable.

## Project-flake graphical flavours: crosvm (deprecated)

Project flakes that need GPU-rendered workloads inside the VM, such as
chessbender card-game UI tests, regicide visual fixtures, or fragpipe render
harnesses, currently pin:

```toml
[cluster]
hypervisor = "crosvm"
```

That selects steampipe's patched crosvm overlay. The overlay:

- Installs crosvm's source seccomp policies and pre-compiles them with the
  same `compile_seccomp_policy.py` that crosvm itself uses.
- Widens `common_device.policy` and `gpu_common.policy` to allow
  `PR_SET_NAME`, used by Rust thread naming on glibc >= 2.40, and
  `MADV_GUARD_INSTALL`, used by kernel >= 6.13 stack guard pages.
- Adds the `gfxstream` and `aemu` inputs needed for Mesa Venus.

Without those patches, plain `nixpkgs.crosvm` can SIGSYS-crash on modern hosts
with kernel >= 6.13 and glibc >= 2.40.

## Known limitation: heavy Vulkan loads

crosvm + virgl + Vulkan is empirically brittle under sustained load. The
original vm-2 incident on 2026-05-26 failed with `vcpu hit unknown error: Bad
address` 79 ms after Steam reached for `GLSL feature level 430`.

For card-game-class workloads, such as lightweight 2D scenes, UI fixtures, and
small visual tests, the crosvm path is usable. For heavy 3D or sustained Vulkan
loads, expect occasional VCPU EFAULT crashes.

Mitigation is retrying the failed test. `cluster-ctl` lease and reset handling
is designed to tolerate repeated VM teardown and rebuild cycles.

## Deprecation horizon

`[cluster] hypervisor = "crosvm"` is deprecated. The patched overlay stays
maintained through the 2026-Q4 nixpkgs window so existing project-flake
graphical workloads keep a working escape hatch while upstream graphics stacks
settle.

The project will re-evaluate the graphical path after upstream stabilisation
of:

- virglrenderer + Mesa Venus on AMD, where the Phase 01 probe saw failures in
  `virgl_render_server`.
- QEMU 11.x + Mesa 27.x + virgl Venus on headless hosts without a display
  backend.

That future evaluation may consider QEMU + Venus, GPU passthrough, or another
shape. This page does not pick that successor.

## What if I need heavy 3D now?

There are three current options, none ideal:

1. Accept occasional crashes and retry. crosvm-virgl survives most
   card-game-class workloads; heavy 3D may fail in some runs.
2. Use manual GPU passthrough outside steampipe's provided runner model. This
   gives one VM direct access to a device and loses the normal multi-VM cluster
   pattern.
3. Use software rendering by setting `graphics = false` in the project's
   `steampipe.toml`. The game falls back to llvmpipe. Frame rate drops sharply,
   which is usually unsuitable for interactive 3D but can be fine for headless
   game-logic tests.

## See also

- [VM resources](./vm-resources.md)
- [Visual testing](./visual-testing.md)
