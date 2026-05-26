# Plan: Hypervisor + Compositor Restructure

> **Recommended Codex model for orchestration: GPT 5.5 high**
>
> Ten phases across four sequencing waves with one frontier-risk leaf
> (Phase 01, the atlas QEMU virgl proof). The orchestrator that fans
> these out must track which downstream phases are blocked when 01
> reports back, and must decide between picking a fallback shape for
> graphical login (llvmpipe / virtio-gpu-gl-pci variant) vs aborting
> waves 2-4 entirely. That's a non-trivial design call mid-plan,
> hence `high` — `medium` would routinely accept the first fallback
> without weighing the consequences.

## Scope and current state

After crosvm + virgl + Vulkan crashed the guest VCPU during
`cluster-ctl steam login vm-2` (see
[hypervisor-restructure-research.md](../hypervisor-restructure-research.md)
for evidence, [hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
for resolved decisions), we are restructuring the hypervisor + memory
+ compositor layer:

- **Graphical login VMs**: crosvm → **QEMU** (with `-display none
  -device virtio-vga-gl` because atlas is headless).
- **Headless runtime VMs**: crosvm → **cloud-hypervisor** (plain
  upstream LF release, not the `-graphics` spectrum-os variant).
- **Compositor**: sway stays. Hyprland is explicitly out of scope
  (wayvnc is wlroots-only; 70 swaymsg callsites; heavier in RAM).
- **Memory**: virtio-balloon + `deflateOnOOM` per-VM, host-side
  `memory-admission` admission gate around parallel VM startup,
  KSM enabled on the host.
- **Disk**: shrink login-runner home image to 2 GiB (Steam config
  only; no game content lands there).
- **Cleanup**: delete the ~155-line crosvm overlay and the
  cloud-hypervisor-graphics hash override once the new paths prove
  themselves on atlas.

## Phase 01 RED — pivot recorded

Phase 01 produced a **RED** verdict on the original Phase 08 shape
(see [01-validate-qemu-virgl-on-atlas-results.md](./01-validate-qemu-virgl-on-atlas-results.md)).
Specifically: QEMU's `-display none -device virtio-vga-gl` is
rejected before guest boot (the `none` backend has no OpenGL); the
Venus fallback (`virtio-gpu-gl-pci,blob=on,venus=on,hostmem=512M`)
enumerated Vulkan devices but segfaulted QEMU under render workload
on atlas's Mesa 26.1.1 + AMD Navi 31 stack.

**Pivot**: drop the graphics device entirely for host-module classes
(`loginRunners` and `runners`). Neither runs games — the login
wizard is CEF/GTK (software-renderable on llvmpipe) and the warm
path uses `DisplayMode::Headless` at
[src/game/steam.rs:490](../../../src/game/steam.rs#L490) (no
compositor at all). Phase 08 is **rewritten**, not blocked. See
[hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
"Pivot 2026-05-26 — host-module classes use QEMU/CHV without
virtio-gpu" for the locked revised design.

**Games** (project-flake `mkTestCluster` graphical runtime VMs) are
**out of scope** for this restructure. The crosvm overlay stays
alive (Phase 10 only drops `cloud-hypervisor-graphics` and the
`__steampipeOverlay` marker, not the crosvm seccomp patches).
Project-level graphical workloads keep `[cluster] hypervisor =
"crosvm"` as a deprecated-but-working escape hatch. A separate
research dossier evaluates the game-rendering question when
upstream QEMU/Mesa/virgl Venus stabilises (target window:
2026-Q4 nixpkgs).

A new docs phase ([Phase 11](./11-document-graphics-and-games.md))
records the limitation publicly.

## Phases

| Phase | File | Depends on | Touches | Tier | Wave |
|-------|------|------------|---------|------|------|
| 01 | [01-validate-qemu-virgl-on-atlas.md](./01-validate-qemu-virgl-on-atlas.md) | — | atlas-only; throwaway QEMU invocation | 5.5 high | 0 |
| 02 | [02-add-cloud-hypervisor-headless.md](./02-add-cloud-hypervisor-headless.md) | — | `nix/module.nix`, `nix/flake/checks.nix` | 5.5 medium | 0 |
| 03 | [03-shrink-login-home-image.md](./03-shrink-login-home-image.md) | — | `nix/lib/microvm-runner.nix`, `src/cli/` (reset subcommand) | 5.5 low | 0 |
| 04 | [04-empty-stderr-diagnostic.md](./04-empty-stderr-diagnostic.md) | — | `src/game/steam.rs`, `src/harness/runner.rs` | 5.5 medium | 0 |
| 05 | [05-document-hyprland-no.md](./05-document-hyprland-no.md) | — | `docs/src/configuration/visual-testing.md` | 5.5 low | 0 |
| 06 | [06-memory-admission-gate.md](./06-memory-admission-gate.md) | — | `Cargo.toml`, `src/core/admission.rs`, multiple call sites | 5.5 medium | 0 |
| 07 | [07-canix-rebuild-and-ksm.md](./07-canix-rebuild-and-ksm.md) | — | canix `root/hosts/atlas/` | 5.5 low | 0 |
| 08 | [08-flip-login-default-to-qemu.md](./08-flip-login-default-to-qemu.md) | 01 | `nix/module.nix`, `nix/lib/microvm-runner.nix`, `nix/lib/test-cluster.nix`, `nix/vm-graphics.nix`, docs | 5.5 medium | 1 |
| 09 | [09-memory-budgets.md](./09-memory-budgets.md) | 02, 08 | `nix/lib/microvm-runner.nix`, `nix/module.nix`, `docs/src/configuration/vm-resources.md` | 5.5 medium | 2 |
| 10 | [10-drop-crosvm-overlays.md](./10-drop-crosvm-overlays.md) | 02, 08, 09 verified on atlas | `nix/flake/overlay.nix`, `nix/flake/checks.nix`, `nix/module.nix` | 5.5 medium | 3 |
| 11 | [11-document-graphics-and-games.md](./11-document-graphics-and-games.md) | — | `docs/src/configuration/graphics-and-games.md`, SUMMARY | 5.5 low | 0 |

## Dependency table

| Phase | Depends on | Touches files | Can parallel with |
|-------|------------|---------------|-------------------|
| 01 | — | atlas-only (no repo edits) | 02, 03, 04, 05, 06, 07 |
| 02 | — | `nix/module.nix` (option), `nix/flake/checks.nix` (new check) | 01, 03, 04, 05, 06, 07 |
| 03 | — | `nix/lib/microvm-runner.nix` (volume size), `src/cli/`, `src/game/lifecycle.rs` | 01, 02, 04, 05, 06, 07 |
| 04 | — | `src/game/steam.rs`, `src/harness/runner.rs` | 01, 02, 03, 05, 06, 07 |
| 05 | — | `docs/src/configuration/visual-testing.md`, SUMMARY | 01, 02, 03, 04, 06, 07 |
| 06 | — | `Cargo.toml`, `src/core/admission.rs` (new), `src/core/backend.rs`, `src/lib.rs`, `src/cli/` | 01, 02, 03, 04, 05, 07 |
| 07 | — | canix repo only | 01, 02, 03, 04, 05, 06 |
| 08 | 01 | `nix/module.nix`, `nix/lib/microvm-runner.nix`, `nix/lib/test-cluster.nix`, `nix/vm-graphics.nix`, installation.md | 09 (after both 02 and 08 land) |
| 09 | 02, 08 | `nix/lib/microvm-runner.nix`, `nix/module.nix`, new `docs/src/configuration/vm-resources.md` | 10 (only after 09 verified) |
| 10 | 02, 08, 09 verified live on atlas | `nix/flake/overlay.nix`, `nix/flake/checks.nix`, `nix/module.nix` (overlay-marker warning) | — |
| 11 | — | `docs/src/configuration/graphics-and-games.md`, SUMMARY | 01, 02, 03, 04, 05, 06, 07 |

Phases 02, 03, 06, 08, 09, 10 all touch `nix/module.nix` or
`nix/lib/microvm-runner.nix` and must serialise file-by-file. The
wave layout already enforces that ordering; if two same-wave phases
need to edit the same file, the second rebases.

## Parallelism layer

**Wave 0** (7 phases, can fan out in parallel sessions):

- **01** — atlas-side empirical proof that QEMU virgl works headless.
  No repo edits. The result (pass / fail / fallback variant) gates
  Wave 1.
- **02** — add `cloud-hypervisor` as a supported runtime hypervisor
  in the NixOS module + flake check, **without** flipping the
  default. Pure Nix.
- **03** — shrink login-runner home image to 2 GiB + add
  `cluster-ctl reset <vm> --home` helper. Independent of the
  hypervisor migration.
- **04** — fix the empty-stdout-AND-stderr diagnostic eat in
  `finish_steam_login` and audit other `bail!` sites for the same
  pattern. Independent.
- **05** — record the hyprland-no decision in
  `docs/src/configuration/visual-testing.md`. Markdown only.
- **06** — add `memory-admission` admission gate around parallel
  VM startup. Rust only. Independent of the hypervisor swap.
- **07** — canix side: rebuild atlas (retires SSH-fix P1) and
  enable `hardware.ksm.enable = true;`. Outside steampipe repo.
- **11** — new docs page `docs/src/configuration/graphics-and-games.md`
  explaining the project-flake graphics limitation and the still-
  supported crosvm escape hatch. Markdown only.

**Wave 1** (after 01):

- **08** — flip `loginRunners.hypervisor` default to `"qemu"` and
  `runners.hypervisor` default to `"cloud-hypervisor"`. **No
  `-device virtio-vga-gl` and no `graphics.enable = true;` for
  host-module classes** — the login wizard runs on sway-headless
  + llvmpipe (software GL), and warm uses `DisplayMode::Headless`.
  Update `nix/vm-graphics.nix` to drop the Venus ICD path and
  Vulkan env vars for the host-module classes. `mkTestCluster`
  default hypervisor derivation **continues to honour `[cluster]
  hypervisor = "crosvm"`** for project-flake graphical workloads —
  the deprecation warning fires but the path keeps working.
  Verify via `cluster-ctl steam login vm-N` on atlas — must
  complete without a guest VCPU crash and reach the Steam login
  step.

**Wave 2** (after 02 and 08):

- **09** — set `microvm.balloon = true; deflateOnOOM = true;` per
  VM class. cloud-hypervisor VMs also get an `initialBalloonMem`
  claim (1024 login, 512 runtime); QEMU keeps
  `initialBalloonMem = 0` because the pinned microvm.nix QEMU
  runner rejects non-zero values. Document the memory contract in
  a new `docs/src/configuration/vm-resources.md`.

**Wave 3** (after 02, 08, 09 proven live on atlas):

- **10** — delete the `cloud-hypervisor-graphics` override block,
  the `__steampipeOverlay` marker warning in `nix/module.nix`, and
  the matching marker check in `nix/flake/checks.nix`. **Keep the
  crosvm overlay block** — it stays alive as the deprecated
  escape hatch for project-flake graphical workloads, awaiting the
  upstream virgl/Venus stabilisation (target: 2026-Q4 nixpkgs).

```
Wave 0: 01, 02, 03, 04, 05, 06, 07, 11   (8 parallel)
Wave 1: 08                                (after 01)
Wave 2: 09                                (after 02 + 08)
Wave 3: 10                                (after 02 + 08 + 09 verified live)
```

Plan exhausts at end of Wave 3.

## Whole-set acceptance criteria

- [ ] `cluster-ctl steam login vm-N` on atlas completes without a
      `vcpu hit unknown error` or QEMU segfault in `vm.log`.
- [ ] `services.steampipe-cluster.loginRunners.hypervisor` default is
      `"qemu"`; `services.steampipe-cluster.runners.hypervisor`
      default is `"cloud-hypervisor"`. `nix flake check` is green.
- [ ] `nix/flake/overlay.nix` no longer contains the
      `cloud-hypervisor-graphics` override block; the `crosvm`
      block remains (deprecated, still maintained for
      project-flake graphical workloads).
- [ ] `nix/module.nix` no longer references
      `pkgs.crosvm.passthru.__steampipeOverlay`.
- [ ] Login-runner `vm-N-home.img` files are 2 GiB; runtime stay 8 GiB.
- [ ] `cluster-ctl admission status` prints `MemoryStats` plus the
      configured watermarks; the admission gate is wired around
      every parallel `start_instance` call site.
- [ ] `cluster-ctl steam login` failures with empty stdout/stderr
      append `vm.log` tail to the error message.
- [ ] atlas's live system has `hardware.ksm.enable = true` and a
      `nixos-system-vm-1` closure that contains the
      `/var/lib/steampipe/ssh/cluster_key.pub` key.
- [ ] `docs/src/configuration/vm-resources.md` and
      `docs/src/configuration/graphics-and-games.md` exist and are
      linked from `SUMMARY.md`. `mdbook build` clean.
- [ ] `cluster-ctl steam warm` on atlas completes for every vm-N
      without regression — proves the headless `runners`
      cloud-hypervisor path works under real load.
- [ ] Project-flake graphical workloads pinning `[cluster]
      hypervisor = "crosvm"` continue to build and run; the
      deprecation warning fires but no breakage.

## Global constraints

- **Reliability priority #1**. Never sacrifice the "VMs don't crash
  from OOM or VCPU EFAULT" goal for memory tightness. If a phase
  hits a choice between "use less RAM" and "guarantee no OOM-crash",
  pick the latter.
- **No new compositor**. Sway only. `DisplayMode` enum unchanged.
- **Crosvm as escape hatch only**. Honour the `hypervisor =
  "crosvm"` pin with a deprecation warning; do not hard-error.
  Removal scheduled for 0.4.
- **No virtiofs revival**. The
  `microvm-virtiofsd@<vm>-<tag>.service` problem from commit 4ee68f0
  is not addressed in this restructure. Steam state lives on the
  per-VM home image (Path A from the design).
- **No phase generates run scripts, dispatch shims, or
  orchestrators**. The user runs each phase in a fresh Codex
  session.

## Reference

- Research dossier:
  [hypervisor-restructure-research.md](../hypervisor-restructure-research.md)
- Locked design:
  [hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
- Predecessor plan (mostly shipped, P1 retires in Phase 07):
  [login-runner-ssh-fix/README.md](../login-runner-ssh-fix/README.md)
- Originating evidence: vm-2 VCPU EFAULT log, captured in the
  research dossier's "Current Reality" section.
