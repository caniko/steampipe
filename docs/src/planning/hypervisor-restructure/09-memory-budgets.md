# Phase 09 — Memory budgets: balloon + deflateOnOOM + cloud-hypervisor initial claim

> **Recommended Codex model: GPT 5.5 medium**
>
> The numbers come from the design, but the agent has to verify the
> microvm.nix option names against the pinned version, place the
> per-class discriminator on the existing `shareSteamState`/
> `loginStateDir != null` switch (no new parameter), and write the
> operator-facing `vm-resources.md` page. Multiple files, must stay
> consistent. `low` would skip the docs page; `high` is overkill for
> ~30 lines of Nix + one markdown page.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase:

1. Every QEMU/cloud-hypervisor VM (login + runtime) has
   `microvm.balloon = true; deflateOnOOM = true;`.
   cloud-hypervisor VMs also get a class-appropriate
   `initialBalloonMem` (1024 MiB for login, 512 MiB for runtime).
   QEMU keeps `initialBalloonMem = 0` because the pinned microvm.nix
   QEMU runner rejects non-zero initial balloon memory.
2. Existing `mem` defaults remain (4096 login, 2048 runtime).
3. `docs/src/configuration/vm-resources.md` exists, is indexed in
   `SUMMARY.md`, and documents the memory contract + the
   host-side KSM requirement + the `cluster-ctl reset --home`
   helper + the `cluster-ctl admission status` helper.
4. No VM OOM-crashes during a `cluster-ctl steam test` cycle on
   atlas.

## Why this matters now

Phases 02 and 08 put both new hypervisors in place with the
default mem values (4096/2048). Those numbers can be exhausted by
Steam GUI's transient spikes (game install resolution, CEF child
process explosion). Without `deflateOnOOM`, a transient spike
trips the guest's OOM-killer and a critical Steam process dies.

Balloon + `deflateOnOOM` together provide the reliability
guarantee the user pinned as priority #1: the host can reclaim
idle VM pages, and on guest pressure the balloon deflates
automatically before the OOM-killer fires.

This is the phase where the design's memory budget table goes
from "documented intent" to "live behaviour".

## Out of scope

- Adding new NixOS module options for ballooning. The settings are
  computed by `nix/lib/microvm-memory-budget.nix` and applied by
  `microvm-runner.nix`; no new option surface is added.
- virtio-mem hotplug. The design explicitly chose ballooning over
  hotplug-mem for reliability reasons.
- Tuning watermarks of the memory-admission gate from Phase 06.
  Those are host-side; this phase is guest-side.
- Touching crosvm-pinned configs. crosvm remains a deprecated
  escape hatch; this phase does not change its balloon settings.

## Plan

1. **Verify microvm.nix option names** in the pinned source:

   ```bash
   grep -nE 'balloon|deflateOnOOM|initialBalloonMem' \
     /nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/nixos-modules/microvm/options.nix
   ```

   Expected matches:
   - `microvm.balloon` (bool)
   - `microvm.deflateOnOOM` (bool)
   - `microvm.initialBalloonMem` (int, MiB)

   Also verify hypervisor support in the pinned runners:

   ```bash
   grep -nE 'qemu does not support initialBalloonMem|initialBalloonMem|--balloon' \
     /nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/lib/runners/{qemu,cloud-hypervisor}.nix
   ```

   Current pinned result: QEMU throws when `initialBalloonMem != 0`;
   cloud-hypervisor supports `--balloon size=...`.

   If any name differs in the canix-pinned microvm.nix version,
   adjust below.

2. **Add the budget helper** in
   [nix/lib/microvm-memory-budget.nix](../../../nix/lib/microvm-memory-budget.nix):

   ```nix
   {
     hypervisor,
     shareSteamState,
   }: let
     supportsBalloon = builtins.elem hypervisor ["qemu" "cloud-hypervisor"];
     supportsInitialBalloon = hypervisor == "cloud-hypervisor";
   in {
     inherit supportsBalloon supportsInitialBalloon;

     balloon = supportsBalloon;
     deflateOnOOM = supportsBalloon;
     initialBalloonMem =
       if supportsInitialBalloon
       then
         if shareSteamState
         then 1024
         else 512
       else 0;
   }
   ```

3. **Inject memory settings** in
   [nix/lib/microvm-runner.nix](../../../nix/lib/microvm-runner.nix):

   ```nix
   memoryBudget = import ./microvm-memory-budget.nix {
     inherit shareSteamState;
     inherit (flavor) hypervisor;
   };

   microvm = {
     # ... existing hypervisor / graphics / extraArgs blocks ...

     mem =
       if flavor.memory != null
       then flavor.memory
       else vm.memory;

     balloon = lib.mkIf memoryBudget.balloon true;
     deflateOnOOM = lib.mkIf memoryBudget.deflateOnOOM true;
     # The pinned microvm.nix QEMU runner rejects non-zero
     # initialBalloonMem; cloud-hypervisor supports it.
     initialBalloonMem =
       lib.mkIf memoryBudget.supportsInitialBalloon
       memoryBudget.initialBalloonMem;

     # ... existing vcpu / vsock / interfaces / volumes / shares ...
   };
   ```

   Reuse `loginStateDir != null` as the login-vs-runtime
   discriminator (same as Phase 03's volume-size split).

4. **Verify the option set composes** with the existing
   `mkIf`-guarded `crosvm.extraArgs` and `qemu.extraArgs`:

   ```bash
   nix eval --raw .#nixosConfigurations.<test-host>.config.microvm.balloon
   # Should print "true" for qemu/chv configs, "false" for crosvm.
   nix eval --raw .#nixosConfigurations.<test-host>.config.microvm.initialBalloonMem
   # Should print "0" for qemu, "1024" for cloud-hypervisor login,
   # and "512" for cloud-hypervisor runtime.
   ```

5. **Add a flake check** that captures the balloon defaults:

   ```nix
   module-memory-budgets-eval =
     pkgs.runCommand "module-memory-budgets-eval" {
       ...
     } ''
      # Assert the shared budget helper keeps QEMU on balloon +
      # deflate-on-OOM with initialBalloonMem=0, gives cloud-hypervisor
      # 1024/512 initial balloon by class, and leaves crosvm unchanged.
       ...
     '';
   ```

   Mirror the existing `module-loginrunners-with-key-evals` check
   structure.

6. **Write `docs/src/configuration/vm-resources.md`**:

   ```markdown
   # VM resources

   This page documents how steampipe sizes the per-VM memory and
   disk envelopes, the host-side primitives that keep them honest,
   and the operator tools for inspection and recovery.

   ## Memory contract

   Each VM is sized with:

   | VM class | Hypervisor class | mem (MiB) | initial balloon (MiB) | balloon | deflate-on-OOM |
   |----------|-------------------|-----------|-----------------------|---------|----------------|
   | Login (graphical, Steam GUI) | QEMU | 4096 | 0 | yes | yes |
   | Login (cloud-hypervisor escape hatch) | cloud-hypervisor | 4096 | 1024 | yes | yes |
   | Runtime (headless) | cloud-hypervisor | 2048 | 512 | yes | yes |

   The pinned microvm.nix QEMU runner rejects non-zero
   `initialBalloonMem`, so QEMU login runners enable virtio-balloon
   and `deflateOnOOM` without an initial claim. cloud-hypervisor
   supports an initial balloon size, so runtime runners start with a
   512 MiB claim and any cloud-hypervisor login runner starts with a
   1024 MiB claim.

   **Why balloon + deflateOnOOM rather than hotplug-mem?** The
   guest sees its full `mem` allocation immediately. The host
   reclaims unused pages via balloon free-page-reporting; when
   the guest hits memory pressure, the balloon deflates
   automatically before the OOM-killer trips. This keeps the
   reliability guarantee (no OOM-crash) while still letting the
   host reclaim idle pages.

   ## Host-side requirement: KSM

   Enable `hardware.ksm.enable = true;` on the host (canix-side).
   KSM (kernel same-page merging) deduplicates identical pages
   across VMs — Mesa, libc, kernel images — recovering ~10-30%
   of resident memory on a cluster running similar guests.

   ## Disk

   - Login-runner home image: 2 GiB. Holds Steam config + login
     state only.
   - Runtime-runner home image: 8 GiB. Holds game content the
     harness deploys.

   ## Operator tools

   ### `cluster-ctl reset <vm> --home`

   Removes the on-disk home image for `<vm>` so the next boot
   starts fresh. Refuses if the VM is currently running (lease
   guard). Use after shrinking the home-image default (e.g. to
   reclaim disk from old 8 GiB login-runner images).

   ### `cluster-ctl admission status`

   Prints the host-side memory admission gate's current sample
   plus the configured high/low watermarks. The gate (powered by
   the `memory-admission` crate) sits in front of every parallel
   VM startup; it blocks new admissions when free host RAM dips
   below the high watermark and releases above the low watermark.

   ## Failure modes

   - **Guest OOM**: balloon deflates first; if memory pressure
     persists, the guest's OOM-killer fires. Visible in
     `vm.log` as `Out of memory: Killed process ...`.
   - **Host OOM** (parallel VM startup overshoot): the host
     OOM-killer claims one of the hypervisor processes; the
     memory-admission gate is the primary defense.
   - **KSM disabled**: the table's "host reclaimable" numbers
     are overstated; the cluster still works but uses more
     resident memory than projected.

   ## See also

   - [Hypervisor + compositor design](../planning/hypervisor-restructure-design.md)
   - [Hypervisor + compositor research](../planning/hypervisor-restructure-research.md)
   ```

7. **Index `vm-resources.md`** in
   [docs/src/SUMMARY.md](../../SUMMARY.md), under Configuration:

   ```markdown
   - [VM resources](./configuration/vm-resources.md)
   ```

8. **Move the `cluster-ctl reset --home` documentation** from
   `vm-lifecycle.md` (where Phase 03 parked it) into the new
   `vm-resources.md` page. Update any cross-references.

9. **Verify**:

   ```bash
   nix flake check --no-build
   nix build .#checks.x86_64-linux.module-memory-budgets-eval
   mdbook build docs/
   cargo test --workspace
   ```

10. **Smoke-test on atlas** (the load-bearing acceptance check):

   - Bump canix's steampipe input pin to this phase's commit.
     Commit + rebuild atlas.
   - Verify the active vm-1 runner script includes
     `-device virtio-balloon` (QEMU side) or the equivalent CHV
     command-line surface (`--balloon size=512M,...` for runtime).
   - Run `cluster-ctl steam test` (any project) — should complete
     without any VM OOM-crashing.
   - Run a synthetic memory-soak inside a guest:
     ```bash
     ssh cluster@10.0.100.1 'stress-ng --vm 1 --vm-bytes 3G --vm-keep --timeout 60s'
     ```
     Verify the guest's free memory drops, the balloon deflates
     (`cat /proc/meminfo` shows the balloon retracting), and the
     guest doesn't OOM-kill the stress-ng process.

11. Commit:

    ```
    microvm-runner: enable virtio-balloon + deflateOnOOM per VM class

    QEMU login runners: mem=4096, balloon=true, deflateOnOOM=true,
    initialBalloonMem=0 due pinned microvm.nix QEMU limitations.
    cloud-hypervisor runtime runners: mem=2048, initialBalloonMem=512.

    Reliability priority: deflateOnOOM rescues the guest before
    the OOM-killer fires under transient spikes (Steam GUI's
    game-install spike, CEF child explosion). Host can reclaim
    idle pages via balloon free-page-reporting; KSM (canix-side)
    deduplicates identical pages across VMs for additional
    savings.

    Settings are computed by nix/lib/microvm-memory-budget.nix and
    applied by microvm-runner; non-zero initialBalloonMem is
    cloud-hypervisor-only because the pinned microvm.nix QEMU runner
    rejects it.

    New page docs/src/configuration/vm-resources.md documents the
    memory contract, KSM requirement, and operator tools.
    ```

## Acceptance criteria

- [ ] `nix eval .#nixosConfigurations.<host>.config.microvm.balloon`
      returns `true` for both login and runtime VM configs (on
      qemu / cloud-hypervisor).
- [ ] Same for `deflateOnOOM` → `true`.
- [ ] `initialBalloonMem` is `0` for QEMU login VMs, `1024` for
      cloud-hypervisor login VMs, and `512` for cloud-hypervisor
      runtime VMs.
- [ ] `nix flake check --no-build` is green; the new
      `module-memory-budgets-eval` check builds.
- [ ] `docs/src/configuration/vm-resources.md` exists with the
      memory contract table, KSM requirement, and operator-tools
      sections.
- [ ] `docs/src/SUMMARY.md` indexes `vm-resources.md` under
      Configuration.
- [ ] `mdbook build docs/` is clean.
- [ ] On atlas (after canix rebuild): `cluster-ctl steam test`
      completes without any VM OOM-crashing.
- [ ] Synthetic `stress-ng --vm 1 --vm-bytes 3G` inside a guest
      causes the balloon to deflate (visible in `/proc/meminfo`)
      and the stress-ng process does not get OOM-killed.

## Files likely touched

- `nix/lib/microvm-runner.nix` — balloon block.
- `nix/lib/microvm-memory-budget.nix` — shared memory contract helper.
- `nix/flake/checks.nix` — new `module-memory-budgets-eval`.
- `docs/src/configuration/vm-resources.md` — new page.
- `docs/src/SUMMARY.md` — index entry.
- `docs/src/configuration/vm-lifecycle.md` — relocate the
  `cluster-ctl reset --home` note that Phase 03 parked here.

## Pitfalls

- **Option name drift**: if microvm.nix renamed `initialBalloonMem`
  → `initial_balloon_mem` or split into per-hypervisor options,
  the literal name above fails eval. Step 1 catches this.
- **QEMU initial balloon**: the pinned microvm.nix QEMU runner throws
  when `initialBalloonMem != 0`. Keep non-zero initial balloon memory
  cloud-hypervisor-only unless microvm.nix is upgraded and the runner
  support is re-verified.
- **`initialBalloonMem` too high**: setting it equal to `mem` means
  the guest starts with effectively zero memory; sshd may not even
  come up. Cap at ≤ 25% of `mem` to be safe; 1024/4096 and 512/2048
  are both 25%.
- **`deflateOnOOM` race**: there's a known concern that the
  balloon-deflate signal can arrive after the OOM-killer has already
  fired. cloud-hypervisor mitigates this with a conservative 25%
  initial claim. QEMU cannot use that pinned option, so the mitigation
  there is full boot memory plus deflate-on-OOM for host-triggered
  balloon pressure.
- **stress-ng synthetic load**: the package may not be in the
  VM by default. Phase 09 doesn't add it; the smoke-test can
  use `dd if=/dev/zero of=/dev/null bs=1G count=3` as a
  lower-fidelity substitute, or temporarily `nix-env -i
  stress-ng` inside the test guest.
- **Don't add KSM enable to steampipe**: KSM is canix-side
  (Phase 07). Document the requirement in `vm-resources.md`,
  don't try to set it from the steampipe NixOS module
  (steampipe's module configures VMs, not the host).
- **Existing 8 GiB home images don't shrink retroactively**:
  same caveat as Phase 03. The `cluster-ctl reset --home` tool
  is for that.

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Final Memory Budgets", "`nix/lib/microvm-runner.nix` Changes").
- microvm.nix balloon options:
  `/nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/nixos-modules/microvm/options.nix`
  L125-L172.
- Predecessor Phase 03 (home image shrink) and Phase 06
  (memory-admission gate) — this phase complements both.
- Canix-side KSM enable: Phase 07.
