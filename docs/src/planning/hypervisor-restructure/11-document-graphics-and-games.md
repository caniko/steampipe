# Phase 11 — Document the graphics-and-games limitation

> **Recommended Codex model: GPT 5.5 low**
>
> One new markdown page explaining the project-flake graphics
> situation and the crosvm escape hatch. No code, no design calls
> beyond restating the locked design. `low` is right; anything
> higher just burns tokens.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

A new page
`docs/src/configuration/graphics-and-games.md` explains, plainly:

- Host-module classes (`loginRunners`, `runners`) do not run games
  and use software rendering (sway-headless + llvmpipe). Reliable.
- Project-flake `mkTestCluster` graphical workloads (chessbender,
  regicide, fragpipe) need GPU rendering and currently rely on
  the deprecated `crosvm` hypervisor with its patched overlay.
  The path works for lightweight 2D / card-game-class workloads
  but is brittle for heavy 3D under sustained Vulkan load (see
  the vm-2 VCPU EFAULT incident, 2026-05-26).
- The crosvm overlay stays maintained through 2026-Q4 nixpkgs,
  pending upstream virglrenderer + Mesa + QEMU virgl Venus
  stabilisation. After that, the project re-evaluates whether
  QEMU + Venus or another path becomes viable.

## Why this matters now

The hypervisor restructure intentionally narrows scope: it fixes
the **host-module classes** that the `cluster-ctl steam login
vm-2` incident exposed. **Game rendering inside VMs** is a
parallel, harder problem that the restructure doesn't solve.

Without a docs page, the next operator who hits a virgl crash
under a real game test will rediscover the situation by reading
commit history and planning notes. A clear public statement at
`docs/src/configuration/graphics-and-games.md` prevents that.

Independent of every other phase. Lands in Wave 0.

## Out of scope

- Removing `crosvm` as a supported `hypervisor` value. Phase 08
  marks it deprecated; this phase only documents.
- Picking a successor path for project-flake games. That's a
  separate research dossier when upstream stabilises.
- Editing any project-flake (chessbender etc.) `steampipe.toml`.
  The docs page describes the situation; downstream projects
  decide when/whether to migrate.
- Touching `mkTestCluster` defaults. The default still derives
  from `graphics` (per Phase 08); projects pinning crosvm
  explicitly opt in to the patched build.

## Plan

1. Inspect adjacent docs to pick the right slug and section:

   ```bash
   ls docs/src/configuration/
   grep -n graphics docs/src/SUMMARY.md
   ```

   Expected: a Configuration section in SUMMARY.md. The new page
   slots in there, next to `vm-resources.md` (Phase 09).

2. Write `docs/src/configuration/graphics-and-games.md`. Suggested
   shape:

   ```markdown
   # Graphics and games inside VMs

   This page documents what graphical workloads work inside
   steampipe VMs and what doesn't.

   ## Summary

   | VM class | What runs | GPU needed? | Default hypervisor |
   |----------|-----------|-------------|---------------------|
   | `services.steampipe-cluster.loginRunners` | Steam login wizard (CEF/GTK) | No (llvmpipe) | QEMU (no virtio-gpu) |
   | `services.steampipe-cluster.runners` | `cluster-ctl steam warm` (`DisplayMode::Headless`) | No (no compositor) | cloud-hypervisor |
   | `mkTestCluster` headless flavours | Project test binary (server, simulation, headless game) | No | cloud-hypervisor |
   | `mkTestCluster` graphical flavours | Project test binary with rendered output (UI, visual fixtures, 2D/3D games) | **Yes** | crosvm (deprecated, see below) |

   ## Host-module classes: software rendering

   The login and warm flows do not need GPU acceleration. The
   Steam login wizard is CEF + GTK; it renders on Mesa's
   `llvmpipe` (software OpenGL) without complaint. sway runs with
   `WLR_BACKENDS=headless` (no DRM, no virtio-gpu); wayvnc
   captures the headless output via `wlr-screencopy`.

   This is by design after the 2026-05-26 hypervisor restructure:
   the QEMU virgl/Venus stack proved unstable on AMD Mesa under
   real Steam GUI load (probe results recorded in
   `docs/src/planning/hypervisor-restructure/01-validate-qemu-virgl-on-atlas-results.md`).
   Removing the graphics device entirely from host-module classes
   eliminated the crash surface.

   Trade-off: Steam login UI runs at ~5-10 FPS instead of 60.
   Acceptable for a one-time wizard.

   ## Project-flake graphical flavours: crosvm (deprecated)

   Projects that need GPU-rendered workloads inside the VM —
   chessbender's card-game UI, regicide's visual fixtures,
   fragpipe's render harness — pin `[cluster] hypervisor =
   "crosvm"` in their `steampipe.toml`. This selects the
   steampipe-patched crosvm overlay that:

   - Installs crosvm's source seccomp policies and pre-compiles
     them with the same `compile_seccomp_policy.py` crosvm
     itself uses.
   - Widens `common_device.policy` / `gpu_common.policy` to
     allow `PR_SET_NAME` (Rust thread name on glibc ≥ 2.40)
     and `MADV_GUARD_INSTALL` (kernel ≥ 6.13 stack guard pages).
   - Adds `gfxstream` / `aemu` buildInputs for Mesa Venus.

   Without those patches, plain `nixpkgs.crosvm` SIGSYS-crashes
   on modern (kernel ≥ 6.13, glibc ≥ 2.40) hosts.

   ### Known limitation: heavy Vulkan loads crash crosvm

   crosvm + virgl + Vulkan is empirically brittle under sustained
   load (the original 2026-05-26 vm-2 incident: `vcpu hit
   unknown error: Bad address` 79 ms after Steam reached for
   `GLSL feature level 430`). For card-game-class workloads
   (~100 verts/frame, no shaders) the path is stable; for
   3D/Vulkan-heavy games, expect occasional VCPU EFAULTs.

   Mitigation: retry the failing test. Cluster-ctl's lease and
   reset machinery handles repeated VM teardown/rebuild cleanly.

   ### Deprecation horizon

   `[cluster] hypervisor = "crosvm"` emits a deprecation warning
   at module eval time. The path is scheduled for removal in
   steampipe 0.4 (target window: 2026-Q4 nixpkgs), pending
   upstream stabilisation of:

   - virglrenderer + Mesa Venus on AMD (the segfaults from
     Phase 01's probe were in virgl_render_server under libc).
   - QEMU 11.x + Mesa 27.x + virgl Venus on a headless host
     without a display backend.

   When the upstream stack matures, a successor research dossier
   will evaluate QEMU + Venus (or GPU passthrough, or another
   shape) as the project-flake graphical path.

   ## What if I need a heavy 3D game right now?

   Three options, none ideal:

   1. **Accept occasional crashes** and retry. crosvm-virgl
      survives most card-game-class workloads; heavy 3D crashes
      maybe 1-in-N runs.
   2. **GPU passthrough** (manual, not provided): pass a DRM
      render node to one VM via vfio. Loses the multi-VM
      cluster pattern (one game at a time).
   3. **Software rendering with explicit `graphics = false`** in
      the project's `steampipe.toml`. Game falls back to
      llvmpipe; FPS drops by ~10×; unplayable for most 3D games
      but fine for headless game-logic tests.

   ## See also

   - [Hypervisor + compositor design](../planning/hypervisor-restructure-design.md)
   - [Hypervisor + compositor research](../planning/hypervisor-restructure-research.md)
   - [VM resources](./vm-resources.md) — memory + disk budgets.
   - [Visual testing](./visual-testing.md) — fixture conventions.
   - Phase 01 results:
     [01-validate-qemu-virgl-on-atlas-results.md](../planning/hypervisor-restructure/01-validate-qemu-virgl-on-atlas-results.md)
     — the empirical probe that drove this docs page.
   ```

3. Index the new page in `docs/src/SUMMARY.md` under
   Configuration, alongside `vm-resources.md`:

   ```markdown
   - [Graphics and games](./configuration/graphics-and-games.md)
   ```

4. Verify the rendered book builds cleanly:

   ```bash
   mdbook build docs/
   ```

5. Commit:

   ```
   docs: explain graphics-and-games scope for steampipe VMs

   Records the post-restructure shape: host-module classes
   software-render reliably; project-flake graphical workloads
   keep the deprecated crosvm escape hatch until upstream
   virgl/Venus stabilises. Names the known crash class
   (vm-2 incident, 2026-05-26), the mitigations, and the
   2026-Q4-nixpkgs re-evaluation horizon.
   ```

## Acceptance criteria

- [ ] `docs/src/configuration/graphics-and-games.md` exists with
      the sections: Summary table, Host-module classes,
      Project-flake graphical flavours, Known limitation,
      Deprecation horizon, What-if-I-need-heavy-3D, See also.
- [ ] `docs/src/SUMMARY.md` indexes `graphics-and-games.md` under
      Configuration.
- [ ] `mdbook build docs/` produces no warnings or broken links.
- [ ] The new page links to: the design doc, the research
      dossier, `vm-resources.md`, `visual-testing.md`, and
      Phase 01's results doc.
- [ ] The page does **not** prescribe a successor path (no
      premature commitment to passthrough / Venus / wait).

## Files likely touched

- `docs/src/configuration/graphics-and-games.md` — new page.
- `docs/src/SUMMARY.md` — index entry.

## Pitfalls

- **Don't editorialise on hypervisor choices**. State facts; let
  the reader pick.
- **Don't link from inside `cluster-vm-base.nix` comments or
  module description strings**. Those are eval-time text and
  won't render as clickable links. Docs cross-references stay in
  Markdown.
- **Don't recommend a successor**. The follow-up research happens
  in 2026-Q4; making a premature commitment locks the project
  into a path that may not hold up under the next round of
  empirical testing.
- **Hyperlinks are relative**. From
  `docs/src/configuration/graphics-and-games.md`, the design doc
  is at `../planning/hypervisor-restructure-design.md`.

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Pivot 2026-05-26").
- Phase 01 results doc:
  [01-validate-qemu-virgl-on-atlas-results.md](./01-validate-qemu-virgl-on-atlas-results.md).
- vm-2 VCPU EFAULT incident: research dossier
  [../hypervisor-restructure-research.md](../hypervisor-restructure-research.md)
  "Current Reality" → vm.log evidence.
