# Hypervisor + Compositor Restructure Research Dossier

## Goal And Trigger

After vm-2's crosvm VCPU EFAULT'd during Steam GUI startup
(see [login-runner-ssh-unreachable-findings.md](./login-runner-ssh-unreachable-findings.md)
"Optional follow-ups" and the `cluster-ctl steam login vm-2` transcript
on 2026-05-26), the user asked to restructure:

1. **Graphical login VMs**: switch hypervisor from `crosvm` to `qemu`.
   crosvm + virgl + Vulkan is empirically unreliable.
2. **Headless / runtime VMs**: switch to the "Rust hypervisor owned by
   the Linux Foundation" — i.e., **`cloud-hypervisor`**.
3. **Compositor**: evaluate `hyprland` as a replacement for `sway`, gated
   on whether VNC into hyprland is reliable through the wayvnc + grim +
   swaymsg-equivalent contract this repo already depends on.
4. **Constraints**: reliability is priority #1 (VMs must never OOM-crash).
   Minimize memory and disk footprint subject to that.

Output is a dossier (not a plan). A multi-phase plan should be invoked
from this dossier afterward.

## Current Reality

### Hypervisor configuration

The repo currently runs **all** VMs (login + runtime) on a heavily
overlay-patched **crosvm**:

- [nix/lib/host-runners.nix:12](../../../nix/lib/host-runners.nix#L12) and
  [nix/module.nix:272](../../../nix/module.nix#L272),
  [310](../../../nix/module.nix#L310) default `hypervisor = "crosvm"`.
- [nix/lib/test-cluster.nix:51](../../../nix/lib/test-cluster.nix#L51) the
  project-flake path also defaults to crosvm.
- [nix/flake/overlay.nix:41-196](../../../nix/flake/overlay.nix#L41-L196)
  carries ~155 lines of patches to make crosvm boot on modern kernels:
  - Faked `version = "107.1-..."` to dodge microvm.nix's
    `--seccomp-log-failures` workaround injection.
  - Source-policy install + `compile_seccomp_policy.py` BPF compile
    (nixpkgs only ships the binary).
  - `common_device.policy` / `gpu_common.policy` widenings for
    `PR_SET_NAME` (Rust thread name on glibc ≥ 2.40) and
    `MADV_GUARD_INSTALL` (kernel ≥ 6.13 stack guard pages).
  - `gfxstream` / `aemu` buildInputs and feature flag.
  - Tracked upstream: NixOS/nixpkgs#517363, microvm-nix/microvm.nix#516.
- [nix/flake/overlay.nix:20-26](../../../nix/flake/overlay.nix#L20-L26)
  also overrides `cloud-hypervisor-graphics`'s cargo vendor hash because
  the spectrum-os pin drifts behind nixos-unstable.

Despite all this, virgl still crashes the guest VCPU during Vulkan
context creation — see the `Bad address (os error 14)` line on vm-2.

### Compositor surface (sway coupling)

Sway is not an interchangeable detail. The Rust code reads sway IPC
into typed structures:

- [src/game/compositor.rs:431-501](../../../src/game/compositor.rs#L431-L501)
  defines `SwayOutput` and `SwayNode` and parses `swaymsg -t get_tree` /
  `swaymsg -r -t get_outputs` JSON.
- 70 callsites of `swaymsg`/`sway-ipc`/`wayvnc` across
  [src/game/capture.rs](../../../src/game/capture.rs),
  [src/game/scene/{runner,sync,action}.rs](../../../src/game/scene/),
  [src/fixture/golden.rs](../../../src/fixture/golden.rs),
  [src/game/compositor.rs](../../../src/game/compositor.rs),
  [src/game/steam.rs](../../../src/game/steam.rs).
- `DisplayMode` enum at
  [src/ui/cli.rs:65-78](../../../src/ui/cli.rs#L65-L78) hardcodes
  `Sway`, `SwayGpu`, `Weston`, `WestonGpu`, `Headless` — no hyprland
  variant.
- Sway readiness uses both `swaymsg -t get_outputs` (output mode
  stabilises before screenshot) and `swaymsg -t get_tree` (window class
  presence / focus). These are the i3 IPC dialect that sway extends.

`wayvnc` is **explicitly documented as "VNC server for wlroots based
Wayland compositors"** (its meta.description in nixpkgs 26.05). Hyprland
forked from wlroots to its own implementation (Aquamarine) around
0.40+. wayvnc still works on hyprland because hyprland implements the
`wlr-screencopy` and `wlr-output-management` protocols, but cursor
sync, multi-output bookkeeping, and resize behaviour diverge from sway.

### Memory + disk shape

- [nix/lib/host-runners.nix:14](../../../nix/lib/host-runners.nix#L14)
  defaults `memory = 2048` MiB for runtime VMs.
- [nix/module.nix:286](../../../nix/module.nix#L286) defaults
  `loginRunners.memory = 4096` MiB.
- [nix/lib/test-cluster.nix:114](../../../nix/lib/test-cluster.nix#L114)
  hard-codes per-VM `memory = 2048`, `cores = 1` for runtime VMs.
- [nix/lib/microvm-runner.nix:78](../../../nix/lib/microvm-runner.nix#L78)
  every VM gets a per-VM `8192` MiB home image
  (`vm-N-home.img`), `size = 8192`. **8 VMs × 8 GiB = 64 GiB** of
  per-host disk reserved before any Steam content lands.
- No balloon, no hotplug-mem, no KSM mention in the module surface.

### Shares (virtiofs vs 9p)

[nix/lib/microvm-runner.nix:82-89](../../../nix/lib/microvm-runner.nix#L82-L89)
declares a `proto = "virtiofs"` share for the Steam state dir whenever
`loginStateDir != null`.

Commit 4ee68f0 ("Drop runtime ssh-authorized virtiofs share from login
runners") documents the trap: **`cluster-ctl launches microvm-run
directly (not via systemd), so the host-side
`microvm-virtiofsd@&lt;vm&gt;-&lt;tag&gt;.service` that backs every
`proto = "virtiofs"` share never runs.**" The login runners specifically
work around this by passing `loginStateDir = null` to the runner
builder, which skips the share entirely
([nix/lib/host-runners.nix:46-52](../../../nix/lib/host-runners.nix#L46-L52)).
The project-flake `mkTestCluster` path
([nix/lib/test-cluster.nix:124-129](../../../nix/lib/test-cluster.nix#L124-L129))
DOES pass `loginStateDir` when `nixosIntegration = true`, which means
that path is also broken for steam-state persistence unless something
external launches virtiofsd.

In short: today, **no share is actually live**. The "Steam state"
persistence is purely in the per-VM home image. The virtiofs declaration
exists only for the systemd-microvm-host integration path, which
cluster-ctl bypasses.

### microvm.nix runner inventory

Verified from
`/nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/lib/runners/`:

- Graphics-capable runners: **`qemu`, `crosvm`, `cloud-hypervisor`
  (-graphics variant only), `vfkit`** (Darwin).
- Plain headless runners (no graphics path): **`firecracker`,
  `kvmtool`, `stratovirt`, `alioth`**, and `cloud-hypervisor` upstream.
- Share protocols:
  - **QEMU**: `virtiofs` (needs external virtiofsd) **OR `9p` (built-in
    to qemu, no daemon needed)**.
  - **cloud-hypervisor**: virtiofs **only**, `throw "cloud-hypervisor
    supports only shares that are virtiofs"` at
    [cloud-hypervisor.nix:240](file:///nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/lib/runners/cloud-hypervisor.nix).
  - crosvm: virtiofs + custom protocols.
- QEMU graphics backend in microvm.nix is restricted to **`gtk` or
  `cocoa`** — both require a host GUI session. There is no `-display
  none` backend exposed; getting "headless host, virgl GPU inside the
  VM, VNC out to the operator" requires going through
  `microvm.qemu.extraArgs` to inject `-display none -device
  virtio-vga-gl` (or `-device virtio-gpu-gl-pci,blob=on`) ourselves.

### Memory primitives in microvm.nix

[options.nix:125-172](file:///nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/nixos-modules/microvm/options.nix#L125-L172):

- `microvm.balloon` (bool) — virtio-balloon with free-page-reporting.
  Available on **qemu and cloud-hypervisor**, not crosvm.
- `microvm.deflateOnOOM` (bool) — automatic balloon deflation when
  guest OOMs. Critical for "never OOM-crash" goal.
- `microvm.hotplugMem` (MiB) — virtio-mem max size, guest can grow up
  to this; `hotpluggedMem` is the initial.
- `microvm.initialBalloonMem` (MiB) — start with N MiB pre-claimed by
  the balloon driver.

KSM (kernel same-page merging) is a host-side knob, not a microvm.nix
option. Enabled by `services.kernel.ksm = true` (or
`MemKSM=yes`-equivalent via host config).

### Existing planning context

- [docs/src/planning/login-runner-ssh-fix/README.md](./login-runner-ssh-fix/README.md)
  is the 5-phase SSH-key fix plan. P1 done in canix
  (995ff7b), P2/P3/P4/P5 done in steampipe (b3cd24d, 294a03d, 27f773a,
  31444bf). Atlas not yet rebuilt to activate the new closure
  (gen 348 predates the canix worktree's P1 effect). The fix is
  orthogonal to this restructure.
- [docs/src/planning/login-runner-ssh-unreachable-findings.md](./login-runner-ssh-unreachable-findings.md)
  is the dossier that drove the SSH fix. Its "Optional follow-ups"
  section flagged the empty-stdout-on-VM-crash diagnostic gap;
  the vm-2 VCPU crash makes that follow-up load-bearing for any
  hypervisor migration.

## Evidence Inventory

| Evidence | What it proves |
|---|---|
| `vm-2/vm.log` tail (`vcpu hit unknown error: Bad address (os error 14)` 79 ms after `GLSL feature level 430`) | crosvm + virgl + Vulkan crashes the guest VCPU, not a VNC failure. |
| `nix/flake/overlay.nix` 197-line overlay (crosvm + cloud-hypervisor-graphics hash override) | Switching off crosvm and off cloud-hypervisor-graphics removes both sub-trees and the upstream-issue dependency. |
| `microvm.nix` `lib/runners/` (8 runners; only 4 with graphics) | Migration target set is bounded. |
| `microvm.nix` `cloud-hypervisor.nix:240` `throw "supports only shares that are virtiofs"` | Cloud-hypervisor + share = virtiofs = blocked-by-no-virtiofsd. **For headless runtime we must avoid shares or wrap virtiofsd ourselves.** |
| Commit 4ee68f0 message ("microvm-run direct, virtiofsd never starts") | Confirms the no-virtiofsd-under-microvm-run regression that informs share protocol choice. |
| `wayvnc.meta.description = "VNC server for wlroots based Wayland compositors"` | Hyprland (post-wlroots fork) is at best partial-compat with wayvnc. |
| `hyprland.version = "0.55.2"`, available in nixpkgs 26.05 | Hyprland is *available*; that's not the same as supported. |
| 70 swaymsg callsites + `SwayOutput`/`SwayNode` typed parsers | Compositor-IPC coupling is structural, not cosmetic. Hyprland would force a ~500-line rewrite plus golden fixture re-record. |
| microvm.nix `options.nix:125-172` (balloon + deflateOnOOM + hotplugMem) | "Never OOM-crash" is achievable through balloon + deflateOnOOM; not crosvm-compatible (only qemu + chv). |
| `nix/lib/microvm-runner.nix:78` `size = 8192` per home image | Disk-side fat — 8 VMs × 8 GiB without anyone needing 8 GiB. Targetable reduction. |

## Existing Plan Status

Audit of `docs/src/planning/login-runner-ssh-fix/` against current state:

| Phase | Status | Evidence |
|---|---|---|
| 01 canix explicit key | **partial** | canix has commit `995ff7b`; atlas not rebuilt against current canix worktree (gen 348 predates the effect; live VM-1 closure still has empty `users-groups.json`). One nixos-rebuild from canix away. |
| 02 drop pathExists + assert | **done** | b3cd24d on trunk; `nix/module.nix:251-282`, `366-425` carry the new shape; `installation.md` rewritten. |
| 03 flake check empty-key regression | **done** | 294a03d; checks `module-loginrunners-empty-key-asserts`, `module-runners-empty-key-asserts`, `module-loginrunners-with-key-evals` build. |
| 04 vm.log tail tolerant read | **done** | 27f773a; `print_vm_log_tail` uses `microvm_log_path` + `from_utf8_lossy`. |
| 05 hostname cosmetic | **done** | 31444bf; `loginRunnerKeyScript` uses `config.networking.hostName`. |

P1's residual blocker (atlas rebuild) is **not blocking this dossier** —
the restructure replaces the entire login-runner derivation shape and
will subsume the P1-style key flow on its way through. It is a stand-
alone "rebuild atlas once" task.

The two SSH-fix planning artifacts (the plan dir and its findings note)
should **survive the restructure** as historical record. They explain
why the explicit-key contract exists and why `pathExists`-default reading
is dead under flakes — that reasoning still applies to whatever new
hypervisor we pick.

## Work That Should Survive

Even after migrating away from crosvm:

- The `services.steampipe-cluster.loginRunners.sshAuthorizedKey`
  contract (require explicit key, no `pathExists` default) — this is
  a flake-pure-eval constraint, not a hypervisor constraint.
- The host-side activation script that generates
  `/var/lib/steampipe/ssh/cluster_key{,.pub}` — bootstrap still works
  the same way.
- The `microvm-vm-1-microvm-run` linkfarm pattern under
  `/etc/steampipe/login-runners` and `/etc/steampipe/runners` —
  cluster-ctl's `microvm_runner_path` accessor at
  [src/core/backend.rs:297-298](../../../src/core/backend.rs#L297-L298)
  expects this layout; switching hypervisors does not change it.
- The sway readiness contract (`swaymsg -t get_outputs` returning
  active outputs, `swaymsg -t get_tree` returning the window tree) —
  see "Open Decision: compositor" below for why this stays.
- The `proto = "virtiofs"` → "actually 9p in QEMU, none in
  cloud-hypervisor" rewrite for any share we want to keep live, since
  no virtiofsd runs under cluster-ctl's microvm-run direct-launch.

## Blockers And Missing Artifacts

### B1: QEMU headless virgl on atlas

`microvm.graphics.backend` upstream supports only `gtk`/`cocoa`. Both
require a host display server. Atlas runs `cluster-ctl steam login`
from a tty or SSH, not from a GUI session. We need to drive QEMU with
`-display none -device virtio-vga-gl` (or `virtio-gpu-gl-pci`) via
`microvm.qemu.extraArgs`. **Unverified**: whether this works on
atlas's GPU (NVIDIA proprietary? Mesa? card model?) without a host
compositor session, and whether `virtio-vga-gl` initialises virgl
without an X11/Wayland connection.

- **Producer**: validation on atlas itself, not an upstream artifact.
- **Regeneration**: hand-craft a minimal qemu invocation
  (`qemu-system-x86_64 -display none -device virtio-vga-gl -nic none
  -M microvm,accel=kvm -kernel <some-kernel>`) and verify a guest can
  successfully open a virgl/Venus context.
- **Validation**: `dmesg | grep virtio_gpu` inside a test guest shows
  a working device, `vulkaninfo --summary` inside the guest enumerates
  Venus, and the QEMU process doesn't crash for at least one steam
  GUI cold-start.

### B2: cloud-hypervisor maturity for our use case

The microvm.nix runner for `cloud-hypervisor` works, but our overlay
exists *because* the graphics variant doesn't track nixpkgs cleanly
([overlay.nix:9-26](../../../nix/flake/overlay.nix#L9-L26)). For
**headless** cloud-hypervisor (no graphics):

- nixpkgs `cloud-hypervisor` package builds straight from upstream
  releases without the spectrum-os patch stack — verified by it being
  the base of the `-graphics` derivation's `prev.cloud-hypervisor` ref.
- microvm.nix's `cloud-hypervisor.nix` runner is the same code path
  graphical CHV uses, gated by `graphics.enable`. Setting
  `graphics.enable = false` should drop the requirement for the
  `-graphics` package entirely.
- **Unverified**: whether `microvm.cloud-hypervisor.package` defaults
  to the graphics variant even when `graphics.enable = false`, or
  whether we need to pin it explicitly. Suspicion: yes, default is
  graphics. Check
  [options.nix:764-772](file:///nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/nixos-modules/microvm/options.nix#L764-L772).

### B3: Whether QEMU `virtio-balloon` actually deflates under guest OOM with Linux 6.18

The microvm.nix option exists. The guest kernel needs `CONFIG_VIRTIO_BALLOON=y`
and the balloon driver to be loaded. Most stock NixOS kernels include
it. **Unverified**: that the deflate-on-OOM path actually rescues a VM
before the OOM killer trips a critical Steam process. Sometimes the
OOM killer fires before the balloon-deflate signal lands. Worth
empirically testing under a synthetic Steam-launch + memory-soak load.

### B4: Atlas-specific GPU stack

The host has crosvm-overlay-patched seccomp because of glibc 2.40 +
kernel ≥ 6.13. Whether atlas's kernel and userspace stack lets QEMU
talk to its GPU through virgl is *independent* of those patches but
**unverified**. Could be `nvidia-drm` (which doesn't expose virgl
nicely) or `i915`/`amdgpu` (which do). Need to read host hardware
config.

### B5: vm-graphics module assumes Vulkan + virgl, not virtio-gpu-gl

[nix/vm-graphics.nix:37](../../../nix/vm-graphics.nix#L37) sets
`VK_DRIVER_FILES = "/run/opengl-driver/share/vulkan/icd.d/virtio_icd.x86_64.json"`
unconditionally (except for `egl` profile). That's the Venus ICD,
which works under crosvm's `--params context-types=...venus` capset.
QEMU's `virtio-vga-gl` enables Venus through `virtio-gpu-pci,gl=on`
or `virtio-vga-gl`. **Unverified**: whether the same VK ICD path is
correct in the QEMU-virgl-Venus combination, or whether we need a
different ICD file or to fall back to `virtio_icd.x86_64.json` vs
`virtio-gpu_icd.x86_64.json`.

## Risks And Constraints

- **Reliability tradeoff on graphical**: QEMU + virgl is the *most
  battle-tested* virtio-gpu path. QEMU's virglrenderer integration is
  what Spectrum-OS and Fedora's vagrant images use. Switching the
  graphical path to QEMU is the **less risky** choice, not more
  risky. The crosvm seccomp/madvise battle has been a continuous
  upstream chase (NixOS/nixpkgs#517363, microvm-nix#516); QEMU has
  none of that.
- **Performance tradeoff on graphical**: QEMU is heavier per-VM
  (~80 MB extra resident for qemu-system-x86_64 + accel layers)
  than crosvm. With balloon + deflateOnOOM the actual working set
  shouldn't grow proportionally — but be honest about the baseline.
- **Reliability tradeoff on headless**: Cloud-hypervisor is what
  AWS Lambda and confidential VMs run, so production maturity is
  excellent. The main concern: upstream cloud-hypervisor's pace
  is fast (monthly releases), and microvm.nix's pinned version may
  drift. nixpkgs 26.05 ships a recent enough version.
- **virtiofs gap**: any future re-introduction of a Steam state share
  needs to either (a) launch virtiofsd from `microvm-run` directly,
  (b) switch to 9p (qemu only — cloud-hypervisor doesn't support 9p),
  or (c) commit to using microvm.host systemd integration (which
  cluster-ctl currently bypasses).
- **Compositor lock-in**: keeping sway is safer than switching to
  hyprland. The user's "consider hyprland" is conditional on VNC
  parity, and wayvnc's wlroots-based design + 70 swaymsg call sites
  fail that gate. Hyprland is also notably heavier than sway in RAM,
  contradicting the "minimize memory" constraint. **Recommendation:
  drop the hyprland leg of the migration entirely.**
- **Test cluster vs login/runtime cluster divergence**: the project-
  flake (`mkTestCluster` in
  [nix/lib/test-cluster.nix](../../../nix/lib/test-cluster.nix))
  reads `[cluster] hypervisor` from `steampipe.toml`. Existing project
  consumers — chessbender, regicide, fragpipe — may have pinned
  `hypervisor = "crosvm"` in their `steampipe.toml`. The migration
  needs to either (a) flip the default and surface a clear deprecation
  for old TOMLs, or (b) keep the `[cluster] hypervisor` option but
  recommend cloud-hypervisor for new projects.
- **vmCount asymmetry**: nix/lib/test-cluster.nix:103 hardcodes
  `vmCount = 7`, but the steampipe-cluster module defaults to 7 and
  atlas sets 8. Memory math has to consider the max realistic VM
  count, not the default.
- **Migration sequencing risk**: if we flip the headless path first
  but leave login on crosvm, the `cloud-hypervisor` and `cloud-
  hypervisor-graphics` overlay halves can be partially dropped, but
  not the crosvm overlay. Sequencing matters for overlay deletion.
- **Steam state persistence**: the per-VM home image (`vm-N-home.img`)
  currently holds `/home/cluster/.local/share/Steam` for login runners
  (because the virtiofs share never actually mounts). That's load-
  bearing for "vm-1 is already logged in" persistence the user
  observed. If we shrink the home image, the Steam state has to move
  somewhere — either a working virtiofs/9p share or a dedicated
  per-VM Steam image.
- **Cargo of swaymsg parse types**: `SwayOutput` and `SwayNode` are
  used in tests. Any compositor migration breaks unit tests at
  [src/game/compositor.rs](../../../src/game/compositor.rs) (and the
  golden fixture comparisons that depend on rect / app_id matching).
- **`microvm-run` PID lifetime**: cluster-ctl's lease lifecycle
  (`vm-1.claim PID X is dead, reclaiming`) depends on the
  microvm-run process being the parent of the hypervisor binary. QEMU
  and cloud-hypervisor wrappers in microvm.nix preserve that contract
  but verify under load.

## Candidate Next Steps

Sequenced by dependency. Each step is a phase candidate for a
multi-phase plan after this dossier.

### Step A — Validate QEMU headless virgl on atlas (1 session, frontier)

Minimal repro: hand-launch a QEMU microvm with `-display none -device
virtio-vga-gl` against atlas's GPU; boot a NixOS guest with
`vm-graphics.nix`-equivalent setup; verify `vulkaninfo --summary`
enumerates Venus, sway starts, wayvnc serves a frame, and the VM
survives a synthetic Steam GUI cold-start. This **unblocks B1, B4, B5
in one go**.

This is the highest-uncertainty step. If it fails on atlas's GPU, the
whole graphical migration needs a different approach (passthrough?
software-only EGL?).

### Step B — Add cloud-hypervisor headless runner (independent of A)

In parallel with A:

- Add `hypervisor = "cloud-hypervisor"` as a supported runtime-runner
  value.
- Ensure `microvm.cloud-hypervisor.package = pkgs.cloud-hypervisor`
  (not `-graphics`) when `graphics.enable = false`.
- Drop the cloud-hypervisor-graphics override from
  `nix/flake/overlay.nix` *if* nothing else depends on it after Step C
  ratchets crosvm-graphics out. (Until then, leave it.)
- Add a flake check that evals a cloud-hypervisor runtime VM.
- No Rust changes — `microvm-runner-path` accessor is
  hypervisor-agnostic.

Verify: a runtime VM boots, sshd accepts the cluster key, and
`cluster-ctl up` lease + `cluster-ctl steam check` survive.

### Step C — Switch login runners to QEMU (depends on A)

Once Step A confirms QEMU virgl works:

- `loginRunners.hypervisor` default flips from `"crosvm"` to `"qemu"`.
- Add `microvm.qemu.extraArgs` in `nix/lib/microvm-runner.nix` for
  graphical login runners: `-display none -device virtio-vga-gl`
  (or the variant Step A validated).
- Update `nix/vm-graphics.nix` if the VK_DRIVER_FILES path needs to
  change (Step A also answers this).
- Verify `cluster-ctl steam login vm-2` survives a Steam GUI cold-
  start; specifically that there is **no VCPU EFAULT** in vm.log.

### Step D — Memory budget rework (depends on B and C)

After both new hypervisors are in place:

- Enable `microvm.balloon = true; microvm.deflateOnOOM = true;` on
  both login and runtime VMs.
- Set per-class memory:
  - **Login VMs**: `mem = 4096`, `initialBalloonMem = 1024`.
    Realistic Steam GUI: ~2.5 GiB working set + 1 GiB headroom; rest
    is host-reclaimable.
  - **Runtime VMs**: `mem = 2048`, `initialBalloonMem = 512`.
    Adjust per-project via `[cluster.flavors.<name>].memory` already
    present in `mkTestCluster`.
- Enable `services.kernel.ksm = true` on the host (canix-side config,
  not steampipe). Document.
- Document the memory contract in `installation.md`.

### Step E — Shrink the home image (independent of A/B/C)

- Drop `volumes[].size` from 8192 to 2048 for login runners (no game
  content there). Runtime VMs keep 8192 only if game-content land
  in the home image; if games land in a separate share, also shrink.
- This is purely a build-time number change; **but** existing on-disk
  `vm-N-home.img` files are already 8 GiB on atlas. Either nuke them
  on rebuild or live with the discrepancy until the next clean run.

### Step F — Fix the swallowed-stderr diagnostic (independent)

Carried forward from the SSH-fix dossier's "Optional follow-ups". When
`backend.run_cmd` returns `success = false` with empty stdout/stderr,
append `print_vm_log_tail` output to the error message. This is what
would have surfaced the crosvm VCPU EFAULT message on the very first
`cluster-ctl steam login vm-2` invocation. Without it, every future
hypervisor crash will hide the same way.

### Step G — Drop the crosvm overlay (depends on A, B, C all green)

The big payoff. Delete:

- `crosvm` block in `nix/flake/overlay.nix` (~150 lines).
- `crosvm.extraArgs` plumbing in `nix/lib/microvm-runner.nix`.
- `patchCrosvmGpuParams` and `--cluster crosvm-*` aliases in
  `nix/lib/test-cluster.nix`.
- The `module-loginrunners-empty-key-asserts`-adjacent overlay-marker
  flake check (the one that asserts `pkgs.crosvm.passthru.__steampipeOverlay`).
- The corresponding warning in `nix/module.nix:435-436`.

Keep `crosvm` as an *option* value if someone wants to test it
manually, but stop maintaining the patched overlay.

### Step H — Document and retire `hyprland` as a non-goal (independent)

A short note in the docs explicitly explaining: "We evaluated hyprland
2026-05; it would force a 70-callsite rewrite of swaymsg IPC parsers,
isn't on wayvnc's wlroots-tested matrix anymore (Aquamarine), and is
heavier in RAM. We're staying on sway." Prevents revisiting the
question every six months.

### Sequencing summary

```
A ────────────┬─→ C ─→ D ─┐
              │           ├─→ G
B ────────────┴─→         ┘

E, F, H : independent, can run any time
```

Dependencies:
- **A blocks C** (QEMU virgl must work on atlas before we make it the
  default).
- **A + B + C all green block G** (drop the crosvm overlay only after
  both new paths prove themselves).
- **D depends on B and C** (the memory primitives are per-hypervisor;
  no point setting `balloon = true` on crosvm where it isn't supported).
- E, F, H have no dependencies and can land in any order.

Recommended waves:
- Wave 0: A, B, E, F, H (5 parallel phases).
- Wave 1: C (after A).
- Wave 2: D (after B + C).
- Wave 3: G (after A + B + C all proven on atlas through real
  cluster-ctl steam login + steam test runs).

## Open Decisions For The User

1. **Drop hyprland from scope.** This dossier recommends keeping sway.
   The user's phrasing was "consider using hyprland ... if it is
   possible to VNC". The evidence (wayvnc wlroots-only docs, 70
   swaymsg callsites, hyprland's higher RAM footprint, and the user's
   own "memory is a priority" constraint) all point to "no". Confirm
   this is the right read before generating phase files.

2. **What lives in `vm-N-home.img`?** The current 8 GiB-per-VM
   reservation is load-bearing for at least the "vm-1 is already
   logged in" Steam state on login runners. Two paths:
   - **Path A**: keep home image, just shrink to 2 GiB for login
     runners (Step E). Simple. Acceptable for ~50 MiB Steam config.
   - **Path B**: re-introduce a real virtiofs/9p share for
     `~/.local/share/Steam` so login state lives on the host's
     `loginStateDir`. More work, but lets us shrink home image to
     512 MiB and centralises rotation.
   Decide before Step E. Path A is the default in this dossier.

3. **Runtime VM hypervisor under `mkTestCluster`.** Project flakes
   (chessbender, regicide, fragpipe) read `[cluster] hypervisor` from
   `steampipe.toml`. Once the headless default flips to cloud-
   hypervisor, downstream TOMLs pinning `"crosvm"` either:
   - Get a warning + auto-upgrade to cloud-hypervisor.
   - Are honoured silently (with future deprecation).
   - Become hard errors.
   The dossier doesn't pick; raise this before phasing.

4. **Bootstrap of `cluster_key` on atlas.** Independent of this
   restructure but currently the live VM-1 closure on atlas
   *still* doesn't bake the SSH key (gen 348 predates the canix
   worktree's effect). A `nixos-rebuild switch` from canix before
   the restructure starts will confirm the SSH-fix plan can be
   retired and clears the deck. If you'd rather restructure first
   and rebuild once at the end, defer; just be aware that
   any test of "cluster-ctl steam login vm-N" before that rebuild
   will still fail at the SSH stage and won't actually exercise the
   new hypervisor paths.
