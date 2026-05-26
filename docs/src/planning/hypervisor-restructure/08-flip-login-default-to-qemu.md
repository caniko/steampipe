# Phase 08 — Flip host-module defaults: QEMU (no-graphics) for login, cloud-hypervisor for runners

> **Recommended Codex model: GPT 5.5 medium**
>
> Three coordinated edits: NixOS-module option defaults, the
> microvm-runner derivation's `graphics.enable`, and a
> `vm-graphics.nix` adjustment to stop claiming Vulkan/Venus for
> host-module classes. Each is mechanical, but they have to compose
> correctly. `low` would edit them in isolation and miss the
> Vulkan-env-var cleanup in `vm-graphics.nix`; `high` is overkill
> for what's still ~50 lines of Nix.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase:

1. `services.steampipe-cluster.loginRunners.hypervisor` defaults to
   `"qemu"` (was `"crosvm"`).
2. `services.steampipe-cluster.runners.hypervisor` defaults to
   `"cloud-hypervisor"` (was `"crosvm"`).
3. **Host-module classes do NOT enable `microvm.graphics.enable`.**
   No `-device virtio-vga-gl`, no Venus, no virgl. The login VM runs
   sway with `WLR_BACKENDS=headless` and renders Steam GUI on
   llvmpipe (software GL).
4. `nix/vm-graphics.nix` is adjusted: when graphics is OFF, no
   Venus ICD path is exported, no `WGPU_BACKEND=vulkan` env var.
   Mesa libs stay on the dlopen path so wayland clients still link.
5. `mkTestCluster` project-flake path continues to honour
   `[cluster] hypervisor = "crosvm"` for graphical workloads (with
   the deprecation warning). New `mkTestCluster` default derives
   from `graphics`: `qemu` if graphical, `cloud-hypervisor` if
   headless. Projects that explicitly set `hypervisor = "crosvm"`
   in `steampipe.toml` keep getting the patched crosvm overlay.
6. `cluster-ctl steam login vm-N` on atlas completes without a
   guest VCPU crash and reaches the Steam login step.

## Why this matters now

Phase 01's results doc records the RED verdict for the original
shape (QEMU + virgl/Venus + AMD Mesa segfaults under render load on
atlas). The pivot is documented in
[hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
"Pivot 2026-05-26":

- The host-module classes (`loginRunners`, `runners`) **don't run
  games**. The login wizard is CEF/GTK; the warm flow uses
  `DisplayMode::Headless` at
  [src/game/steam.rs:490](../../../src/game/steam.rs#L490).
- sway with `WLR_BACKENDS=headless`
  ([src/game/steam.rs:56-57](../../../src/game/steam.rs#L56-L57))
  already runs on a software backend; no virtio-gpu needed.
- wayvnc captures via wlr-screencopy; works on software-rendered
  outputs.

Removing the GPU device from the QEMU login path eliminates the
entire virgl/Venus crash surface for host-module classes.
**Project-flake games are a separate problem**; they keep crosvm.

## Out of scope

- **Project-flake `mkTestCluster` graphical workloads pinning
  `[cluster] hypervisor = "crosvm"`** — those still get the
  patched crosvm overlay (deprecated, still maintained). Phase 10
  does **not** delete the crosvm overlay.
- Deleting the `cloud-hypervisor-graphics` override or the
  `__steampipeOverlay` marker — that's Phase 10.
- Memory budgets (balloon, deflateOnOOM) — that's Phase 09.
- Anything in `mkTestCluster`'s `graphics = true` path beyond the
  hypervisor-derivation default; projects that explicitly pin
  crosvm are not modified.
- Re-recording golden fixtures for visual tests. Project-flake
  visual tests stay on whatever hypervisor their TOML pins.

## Plan

1. **Flip module defaults** in
   [nix/module.nix](../../../nix/module.nix):

   ```nix
   # loginRunners.hypervisor (~ line 272)
   default = "qemu";          # was "crosvm"

   # runners.hypervisor (~ line 310)
   default = "cloud-hypervisor";  # was "crosvm"
   ```

   Update each option's `description` and `example` to match the
   new defaults; mention `"crosvm"` is deprecated.

2. **Add valid-value assertions** in the `assertions` block:

   ```nix
   {
     assertion = !cfg.loginRunners.enable
       || builtins.elem cfg.loginRunners.hypervisor
            [ "qemu" "crosvm" "cloud-hypervisor" ];
     message = ''
       services.steampipe-cluster.loginRunners.hypervisor must be
       one of "qemu" (default, supported), "crosvm" (deprecated;
       see docs/src/planning/hypervisor-restructure-design.md), or
       "cloud-hypervisor".
     '';
   }
   {
     assertion = !cfg.runners.enable
       || builtins.elem cfg.runners.hypervisor
            [ "cloud-hypervisor" "qemu" "crosvm" ];
     message = ''
       services.steampipe-cluster.runners.hypervisor must be one of
       "cloud-hypervisor" (default, supported), "qemu", or "crosvm"
       (deprecated).
     '';
   }
   ```

3. **Add deprecation warnings** in the `warnings` block:

   ```nix
   ++ lib.optional (cfg.loginRunners.enable && cfg.loginRunners.hypervisor == "crosvm")
        "services.steampipe-cluster.loginRunners.hypervisor = \"crosvm\" is deprecated and scheduled for removal in steampipe 0.4. crosvm + virgl + Vulkan crashes the guest VCPU on Steam GUI startup; switch to \"qemu\" (host-module classes run sway-headless + llvmpipe; no virgl needed)."
   ++ lib.optional (cfg.runners.enable && cfg.runners.hypervisor == "crosvm")
        "services.steampipe-cluster.runners.hypervisor = \"crosvm\" is deprecated and scheduled for removal in steampipe 0.4. Switch to \"cloud-hypervisor\" (the warm path uses DisplayMode::Headless; no graphics device needed)."
   ```

4. **Disable graphics for host-module classes** by passing
   `graphics = false` through `host-runners.nix` to
   `microvm-runner.nix`. Concretely, in
   [nix/lib/host-runners.nix](../../../nix/lib/host-runners.nix):

   ```nix
   flavor = {
     inherit hypervisor useSystemdStage1;
     graphics = false;     # host-module classes never need virtio-gpu
     memory = null;
     gpuProfile = "none";  # no Venus, no virgl
   };
   ```

   The `graphics ? true` parameter at
   [nix/lib/host-runners.nix:13](../../../nix/lib/host-runners.nix#L13)
   becomes a no-op for host-module classes; remove it or document
   that it's ignored. Operators who want a graphical runner
   variant should use `mkTestCluster` with explicit `graphics =
   true` and a hypervisor that supports it.

5. **No `qemu.extraArgs` virgl injection** in
   [nix/lib/microvm-runner.nix](../../../nix/lib/microvm-runner.nix).
   The original design's `[ "-display" "none" "-device"
   "virtio-vga-gl" "-object" "memory-backend-memfd,..." ]` is
   **deleted from the plan**. The block becomes:

   ```nix
   microvm = {
     hypervisor = flavor.hypervisor;
     graphics.enable = flavor.graphics;  # false for host-module classes

     # crosvm escape hatch (deprecated; project-flake graphical workloads only)
     crosvm.extraArgs = lib.mkIf (flavor.hypervisor == "crosvm") [
       "--seccomp-policy-dir=${pkgs.crosvm}/share/policy/${pkgs.stdenv.hostPlatform.linuxArch}"
       "--no-usb"
     ];

     # Plain upstream cloud-hypervisor when headless (Phase 02 wired)
     cloud-hypervisor.package =
       lib.mkIf (flavor.hypervisor == "cloud-hypervisor" && !flavor.graphics)
         pkgs.cloud-hypervisor;

     # ... rest of microvm block unchanged
   };
   ```

   No `qemu.extraArgs` block. microvm.nix's QEMU runner produces
   `-nographic` when `graphics.enable = false`, which is exactly
   what we want.

6. **Adjust `nix/vm-graphics.nix`** to handle the
   graphics-disabled case cleanly. Current state at
   [nix/vm-graphics.nix:32-89](../../../nix/vm-graphics.nix#L32-L89)
   sets `VK_DRIVER_FILES`, `WGPU_BACKEND=vulkan`, and exports the
   Vulkan loader path. When the host-module classes pull this
   module with `microvm.graphics.enable = false`, the entire
   `config = lib.mkIf graphicsEnabled { ... }` block is skipped,
   so nothing claims Vulkan. **Verify** that's the case by reading
   the file end-to-end; if any non-gated env var is set
   unconditionally (e.g. outside the `lib.mkIf`), gate it.

   If host-module classes still need *some* graphics libs for
   wayland-client dlopen compatibility (so cluster-ctl-deployed
   binaries can link `libxkbcommon` / `libwayland`), add a
   smaller mode:

   ```nix
   options.steampipe.vmGraphics = {
     softwareRendering = lib.mkOption {
       type = lib.types.bool;
       default = false;
       description = ''
         When true and graphics.enable is false, still publish the
         Wayland client library path so cluster-ctl-deployed
         binaries can link libxkbcommon/libwayland/libGL (Mesa
         llvmpipe path). Use for VMs that run a sway/wayvnc UI
         on software rendering (e.g. the Steam login wizard).
       '';
     };
     # ... existing runtimeLibraryPath ...
   };

   config = lib.mkMerge [
     (lib.mkIf graphicsEnabled { /* existing Vulkan-aware config */ })
     (lib.mkIf (!graphicsEnabled && cfg.softwareRendering) {
       hardware.graphics.enable = true;  # Mesa for llvmpipe
       # NO VK_DRIVER_FILES; NO WGPU_BACKEND=vulkan.
       environment.sessionVariables.WAYLAND_DISPLAY = "wayland-1";
       environment.variables.STEAMPIPE_GRAPHICS_LIB_PATH =
         graphicalRuntimeLibPath;
       environment.etc."profile.d/steampipe-graphics.sh".text = ''
         export STEAMPIPE_GRAPHICS_LIB_PATH="${graphicalRuntimeLibPath}"
         # No WGPU_BACKEND; clients can override if they need GL.
       '';
       environment.systemPackages = with pkgs; [
         mesa-demos          # glxinfo, eglinfo for debugging
         wayland-utils
       ];
     })
   ];
   ```

   Then `host-runners.nix` sets `steampipe.vmGraphics.softwareRendering
   = true;` on the host-module microvm config. The login VM gets
   Mesa + Wayland libs but doesn't claim Vulkan; Steam's GUI falls
   back to llvmpipe naturally.

7. **`mkTestCluster` hypervisor derivation**. In
   [nix/lib/test-cluster.nix](../../../nix/lib/test-cluster.nix):

   ```nix
   mkFlavor = f: let
     graphics = f.graphics or vmGraphics;
     # New default: graphics → qemu (no virgl; software rendering),
     # headless → cloud-hypervisor. Project TOMLs pinning
     # hypervisor = "crosvm" keep the deprecated patched overlay.
     hypervisor = f.hypervisor or (if graphics then "qemu" else "cloud-hypervisor");
   in {
     inherit hypervisor graphics;
     memory = f.memory or null;
     gpuProfile = f.gpu_profile or "vulkan";  # honoured for crosvm only
     useSystemdStage1 =
       if f ? use_systemd_stage1
       then f.use_systemd_stage1
       else if clusterConfig ? use_systemd_stage1
       then clusterConfig.use_systemd_stage1
       else false;
   };

   vmHypervisor = clusterConfig.hypervisor
     or (if vmGraphics then "qemu" else "cloud-hypervisor");
   ```

   **Keep `patchCrosvmGpuParams` and the
   `if flavor.hypervisor == "crosvm" && flavor.graphics` branch in
   `mkRunnersDir`** — they're still needed when a project pins
   crosvm. Don't delete them in this phase (Phase 10 also keeps
   them).

8. **Update the `nix/lib/test-cluster.nix` header comment** to
   document the new default and the crosvm escape hatch.

9. **Documentation**:
   - [docs/src/getting-started/installation.md](../../getting-started/installation.md):
     drop any `hypervisor = "crosvm"` snippets from module config
     examples (the new defaults match what we want).
   - [docs/src/configuration/host-config.md](../../configuration/host-config.md):
     update default rows for both `loginRunners.hypervisor` and
     `runners.hypervisor`.

10. **New flake checks** in
    [nix/flake/checks.nix](../../../nix/flake/checks.nix):

    - `module-qemu-loginrunners-eval`: asserts a host with
      `loginRunners.enable = true; hypervisor = "qemu"; sshAuthorizedKey
      = "<fake>";` evals; the resulting `microvm-run` invokes
      `qemu-system-x86_64` AND **does not** contain
      `-device virtio-vga-gl` or `-device virtio-gpu-gl-pci`
      (graphics is off).
    - `module-crosvm-deprecation-warning`: asserts a host with
      `loginRunners.hypervisor = "crosvm"` emits the deprecation
      warning string in the eval output.

11. **Verify locally**:

    ```bash
    nix flake check --no-build
    cargo test --workspace  # nothing should break; Nix-only diff
    ```

12. **Smoke-test on atlas** (load-bearing acceptance check):

    - Bump canix's steampipe input pin to this phase's commit.
      Commit + rebuild atlas.
    - Inside the login VM (boot one manually first to verify):
      ```bash
      cluster-ctl up vm-1            # or one-off
      ssh cluster@10.0.100.1 -- 'eglinfo | head -10; vulkaninfo --summary 2>&1 | head -10'
      ```
      Expected: `eglinfo` reports a Mesa/llvmpipe renderer;
      `vulkaninfo` either fails or enumerates llvmpipe (Lavapipe)
      — **no Venus device**, **no segfault**.
    - Run `cluster-ctl steam login vm-N` (try vm-2 since it's
      the one that crashed historically). Expected: reaches
      Steam login UI, no `vcpu hit unknown error`, no QEMU
      segfault. UI may feel sluggish; that's expected with
      llvmpipe.
    - Run `cluster-ctl steam warm vm-N`. Expected: completes
      cleanly (headless display mode, no GUI rendering).

13. Commit (in steampipe):

    ```
    module: host-module classes default to QEMU (no-graphics) and cloud-hypervisor

    Phase 01 of the hypervisor-restructure plan proved QEMU virgl/Venus
    is empirically broken on atlas's Mesa 26.1.1 + AMD Navi 31 stack.
    Pivot: host-module classes don't need GPU rendering. The login
    wizard runs sway-headless + llvmpipe; warm uses
    DisplayMode::Headless. No virtio-gpu, no Vulkan, no virgl.

    Defaults:
      loginRunners.hypervisor: "crosvm" -> "qemu"
      runners.hypervisor:      "crosvm" -> "cloud-hypervisor"

    Host-module microvm configs pass graphics = false; nix/vm-graphics.nix
    gains a softwareRendering mode that publishes Mesa libs for
    Wayland-client dlopen compatibility but does not claim Vulkan.

    mkTestCluster's hypervisor default also derives from graphics
    (qemu graphical, cloud-hypervisor headless), but project-flake
    TOMLs pinning hypervisor = "crosvm" keep the deprecated patched
    overlay (project-flake graphical workloads need it until upstream
    virgl/Venus stabilises).

    Smoke-tested on atlas: cluster-ctl steam login vm-N completes
    without VCPU crash; cluster-ctl steam warm completes without
    regression.
    ```

## Acceptance criteria

- [ ] `nix eval .#nixosConfigurations.<test-host>.config.services.steampipe-cluster.loginRunners.hypervisor`
      returns `"qemu"`.
- [ ] Same for `runners.hypervisor` → `"cloud-hypervisor"`.
- [ ] `nix eval .#nixosConfigurations.<test-host>.config.microvm.graphics.enable`
      on a login-runner config returns `false`.
- [ ] The generated `microvm-run` for a login runner invokes
      `qemu-system-x86_64` and contains **neither**
      `-device virtio-vga-gl` **nor** `-device virtio-gpu-gl-pci`.
- [ ] `module-qemu-loginrunners-eval` flake check passes and
      verifies the no-virgl property.
- [ ] `module-crosvm-deprecation-warning` flake check fires the
      expected warning string.
- [ ] On atlas (after canix bumps and rebuilds): inside a booted
      login VM, `vulkaninfo --summary` does **not** enumerate a
      Venus device (it should fail or list only llvmpipe/Lavapipe).
- [ ] `cluster-ctl steam login vm-2` on atlas reaches the Steam
      login UI (verifiable via VNC); `vm-2/vm.log` contains
      **no** `vcpu hit unknown error: Bad address` line.
- [ ] `cluster-ctl steam warm` on atlas completes for every vm-N
      that has credentials.
- [ ] `nix/lib/test-cluster.nix` still contains
      `patchCrosvmGpuParams` (kept for the project-flake crosvm
      escape hatch).
- [ ] `mkTestCluster` with `[cluster] hypervisor = "crosvm"; graphics =
      true;` in a project's TOML still builds the
      `patchCrosvmGpuParams`-wrapped runner.
- [ ] `docs/src/getting-started/installation.md` and `host-config.md`
      reflect the new defaults; `mdbook build` clean.

## Files likely touched

- `nix/module.nix` — defaults, assertions, warnings.
- `nix/lib/host-runners.nix` — pass `graphics = false;
  gpuProfile = "none";` for host-module classes.
- `nix/lib/microvm-runner.nix` — preserve `crosvm.extraArgs` and
  the Phase-02 `cloud-hypervisor.package` pin; **no** new
  `qemu.extraArgs` block.
- `nix/vm-graphics.nix` — add `softwareRendering` option for
  host-module classes; keep the `graphicsEnabled` Vulkan path for
  project-flake graphical workloads on crosvm.
- `nix/lib/test-cluster.nix` — `mkFlavor` and `vmHypervisor`
  default derivation; **keep** `patchCrosvmGpuParams`; update the
  header comment.
- `nix/flake/checks.nix` — new `module-qemu-loginrunners-eval` +
  `module-crosvm-deprecation-warning`.
- `docs/src/getting-started/installation.md`,
  `docs/src/configuration/host-config.md` — default updates.

## Pitfalls

- **Don't delete `patchCrosvmGpuParams`**. The original design
  deleted it; the pivot keeps it because project-flake graphical
  workloads pinning crosvm still need it. Phase 10 also doesn't
  delete it.
- **Don't add `microvm.qemu.extraArgs` with virgl args**. That
  was the original design; Phase 01 disproved it. The login VM
  has no graphics device.
- **`microvm.graphics.enable = false` plus `qemu.extraArgs`**
  injecting `-device virtio-vga-gl` is contradictory — microvm.nix
  emits `-nographic` and then your extraArgs adds a GPU device.
  Pick one path; the design's choice is "no graphics."
- **vm-graphics.nix's `lib.mkIf graphicsEnabled` gate**. Verify
  every `config = ...` block is gated on either `graphicsEnabled`
  or the new `softwareRendering` flag. An unconditional
  `VK_DRIVER_FILES = ...` outside the gate would still claim
  Vulkan and may trigger guest-side llvmpipe rejection.
- **Don't break `mkTestCluster` crosvm projects**. Test the
  `[cluster] hypervisor = "crosvm"` path explicitly:
  `nix build .#cluster-1v1-up` (or whatever the project's
  default attr is) on a project that pins crosvm; the runner
  script must still reference `crosvm` and the patched
  `patchCrosvmGpuParams` derivation.
- **canix steampipe pin bump** is a canix commit. Phase 08
  steampipe commit lands first; canix bumps follow.

## Reference

- Design source of truth (pivot recorded):
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  "Pivot 2026-05-26 — host-module classes use QEMU/CHV without
  virtio-gpu".
- Phase 01 RED results:
  [01-validate-qemu-virgl-on-atlas-results.md](./01-validate-qemu-virgl-on-atlas-results.md).
- Why the warm path needs no GPU:
  [src/game/steam.rs:490](../../../src/game/steam.rs#L490) (uses
  `DisplayMode::Headless`).
- Why sway needs no GPU:
  [src/game/steam.rs:56-57](../../../src/game/steam.rs#L56-L57)
  (uses `WLR_BACKENDS=headless`).
- Crosvm escape hatch lives on:
  [Phase 10](./10-drop-crosvm-overlays.md) does NOT delete it.
