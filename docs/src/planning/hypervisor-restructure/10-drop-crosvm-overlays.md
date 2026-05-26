# Phase 10 — Delete the `cloud-hypervisor-graphics` override + `__steampipeOverlay` marker

> **Recommended Codex model: GPT 5.5 medium**
>
> Narrow deletion: drop the `cloud-hypervisor-graphics` cargo
> vendor hash override (no longer used by anything in the
> supported configuration) and the `__steampipeOverlay` marker
> warning/check. **The crosvm overlay block stays** — it backs the
> project-flake graphical escape hatch. The agent must audit
> exhaustively to ensure nothing else still depends on the deleted
> symbols. `low` would skip the audit; `high` is overkill for ~25
> lines of deletion.

## Risk profile

- **F1**: A downstream consumer (chessbender, regicide, fragpipe)
  still references `cloud-hypervisor-graphics` from their own
  flake. Their build breaks on the next steampipe input bump.
  Audit step catches this.
- **F2**: A flake check or eval somewhere still asserts on
  `__steampipeOverlay`. The grep audit catches this.
- **F3**: Project-flake graphical workloads stop working because
  a previously-implicit dependency on the `-graphics` variant of
  cloud-hypervisor got dropped silently. Unlikely (graphical
  workloads use crosvm); audit confirms.

## Strategy

Single commit. Each deletion is independently revertible via the
preserved git history. The crosvm overlay block stays; only the
two narrower artefacts go.

## Rollback drill

`git revert <this-commit>` restores both deletions. SLA: 2 minutes
to revert + 10 minutes to rebuild atlas back to the previous
closure.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase:

1. `nix/flake/overlay.nix` no longer contains the
   `cloud-hypervisor-graphics = prev.cloud-hypervisor-graphics.overrideAttrs ...`
   block (~10 lines).
2. `nix/module.nix` no longer references
   `pkgs.crosvm.passthru.__steampipeOverlay` (the marker warning
   in the `warnings` block).
3. `nix/flake/checks.nix` no longer contains the
   `__steampipeOverlay`-asserting marker check.
4. **The `crosvm = prev.crosvm.overrideAttrs ...` block in
   `nix/flake/overlay.nix` is preserved** — project-flake
   graphical workloads pinning `[cluster] hypervisor = "crosvm"`
   keep getting the patched build.
5. `nix flake check --no-build` is green.
6. `cluster-ctl steam login vm-N` + `cluster-ctl steam warm` still
   pass on atlas — no regression.
7. Project-flake graphical workloads (e.g. chessbender if it pins
   crosvm) still build and run.

## Why this matters now

After Phases 02, 08, 09 land and prove out on atlas:

- The `cloud-hypervisor-graphics` override exists only because
  spectrum-os's patched CHV variant drifts behind nixos-unstable's
  rustPlatform vendoring. Plain `cloud-hypervisor` (used for
  headless runtime VMs) doesn't need the override.
- The `__steampipeOverlay` marker on `pkgs.crosvm` exists to warn
  when a downstream overlay shadows our patched crosvm and a
  modern-kernel SIGSYS becomes likely. Since the default path no
  longer uses the patched crosvm (host-module classes are QEMU
  + cloud-hypervisor), the marker warning is less load-bearing.
  Operators still pinning crosvm get the existing deprecation
  warning from Phase 08, which carries clearer migration
  guidance.

The **crosvm overlay itself stays**: project-flake graphical
workloads (chessbender, regicide) pin `[cluster] hypervisor =
"crosvm"` in `steampipe.toml` and depend on the patched build for
their visual tests. Until upstream virglrenderer + Mesa + QEMU's
virgl Venus stabilises (target window: 2026-Q4 nixpkgs), the
crosvm path is the only working graphical-test option.

## Out of scope

- Deleting the `crosvm = prev.crosvm.overrideAttrs ...` block.
  Stays for the deprecated escape hatch.
- Deleting `crosvm.extraArgs` in `nix/lib/microvm-runner.nix`.
  Stays under `lib.mkIf (flavor.hypervisor == "crosvm")` for
  project-flake users.
- Deleting `patchCrosvmGpuParams` in `nix/lib/test-cluster.nix`.
  Stays for the same reason.
- Removing `crosvm` as a valid value from
  `loginRunners.hypervisor` / `runners.hypervisor`. The escape
  hatch stays; only the marker warning goes (the Phase-08
  deprecation warning is the load-bearing signal now).
- Adding new flake checks. Phase 08's `module-qemu-loginrunners-eval`
  and Phase 02's `module-cloud-hypervisor-runners-eval` cover the
  supported paths.

## Plan

1. **Audit `__steampipeOverlay` references**:

   ```bash
   grep -rn "__steampipeOverlay" --include="*.nix" --include="*.rs" .
   ```

   Expected match sites (verify count, none unexpected):
   - `nix/flake/overlay.nix` — sets the marker on both crosvm and
     CHV-graphics derivations.
   - `nix/flake/checks.nix` line ~373-383 — the marker assertion.
   - `nix/module.nix` line 435-436 — the warning that fires when
     the marker is absent.

   Anything outside these sites is a hidden coupling — investigate.

2. **Audit `cloud-hypervisor-graphics` references**:

   ```bash
   grep -rn "cloud-hypervisor-graphics" --include="*.nix" --include="*.rs" .
   ```

   Expected match sites:
   - `nix/flake/overlay.nix` (the override block; deleting).
   - `nix/flake/checks.nix` line ~378 (inside the marker check;
     deleting).
   - Possibly comments. Verify each occurrence; non-comment
     references outside the deletion targets are blockers — chase
     them before proceeding.

3. **Verify the gating phases shipped and verified**:

   - Phase 02 (cloud-hypervisor wired): commit on `trunk`, atlas
     bumped + rebuilt.
   - Phase 08 (defaults flipped): commit on `trunk`, atlas
     bumped + rebuilt; `cluster-ctl steam login vm-N` survives.
   - Phase 09 (memory budgets): commit on `trunk`, atlas bumped +
     rebuilt; `cluster-ctl steam warm vm-N` survives.

   If any of those checks fail, stop and report.

4. **Delete the `cloud-hypervisor-graphics` block** in
   [nix/flake/overlay.nix](../../../nix/flake/overlay.nix). Lines
   roughly 19-26:

   ```nix
   # before
   nixpkgs.lib.composeExtensions microvm.overlay (final: prev: {
     cloud-hypervisor-graphics = prev.cloud-hypervisor-graphics.overrideAttrs (oldAttrs: {
       cargoDeps = final.rustPlatform.fetchCargoVendor {
         inherit (oldAttrs) src patches;
         hash = "sha256-YYrzd44DbaRT3jieclHrGoDLT14vTzTtNwIRLCtv3Ko=";
       };
       passthru = (oldAttrs.passthru or {}) // {__steampipeOverlay = true;};
     });

     crosvm = prev.crosvm.overrideAttrs (oldAttrs: { ... });
   })

   # after (CHV block removed, crosvm block stays)
   nixpkgs.lib.composeExtensions microvm.overlay (final: prev: {
     crosvm = prev.crosvm.overrideAttrs (oldAttrs: { ... });
   })
   ```

5. **Strip the `__steampipeOverlay` passthru** from the remaining
   `crosvm` override in `nix/flake/overlay.nix` (the marker is no
   longer needed; Phase-08's deprecation warning is the operator
   signal):

   ```nix
   # in the crosvm block
   passthru = (oldAttrs.passthru or {}) // {__steampipeOverlay = true;};
   # → delete this line
   ```

6. **Delete the overlay-marker warning** in
   [nix/module.nix:430-436](../../../nix/module.nix#L430-L436):

   ```nix
   # before
   warnings = lib.optional ...
     ++ lib.optional (runnersEnabled && !(pkgs.crosvm.passthru.__steampipeOverlay or false))
        "services.steampipe-cluster: pkgs.crosvm lacks the steampipe overlay's `__steampipeOverlay` marker. A downstream overlay has shadowed our patched crosvm; login VMs may hit SIGSYS on Linux >= 6.13.";

   # after (overlay-marker warning removed; Phase-08 deprecation warnings remain)
   warnings = lib.optional ...  # other warnings stay
   ```

7. **Delete the overlay-marker flake check** in
   [nix/flake/checks.nix](../../../nix/flake/checks.nix):

   ```bash
   grep -n "__steampipeOverlay\|crosvmMarker\|moduleCrosvmOverlayMarker" nix/flake/checks.nix
   ```

   Delete the matched check block(s). The exact name and line
   range may differ; verify before deleting.

8. **Verify the build still composes**:

   ```bash
   nix flake check --no-build
   nix build .#checks.x86_64-linux.module-runners-eval
   nix build .#checks.x86_64-linux.module-qemu-loginrunners-eval
   nix build .#checks.x86_64-linux.module-cloud-hypervisor-runners-eval
   nix build .#checks.x86_64-linux.module-memory-budgets-eval
   nix build .#checks.x86_64-linux.module-crosvm-deprecation-warning
   ```

   All must succeed.

9. **Atlas smoke-test** (load-bearing):

   - Bump canix's steampipe input pin to this commit.
   - `sudo nixos-rebuild switch --flake /data/nvme0/can/Projects/canix#atlas`.
   - `cluster-ctl steam login vm-N` — passes.
   - `cluster-ctl steam warm vm-N` — passes for every credentialed VM.
   - Confirm `/etc/steampipe/login-runners/vm-N/bin/microvm-run`
     references `qemu-system-x86_64` (unchanged from Phase 08).
   - Confirm `/etc/steampipe/runners/vm-N/bin/microvm-run`
     references `cloud-hypervisor` (not `-graphics`).

10. **Optional: build a project-flake graphical workload** that
    pins `[cluster] hypervisor = "crosvm"`. If any chessbender /
    regicide / fragpipe project is reachable, do a smoke build of
    its `cluster-*-up` attribute to confirm crosvm still works:

    ```bash
    cd /data/nvme0/can/Projects/chessbender  # or similar
    nix build .#cluster-1v1-up --no-link 2>&1 | tail -5
    ```

    Expected: build succeeds; the runner script references the
    patched `crosvm` overlay (`grep crosvm $(readlink result)/bin/microvm-run`).

11. Commit:

    ```
    flake: delete cloud-hypervisor-graphics override and overlay-marker

    After Phases 02/08/09 (cloud-hypervisor wired, defaults flipped
    to QEMU/CHV without virtio-gpu, memory budgets shipped, all
    verified live on atlas), the cloud-hypervisor-graphics override
    has no consumer in the supported configuration and the
    __steampipeOverlay marker warning is superseded by the
    Phase-08 deprecation warning.

    Deletes:
    - nix/flake/overlay.nix cloud-hypervisor-graphics override
      (cargo vendor hash override; no longer used).
    - nix/flake/overlay.nix crosvm.passthru.__steampipeOverlay
      marker (the warning it backed is removed).
    - nix/module.nix __steampipeOverlay marker warning.
    - nix/flake/checks.nix overlay-marker check.

    Keeps:
    - nix/flake/overlay.nix crosvm.overrideAttrs block — backs the
      deprecated escape hatch for project-flake graphical
      workloads. Removal awaits upstream virgl/Venus
      stabilisation (target: 2026-Q4 nixpkgs).
    - nix/lib/microvm-runner.nix crosvm.extraArgs.
    - nix/lib/test-cluster.nix patchCrosvmGpuParams.
    - "crosvm" as a valid hypervisor value (with Phase-08
      deprecation warning).
    ```

## Acceptance criteria

- [ ] `grep -n "__steampipeOverlay" nix/` returns zero matches.
- [ ] `grep -n "cloud-hypervisor-graphics" nix/` returns zero
      matches inside `nix/` (comments may remain in `docs/`).
- [ ] `nix/flake/overlay.nix` still contains the `crosvm =
      prev.crosvm.overrideAttrs ...` block (verifiable by
      `grep -c "crosvm = prev.crosvm.overrideAttrs"`).
- [ ] `nix/lib/microvm-runner.nix` still contains
      `crosvm.extraArgs` (under `lib.mkIf`).
- [ ] `nix/lib/test-cluster.nix` still contains
      `patchCrosvmGpuParams`.
- [ ] `nix/module.nix` warnings block no longer references
      `__steampipeOverlay`; Phase-08 deprecation warnings remain.
- [ ] `nix/flake/checks.nix` no longer contains a check that
      asserts on `__steampipeOverlay`.
- [ ] `nix flake check --no-build` is green.
- [ ] `cluster-ctl steam login vm-N` on atlas (after canix bumps
      and rebuilds) passes without VCPU error.
- [ ] `cluster-ctl steam warm vm-N` on atlas passes for every
      credentialed VM.
- [ ] On atlas:
      `/etc/steampipe/login-runners/vm-N/bin/microvm-run` contains
      `qemu-system-x86_64`;
      `/etc/steampipe/runners/vm-N/bin/microvm-run` contains
      `cloud-hypervisor` (not `cloud-hypervisor-graphics`).
- [ ] A project-flake `cluster-*-up` build with `[cluster]
      hypervisor = "crosvm"` still succeeds (smoke-tested against
      at least one project).
- [ ] `mdbook build docs/` is clean.

## Files likely touched

- `nix/flake/overlay.nix` — delete CHV-graphics block; strip
  `__steampipeOverlay` from crosvm passthru.
- `nix/module.nix` — remove `__steampipeOverlay` marker warning.
- `nix/flake/checks.nix` — remove marker check.

## Failure modes and recoveries

- **F1** (downstream consumer pins crosvm and rebuilds — should be
  fine): their build still uses the patched crosvm overlay, no
  change. If they pinned `cloud-hypervisor-graphics` directly (not
  via microvm.nix), their build fails. Recovery: revert this
  phase; they migrate off CHV-graphics on their own timeline.
- **F2** (overlay residue): the grep audit (steps 1-2) catches
  missed references. If found post-commit, follow up with a
  trivial revert.
- **F3** (project-flake graphical build regression): step 10
  catches this. If it fails, revert this phase; investigate
  what the marker was load-bearing for that the audit missed.

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Pivot 2026-05-26", "Overlay Deletions (Wave 3, scoped)").
- Phase 02 (cloud-hypervisor wired):
  [02-add-cloud-hypervisor-headless.md](./02-add-cloud-hypervisor-headless.md).
- Phase 08 (defaults flipped):
  [08-flip-login-default-to-qemu.md](./08-flip-login-default-to-qemu.md).
- Why crosvm overlay stays:
  [11-document-graphics-and-games.md](./11-document-graphics-and-games.md)
  — project-flake graphical workloads need it until upstream
  virgl/Venus stabilises.
- Upstream PRs the crosvm overlay was tracking (will eventually
  retire it):
  - NixOS/nixpkgs#517363 (install policies + widenings).
  - microvm-nix/microvm.nix#516 (drop `--seccomp-log-failures`
    version-gate).
