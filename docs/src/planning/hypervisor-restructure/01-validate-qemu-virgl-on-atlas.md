# Phase 01 — Validate QEMU virgl headless on atlas

> **Recommended Codex model: GPT 5.5 high**
>
> Frontier-complexity empirical work: atlas's GPU stack is unknown
> from this session and the answer determines whether the rest of
> the plan stays as designed or has to fall back. The agent has to
> diagnose virgl/Venus/Vulkan failures (which can present as kernel
> oopses, vhost-user socket errors, EGL init failures, or silent
> guest hangs), then decide whether a fallback shape (llvmpipe,
> `virtio-gpu-gl-pci,blob=on`, GPU passthrough) is acceptable or
> whether to escalate. `medium` would pick the first thing that
> compiles; `max` is overkill for what's still a contained probe.

## Risk profile

- **F1**: Atlas's GPU is NVIDIA proprietary, virgl initialises but
  Mesa Venus refuses to bind to it. Symptom: guest `vulkaninfo`
  shows zero physical devices. Cause: Venus needs Mesa-based host
  drivers. Recovery: try `virtio-gpu-gl-pci,blob=on` (uses dGPU
  through DRM render nodes) or fall back to llvmpipe (software).
- **F2**: QEMU starts but virgl fails because no host display
  server is running. Symptom: `qemu-system-x86_64: virtio-vga-gl
  requires `-display gtk,gl=on`` or similar. Cause: virtio-vga-gl
  binds to host EGL via the display backend. Recovery: use
  `virtio-gpu-gl-pci` (the render-node variant) instead.
- **F3**: QEMU initialises virgl but the guest kernel doesn't have
  `CONFIG_DRM_VIRTIO_GPU=y` or the Venus ICD isn't installed.
  Symptom: guest dmesg shows `virtio_gpu: probe failed` or
  vulkaninfo shows no Vulkan device. Recovery: NixOS guest module
  in `nix/vm-graphics.nix` already pulls vulkan-loader + Mesa; check
  the ICD path.
- **F4**: virgl works for one frame and crashes (the original
  vm-2-style EFAULT but on QEMU). Less likely on QEMU than crosvm,
  but possible. Recovery: capture the crash mode and re-evaluate.

## Strategy

This phase commits **nothing to the steampipe repo**. The output is
a typed verdict that gates Phase 08.

Verdicts:

- **GREEN** — `-display none -device virtio-vga-gl` works on atlas
  for at least one full Steam GUI cold-start cycle. Phase 08 ships
  exactly as designed.
- **YELLOW** — Some `virtio-gpu*` variant works but the exact device
  string differs from the design. Phase 08's `qemu.extraArgs` need
  the corrected string; nothing else changes.
- **RED** — No `virtio-gpu*` variant survives a Steam GUI cold-start.
  Phase 08 cannot ship as designed; the plan needs to either keep
  crosvm for graphical (which defeats the purpose) or pursue a
  bigger fallback (GPU passthrough, vGPU). Open a follow-up
  research note and pause the plan.

## Rollback drill

There's nothing to roll back — this phase doesn't change anything
persistent. If the test QEMU process gets wedged, `pkill
qemu-system-x86_64`. SLA: instant.

## Working tree

Atlas (`atlas.candee.baby` or wherever you SSH in). No edits to
the steampipe repo, but it's convenient to run this from the
steampipe worktree on atlas because you need `nix` and `microvm.nix`
attributes available.

## Goal

Produce a typed verdict (GREEN / YELLOW / RED) on whether QEMU can
run a guest with virgl-accelerated virtio-gpu *without* a host
display server, on atlas. If YELLOW, identify the working device
string. Record findings in
`docs/src/planning/hypervisor-restructure/01-validate-qemu-virgl-on-atlas-results.md`.

## Why this matters now

Phase 08 (flip login default to QEMU) and the entire Wave 1+ slice
of this plan rest on this empirical claim:

> QEMU + virgl + Venus boots on atlas through
> `microvm.qemu.extraArgs = [ "-display" "none" "-device"
> "virtio-vga-gl" ... ]`, survives Steam GUI cold-start, and runs
> wayvnc inside the guest.

The dossier ([hypervisor-restructure-research.md "B1"](../hypervisor-restructure-research.md))
flagged this as the highest-uncertainty blocker. Failing fast here
saves the rest of the plan from being designed around a wrong
assumption.

The vm-2 crash that triggered this whole restructure is the
reference symptom — the failure mode the new graphical path must
demonstrably avoid:

```
2026-05-26T08:29:47 rutabaga_gfx::virgl_renderer] GLSL feature level 430
2026-05-26T08:29:47 crosvm::crosvm::sys::linux::vcpu] vcpu hit unknown error: Bad address (os error 14)
2026-05-26T08:29:47 crosvm::crosvm::sys::linux] vcpu crashed
```

## Out of scope

- Editing any `.nix` or `.rs` file in steampipe.
- Editing canix.
- Touching `services.steampipe-cluster` defaults.
- Building a release artefact.
- Anything that requires a steampipe code commit.

## Plan

1. On atlas, identify the host GPU stack:

   ```bash
   lspci | grep -iE 'vga|3d'
   ls /dev/dri/
   readlink /run/opengl-driver
   nix-store -q --references /run/opengl-driver/lib | grep -iE 'mesa|nvidia'
   ```

   Record whether atlas uses Mesa-based drivers (i915, amdgpu, nouveau)
   or proprietary NVIDIA. Venus requires Mesa.

2. Build a throwaway microvm derivation that uses QEMU with the
   design's proposed args. Start from
   `nix/lib/microvm-runner.nix` but inline a one-off:

   ```nix
   # /tmp/qemu-virgl-probe.nix
   { pkgs, microvm, ... }: ... # minimal NixOS microvm:
   {
     microvm = {
       hypervisor = "qemu";
       graphics.enable = true;
       mem = 4096;
       vcpu = 1;
       interfaces = [ { type = "user"; id = "u0"; mac = "52:54:00:cb:99:01"; } ];
       volumes = [{ mountPoint = "/home/cluster"; image = "probe-home.img"; size = 2048; }];
       qemu.extraArgs = [
         "-display" "none"
         "-device" "virtio-vga-gl"
         "-object" "memory-backend-memfd,id=mem,size=4096M,share=on"
       ];
     };
     # vm-graphics.nix equivalent
     hardware.graphics.enable = true;
     environment.systemPackages = with pkgs; [
       vulkan-tools
       mesa-demos
       glmark2
     ];
   }
   ```

   `nix build` the runner, run it under `microvm-run`, and capture
   the console.

3. Inside the guest, verify in order:

   ```bash
   dmesg | grep -E 'virtio.gpu|drm'      # device probed
   ls /sys/class/drm/                     # cardX present
   vulkaninfo --summary                   # Venus enumerated?
   eglinfo | head -50                     # GL ICD?
   glmark2 -b build --run-forever &
   sleep 30; kill %1                      # no crash?
   ```

   If any of these fail, capture the exact error and the qemu
   stderr. Try the YELLOW fallbacks in order:

   - Replace `virtio-vga-gl` with
     `-device virtio-gpu-gl-pci,blob=on`.
   - Add `-display egl-headless` and re-test.
   - Force the Venus ICD path explicitly via
     `VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/virtio_icd.x86_64.json`
     in the guest.

4. Once a GREEN or YELLOW variant is identified, install
   `steam` inside the guest (or copy in `steamcmd`) and run a
   cold-start with the same command Phase 08 will use:

   ```bash
   steam -silent -login <test-account> <test-pass> -cef-disable-gpu
   ```

   Watch the QEMU console and host journal for ≥ 60 s. Expectation:
   no VCPU EFAULT, no crosvm-style `vcpu hit unknown error`, no
   silent guest hang. wayvnc should be reachable on guest port 5900
   over the user-mode net forward.

5. Write the verdict to
   `docs/src/planning/hypervisor-restructure/01-validate-qemu-virgl-on-atlas-results.md`
   in the steampipe repo. Format:

   ```markdown
   # Phase 01 results

   ## Verdict: GREEN | YELLOW | RED

   ## Atlas GPU stack
   <lspci output, driver name, /dev/dri/ contents>

   ## Working QEMU device string
   <exact -device line that survived Steam GUI cold-start, or "none"
   if RED>

   ## Failures observed during the probe
   <each fallback variant tried + the error it produced>

   ## Steam GUI cold-start trace
   <stdout/stderr from the steam -login invocation, redacted of
   credentials>

   ## Recommendation
   <if GREEN: "proceed with Phase 08 as designed">
   <if YELLOW: "Phase 08 qemu.extraArgs needs `<exact-list>`">
   <if RED: "pause plan; escalate to follow-up research dossier">
   ```

   Commit it on a branch like
   `phase-01-validate-qemu-virgl-on-atlas`.

## Acceptance criteria

- [ ] `docs/src/planning/hypervisor-restructure/01-validate-qemu-virgl-on-atlas-results.md`
      exists on the branch with a verdict (GREEN / YELLOW / RED).
- [ ] If GREEN or YELLOW: the file names the exact `-device` string
      that survived the steam cold-start.
- [ ] The verdict is justified by guest-side `vulkaninfo` and
      `glmark2` output, not just "qemu started OK".
- [ ] If RED: a follow-up plan or research note exists explaining
      what fallback path to investigate (passthrough? vGPU?
      software-only?).
- [ ] No file in `nix/`, `src/`, or `flake.nix` is modified by this
      phase.

## Files likely touched

- `docs/src/planning/hypervisor-restructure/01-validate-qemu-virgl-on-atlas-results.md`
  (new, on a `phase-01-...` branch). That is the only file this
  phase commits.

## Pitfalls

- **Testing without KVM**: `microvm.qemu` defaults to `qemu_kvm` on
  Linux, but if you run as a user without `/dev/kvm` access, QEMU
  falls back to TCG (software-only) which makes virgl meaningless
  and Steam GUI agonisingly slow. Verify `groups | grep -w kvm`.
- **The Steam test account**: don't use the production
  caniko_jarvis_N accounts for this probe — repeated cold-starts
  in different hypervisor configurations can trip Steam Guard. Use
  a throwaway, or skip the Steam step if a `vulkaninfo` + `glmark2`
  pair already proves virgl works.
- **wayvnc + user-net**: `microvm.interfaces.type = "user"`'s
  port-forward syntax in microvm.nix is the qemu user-net hostfwd
  string. To VNC in for visual confirmation, set
  `microvm.forwardPorts = [{ from = "host"; host.address =
  "127.0.0.1"; host.port = 5900; guest.address = ""; guest.port =
  5900; proto = "tcp"; }]`.
- **"Works on my desktop, not on atlas"**: the whole point is the
  atlas GPU. Do not declare GREEN based on a desktop probe.
- **Confounding crosvm leftovers**: if a crosvm patched-overlay
  build is in the binary cache, accidentally building the wrong
  attribute can give misleading signal. Verify the runner script
  is QEMU by `grep qemu-system runners/.../bin/microvm-run`.

## Failure modes and recoveries

- **F1 (Venus refuses NVIDIA)**: try `virtio-gpu-gl-pci,blob=on`
  + `egl-headless`. If atlas is NVIDIA-only, RED is the honest
  outcome — Venus is Mesa-only, the entire graphical-login
  restructure needs a different shape.
- **F2 (virgl wants display backend)**: documented above; try the
  render-node variant or `egl-headless`.
- **F3 (Venus ICD missing)**: confirm `nix/vm-graphics.nix` pulls
  `vulkan-loader` and the guest has
  `/run/opengl-driver/share/vulkan/icd.d/virtio_icd.x86_64.json`.
  If not, add `mesa.drivers` overlay attribute as part of this
  probe (don't merge yet; just confirm the missing piece).
- **F4 (delayed virgl crash)**: capture the QEMU stderr around the
  crash and the guest dmesg. Mark YELLOW if the variant survives
  Steam cold-start in another configuration; RED if every variant
  crashes.

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Empirical Dependencies", item 1).
- Research dossier:
  [../hypervisor-restructure-research.md](../hypervisor-restructure-research.md)
  ("Blockers And Missing Artifacts" B1, B4, B5).
- vm-2 crash log (the symptom we must not reproduce):
  `/home/can/.local/state/steampipe/default/vm-2/vm.log` tail.
- microvm.nix QEMU runner reference:
  `/nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/lib/runners/qemu.nix`.
