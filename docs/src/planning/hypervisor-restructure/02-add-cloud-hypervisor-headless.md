# Phase 02 — Add cloud-hypervisor as a supported headless hypervisor

> **Recommended Codex model: GPT 5.5 medium**
>
> One module option enum-widening, one new flake check, one
> conditional `microvm.cloud-hypervisor.package` pin. Mechanical
> given the design; the only judgement call is verifying microvm.nix
> doesn't default the cloud-hypervisor package to the `-graphics`
> variant when `graphics.enable = false`. `low` would copy-paste the
> existing `module-runners-eval` check without verifying the package
> selection; `high` is overkill for ~40 lines of Nix.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase, operators can set
`services.steampipe-cluster.runners.hypervisor = "cloud-hypervisor";`
and get a working runtime VM runner built from plain upstream
`pkgs.cloud-hypervisor` (not the spectrum-os `-graphics` variant).
A flake check validates eval. **The default does not yet flip** —
that's Phase 09 (after Phase 08's QEMU login proves out and the
overall plan is moving toward Wave 3).

## Why this matters now

Plain cloud-hypervisor is the LF-owned Rust hypervisor the user
asked for on the headless side. Adding it as a supported value
(without changing defaults) lets Phase 09 lean on it for memory
budgets without first having to introduce the hypervisor itself.
Separating "add support" from "flip default" keeps the diff small
and reversible.

Today the module's `runners.hypervisor` is `"crosvm"` by default,
and the only other option-accepting value in `host-runners.nix:12`
is whatever microvm.nix supports. The module already allows any
string; what it doesn't do is *verify* that cloud-hypervisor
specifically evals cleanly through this codebase.

The dossier's blocker B2 flagged that microvm.nix's default
`microvm.cloud-hypervisor.package` may resolve to the
`-graphics` variant even when `graphics.enable = false`. Reference:
[microvm.nix options.nix:763-772](file:///nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/nixos-modules/microvm/options.nix#L763-L772).
This phase confirms the selection and pins explicitly if needed.

## Out of scope

- Flipping any default (`loginRunners.hypervisor`,
  `runners.hypervisor`, `mkTestCluster` defaults). That's Phase 08.
- Memory budgets (balloon, deflateOnOOM). That's Phase 09.
- Deleting the `cloud-hypervisor-graphics` overlay. That's Phase 10.
- Touching graphical login runners. cloud-hypervisor is headless-only
  in this plan.

## Plan

1. Verify the current microvm.nix package selection for
   cloud-hypervisor:

   ```bash
   sed -n '760,775p' /nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/nixos-modules/microvm/options.nix
   ```

   Confirm whether `cloud-hypervisor.package`'s default resolves to
   `cloud-hypervisor-graphics` (graphics variant) or
   `cloud-hypervisor` (plain) based on `graphics.enable`.

2. In [nix/lib/microvm-runner.nix](../../../nix/lib/microvm-runner.nix),
   add an explicit pin when graphics is off and the hypervisor is
   cloud-hypervisor:

   ```nix
   microvm = {
     hypervisor = flavor.hypervisor;
     graphics.enable = flavor.graphics;

     cloud-hypervisor.package =
       lib.mkIf (flavor.hypervisor == "cloud-hypervisor" && !flavor.graphics)
         pkgs.cloud-hypervisor;

     # ... rest unchanged
   };
   ```

   Place it adjacent to the existing `crosvm.extraArgs` block. The
   pin is a defensive measure even if microvm.nix already gets it
   right — explicit beats implicit when the empirical evidence shows
   the package selection has a `graphics.enable`-conditional.

3. In [nix/flake/checks.nix](../../../nix/flake/checks.nix), add a
   new eval-only check:

   ```nix
   module-cloud-hypervisor-runners-eval =
     pkgs.runCommand "module-cloud-hypervisor-runners-eval" {
       ...
     } ''
       # Eval an atlas-like config with runners.enable + the new
       # hypervisor pin. Assert the generated runner script invokes
       # cloud-hypervisor (not cloud-hypervisor-graphics).
       ...
     '';
   ```

   Mirror the structure of `module-runners-eval` at
   [nix/flake/checks.nix:140-170](../../../nix/flake/checks.nix#L140-L170).
   The assertion: `grep -q cloud-hypervisor "$runnerScript" &&
   ! grep -q cloud-hypervisor-graphics "$runnerScript"`.

4. Build the new check locally:

   ```bash
   nix build .#checks.x86_64-linux.module-cloud-hypervisor-runners-eval
   nix flake check --no-build
   ```

   Both must succeed.

5. Sanity-check that a runner *built* from cloud-hypervisor + a
   minimal test VM actually exists:

   ```bash
   nix build .#nixosConfigurations.<some-test-host>.config.microvm.declaredRunner \
     --override-input ...  # if no test host exists, scaffold one
   ```

   The runner script's `exec` line should reference
   `cloud-hypervisor` binary, not `cloud-hypervisor-graphics`.

6. Update [docs/src/configuration/host-config.md](../../configuration/host-config.md)'s
   `runners.hypervisor` row to list `"cloud-hypervisor"` as a
   supported value alongside the existing `"crosvm"`, with a one-
   sentence note that headless VMs should use it. **Do not** mark
   it as the default yet — that's Phase 09.

7. Commit:

   ```
   module: add cloud-hypervisor as supported runtime hypervisor

   Wires the plain-upstream cloud-hypervisor (not the
   spectrum-os -graphics variant) as a supported value for
   services.steampipe-cluster.runners.hypervisor. Default flip
   deferred to the post-validation phase.
   ```

## Acceptance criteria

- [ ] `nix flake check --no-build` is green.
- [ ] `nix build .#checks.x86_64-linux.module-cloud-hypervisor-runners-eval`
      succeeds; its assertion verifies the runner script references
      plain `cloud-hypervisor` (not `-graphics`).
- [ ] Inspecting a built runner script
      (`/nix/store/.../microvm-vm-N-microvm-run`) for a config with
      `runners.hypervisor = "cloud-hypervisor"` shows the
      cloud-hypervisor binary in the `exec` line, not
      cloud-hypervisor-graphics.
- [ ] `services.steampipe-cluster.runners.hypervisor`'s default is
      still `"crosvm"` (no behavioural change for existing hosts).
- [ ] `docs/src/configuration/host-config.md` lists
      `"cloud-hypervisor"` as a supported value for the
      `runners.hypervisor` option.
- [ ] `mdbook build docs/` is clean.

## Files likely touched

- `nix/lib/microvm-runner.nix` — add `cloud-hypervisor.package` pin.
- `nix/flake/checks.nix` — new `module-cloud-hypervisor-runners-eval`
  check.
- `docs/src/configuration/host-config.md` — documentation row update.

## Pitfalls

- **Don't flip the default.** This phase only *adds* support. The
  diff should be a strict expansion of the option's working value
  set; existing `"crosvm"` users see no behavioural change.
- **Don't add cloud-hypervisor to the `loginRunners.hypervisor`
  surface.** Plain cloud-hypervisor has no virgl support. The
  graphical path is QEMU (Phase 08). Adding cloud-hypervisor here
  would mislead operators.
- **Don't delete the `cloud-hypervisor-graphics` overlay.** It
  stays until Phase 10 because Phase 08-09 still rely on the
  graphical-CHV path being build-clean for the deprecation window.
- **`microvm.cloud-hypervisor.package` doesn't accept null.** If
  the `lib.mkIf` syntax above produces an eval error, use a
  conditional outside the attrset:
  ```nix
  microvm = {
    ...
  } // lib.optionalAttrs (flavor.hypervisor == "cloud-hypervisor" && !flavor.graphics) {
    cloud-hypervisor.package = pkgs.cloud-hypervisor;
  };
  ```

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Final Hypervisor Matrix", "Flake Check Changes").
- microvm.nix CHV runner:
  `/nix/store/zizh1qf2gzqr47l2nix0ppgaixbbjpxc-source/lib/runners/cloud-hypervisor.nix`.
- Existing flake check pattern: `nix/flake/checks.nix` near
  `module-runners-eval`.
