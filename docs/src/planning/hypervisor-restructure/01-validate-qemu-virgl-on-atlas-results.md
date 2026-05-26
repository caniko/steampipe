# Phase 01 results

## Verdict: RED

No QEMU `virtio-gpu*` configuration tested on atlas survived the required Vulkan and render-workload gates. The proposed Phase 08 shape must not ship as designed.

`-display none -device virtio-vga-gl` fails before guest boot because QEMU's `none` display backend has no OpenGL support. `-display egl-headless -device virtio-gpu-gl-pci,blob=on` boots and exposes virgl OpenGL, but guest `vulkaninfo --summary` enumerates no Vulkan physical devices. Enabling Venus with `venus=on,hostmem=512M` allows guest Vulkan enumeration only when the QEMU process inherits host Vulkan loader/ICD paths, but QEMU then segfaults when the next render workload starts.

Steam cold-start was not run because no variant passed the lower Vulkan/render gates, and no throwaway Steam account credentials were available for this probe.

## Atlas GPU stack

Host: `atlas`

```text
$ nix shell nixpkgs#pciutils -c lspci | grep -iE 'vga|3d'
03:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M] (rev c8)
7d:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Granite Ridge [Radeon Graphics] (rev c9)

$ ls -l /dev/dri/
card0
card1
renderD128
renderD129

$ ls -l /dev/dri/by-path
pci-0000:03:00.0-card -> ../card1
pci-0000:03:00.0-render -> ../renderD128
pci-0000:7d:00.0-card -> ../card0
pci-0000:7d:00.0-render -> ../renderD129

$ readlink -f /run/opengl-driver
/nix/store/9m2v3xj68l8nmh3spqn9894lhhchqm9c-graphics-drivers

$ nix-store -q --references "$(readlink -f /run/opengl-driver)" | grep -iE 'mesa|nvidia'
/nix/store/bfkyl4qgipm1vpfls2cqr3kv0afgpbya-mesa-26.1.1
```

Atlas is Mesa/AMDGPU based, not proprietary NVIDIA. The expected host-side Vulkan ICD exists at `/run/opengl-driver/share/vulkan/icd.d/radeon_icd.x86_64.json`, and the guest-visible virtio ICD exists at `/run/opengl-driver/share/vulkan/icd.d/virtio_icd.x86_64.json`.

KVM was usable for the probe:

```text
$ ls -l /dev/kvm
crw-rw-rw- 1 root kvm 10, 232 May 26 13:24 /dev/kvm
```

## Working QEMU device string

None.

The closest partial configuration was:

```text
-display egl-headless,rendernode=/dev/dri/renderD128
-device virtio-gpu-gl-pci,blob=on,venus=on,hostmem=512M
```

It enumerated Venus devices in the guest only when QEMU was launched with:

```text
LD_LIBRARY_PATH=/run/opengl-driver/lib
VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/radeon_icd.x86_64.json
```

That configuration is not acceptable because QEMU segfaulted when a render workload started after Vulkan enumeration.

## Failures observed during the probe

### Proposed design args

```text
microvm.graphics.enable = true
qemu.extraArgs = [
  "-display" "none"
  "-device" "virtio-vga-gl"
  "-object" "memory-backend-memfd,id=probe-mem,size=4096M,share=on"
]
```

Effective runner contained microvm.nix's default graphical args before the extra args:

```text
-display gtk,gl=on -device virtio-vga-gl ... -display none -device virtio-vga-gl
```

QEMU failed before guest boot:

```text
microvm@qemu-virgl-probe: -device virtio-vga-gl: The display backend does not have OpenGL support enabled
It can be enabled with '-display BACKEND,gl=on' where BACKEND is the name of the display backend to use.
```

### Clean design args with graphics-capable QEMU

```text
-display none
-device virtio-vga-gl
```

QEMU failed before guest boot:

```text
microvm@probe-none-vga-pcie: -device virtio-vga-gl: The display backend does not have OpenGL support enabled
It can be enabled with '-display BACKEND,gl=on' where BACKEND is the name of the display backend to use.
```

### `virtio-gpu-gl-pci,blob=on` with `-display none`

```text
-display none
-device virtio-gpu-gl-pci,blob=on
```

QEMU failed before guest boot:

```text
microvm@probe-none-gpu-blob: -device virtio-gpu-gl-pci,blob=on: The display backend does not have OpenGL support enabled
It can be enabled with '-display BACKEND,gl=on' where BACKEND is the name of the display backend to use.
```

### `egl-headless` GL-only variant

```text
-display egl-headless
-device virtio-gpu-gl-pci,blob=on
```

Guest DRM and OpenGL were present:

```text
[drm] pci: virtio-gpu-pci detected at 0000:00:01.0
[drm] features: +virgl +edid +resource_blob -host_visible
[drm] number of cap sets: 2
[drm] Initialized virtio_gpu 0.1.0 for 0000:00:01.0 on minor 0

EGL driver name: virtio_gpu
OpenGL core profile renderer: virgl (AMD Ryzen 9 9950X3D 16-Core Processor (radeonsi, rap...)
```

Guest Vulkan failed:

```text
$ vulkaninfo --summary
'DISPLAY' environment variable not set... skipping surface info
ERROR: [Loader Message] Code 0 : setup_loader_term_phys_devs:  Failed to detect any valid GPUs in the current config
ERROR at /build/source/vulkaninfo/./vulkaninfo.h:249:vkEnumeratePhysicalDevices failed with ERROR_INITIALIZATION_FAILED
```

`glmark2-es2-drm` ran long enough to prove virgl GL command processing, but it repeatedly reported KMS mode-setting failures:

```text
GL_VENDOR:      Mesa
GL_RENDERER:    virgl (AMD Ryzen 9 9950X3D 16-Core Processor (radeonsi, rap...)
GL_VERSION:     OpenGL ES 3.2 Mesa 26.0.2
[build] <default>: Error: Failed to set crtc: -22
FPS: 1700 FrameTime: 0.588 ms
```

### Venus-enabled variant without host Vulkan environment

```text
-display egl-headless
-device virtio-gpu-gl-pci,blob=on,venus=on,hostmem=512M
```

Guest saw the expected host-visible window and Venus cap set:

```text
[drm] Host memory window: 0xc00000000000 +0x20000000
[drm] features: +virgl +edid +resource_blob +host_visible
[drm] number of cap sets: 3
[drm] cap set 2: id 4, max-version 0, max-size 160
```

But guest Vulkan failed because the host virgl render server could not open `libvulkan.so`:

```text
virgl_render_server: vkr: failed to open libvulkan: libvulkan.so: cannot open shared object file: No such file or directory
virgl_render_server: failed to dispatch context op 1
ERROR at /build/source/vulkaninfo/./vulkaninfo.h:613:vkCreateInstance failed with ERROR_OUT_OF_HOST_MEMORY
```

### Venus-enabled variant with host Vulkan environment

```text
LD_LIBRARY_PATH=/run/opengl-driver/lib
VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/radeon_icd.x86_64.json

-display egl-headless
-device virtio-gpu-gl-pci,blob=on,venus=on,hostmem=512M
```

Guest Vulkan enumerated physical devices:

```text
Vulkan Instance Version: 1.4.341

GPU0:
  deviceName = Virtio-GPU Venus (AMD Radeon RX 7900 XTX (RADV NAVI31))
  driverName = venus

GPU1:
  deviceName = Virtio-GPU Venus (AMD Ryzen 9 9950X3D 16-Core Processor (RADV RAPHAEL_MENDOCINO))
  driverName = venus
```

QEMU then segfaulted during the next render probe:

```text
atlas kernel: .qemu-system-x8[1416308]: segfault at 0 ip ... in libc.so.6
atlas systemd-coredump: Process 1416300 (.qemu-system-x8) of user 1000 dumped core.
```

Repeating with the display pinned to the discrete render node reproduced the segfault when `glmark2-es2-drm` started:

```text
LD_LIBRARY_PATH=/run/opengl-driver/lib
VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/radeon_icd.x86_64.json

-display egl-headless,rendernode=/dev/dri/renderD128
-device virtio-gpu-gl-pci,blob=on,venus=on,hostmem=512M
```

Guest Vulkan again enumerated Venus devices, then host journal recorded:

```text
atlas kernel: .qemu-system-x8[1423200]: segfault at 0 ip ... in libc.so.6
atlas systemd-coredump: Process 1423192 (.qemu-system-x8) of user 1000 dumped core.
```

No `vcpu hit unknown error: Bad address` string appeared, but the QEMU process still crashed under the Vulkan/GL workload gate.

## Steam GUI cold-start trace

Not run.

Reasons:

- No tested configuration passed the required guest-side `vulkaninfo` plus render-workload gate.
- The only configuration that enumerated Venus required extra host environment and then crashed QEMU before Steam.
- No throwaway Steam test account credentials were supplied; using production credentials was explicitly out of bounds for this phase.

## Recommendation

Pause the Phase 08 QEMU graphical-login plan. Do not flip the graphical login default to QEMU with the current `microvm.qemu.extraArgs` design.

Follow-up research should investigate, in order:

1. Whether QEMU 10.2.1 plus virglrenderer/Venus has a known crash with `virtio-gpu-gl-pci,venus=on,hostmem=...` on AMD hosts under `egl-headless`.
2. Whether microvm.nix can wrap QEMU with the required host Vulkan runtime environment without relying on ambient `LD_LIBRARY_PATH`.
3. Whether a non-Venus virgl/GL path is sufficient for the actual Steam + wayvnc workload. Current evidence says it is not sufficient for the stated Vulkan-first Phase 08 claim.
4. If Venus remains unstable, evaluate larger fallbacks: GPU passthrough, mediated/vGPU where available, or a software-only llvmpipe path with explicit performance expectations.
