# Phase C — Build runner-dirs that span the full slot range, not just vm-count

> **Recommended Codex model: GPT-5.4 / medium effort**
>
> Moderate-complexity Nix refactor in
> [nix/lib/test-cluster.nix](../../../../nix/lib/test-cluster.nix) and
> the fixture flake. The work is straightforward — change
> `takeFirstVMs count` to "take all VMs up to max_vms" and verify
> wrappers still resolve their runners — but Nix evaluation is
> unforgiving of subtle shape changes, and the disk-cost decision
> (build all vs build on-demand) needs the audit numbers. 5.4 at
> medium fits.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Depends on Phase A's audit
(specifically the disk-cost decision). Can run in parallel with
Phase B at the file level — no overlap with Rust code. Both must
land before Phase D's integration sweep makes flexible allocation
the default.

## Goal

Every `mkRunnersDir` invocation produces a linkFarm containing
runners for *every* VM in the host pool (vm-1..vm-`max_vms`),
not just vm-1..vm-`count`. Wrappers that today refer to
`--runners-dir <flavorRunnersDir>` continue to work because the
larger dir is a superset. After this phase, `cluster-ctl … up`
with a lease that picks vm-5 can find `<runner-dir>/vm-5/bin/
microvm-run` regardless of whether `--vm-count` was 1, 5, or 7.

## Why this matters now

`mkRunnersDir` at
[nix/lib/test-cluster.nix:283-296](../../../../nix/lib/test-cluster.nix#L283-L296)
takes a `count` arg and uses `takeFirstVMs count` to pick the
first `count` VMs from `linuxVMs`. The `mkAllFlavorScripts` call
sites at line 447 (`"1v1" 1 2` — count=1) and line 454
(`"tournament" vmCount 8` — count=7) determine which runners get
built per cluster mode.

This is the second half of the "always vm-1" footgun. Phase B
makes the lease pick vm-7 cleanly; Phase C ensures the runner
binary for vm-7 actually exists in the runner-dir the wrapper
passes to cluster-ctl. Without C, B's success boots vm-7 in
the lease state but then fails with "Runner not found:
…/cluster-vm-runners-uifull/vm-7/bin/microvm-run" — exactly
the error captured on 2026-05-11 during the diagnostic ladder
run.

## Out of scope

- Lazy/on-demand runner building. If the audit's disk-cost
  number is acceptable (≤ ~5 GB total per runner-dir per
  flavor), build all slots eagerly. Lazy building is a
  scope-creep trap; defer unless audit numbers force it.
- Per-VM RAM/CPU overrides via `[[vm]] index = N` — those still
  work after this phase because the change is "build more
  runners," not "change VM definitions."
- Decoupling runner-dir from flavor (e.g., one runner-dir
  shared across default + uifull). Out of scope.
- Adding new cluster modes (e.g., a flexible "any-of-N" mode).
  Existing modes (1v1, tournament) suffice.

## Plan

1. **Read AUDIT.md.** Confirm the disk-cost number and the
   "build all slots" decision.
2. **Refactor `mkRunnersDir`** at
   [nix/lib/test-cluster.nix:283-296](../../../../nix/lib/test-cluster.nix#L283-L296)
   to ignore the `count` argument and always include every VM
   in `linuxVMs` (which itself is bounded by `vmCount = 7`).
   Or — cleaner — rename the parameter to something less
   misleading and have it cap at `vmCount` always.
3. **Audit `mkAllFlavorScripts`** call sites to confirm they
   pass the right `count` for the *cluster-ctl wrapper's*
   `--vm-count` arg (still 1 for 1v1, 7 for tournament) but
   no longer for the runner-dir. The `cluster-ctl … up` flag
   passing in the generated wrappers stays as-is; only the
   runner-dir builder changes.
4. **Verify wrapper outputs.** `nix flake show
   ./tests/fixture` should still list the same apps. The
   `cluster-1v1-up` wrapper now points at a runner-dir that
   has vm-1..vm-7, but its `--vm-count 1` argument still means
   "lease one slot."
5. **Build cost measurement.**
   ```bash
   rm result-default result-uifull 2>/dev/null
   nix build .#packages.x86_64-linux.cluster-vm-runners-default -o result-default
   nix build .#packages.x86_64-linux.cluster-vm-runners-uifull  -o result-uifull
   du -sh result-default result-uifull
   du -sh result-default/vm-1 result-uifull/vm-1
   ```
   Capture the numbers in a PR-description note. If total
   per-flavor exceeds 6 GB, stop and escalate — audit assumed
   smaller; revisit.
6. **Smoke against the fixture.** Once Phase B is in flight
   (or use a placeholder claim on vm-1 as a stand-in):
   ```bash
   nix run ./tests/fixture#cluster-1v1-up
   # Confirm:
   ls -la ~/.local/state/steampipe/fixture-uifull-solo/result*/vm-7/bin/
   #   should contain microvm-run
   ```
7. **Update wrapper comments** in test-cluster.nix that explain
   "this builds vm-1..vm-N" to reflect the new "vm-1..vm-`vmCount`
   for any cluster mode" reality.
8. **Run `nix flake check ./tests/fixture`** and the top-level
   `nix flake check .` to surface any evaluation regressions.

## Acceptance criteria

- [ ] `nix build /data/nvme0/can/Projects/steampipe/tests/fixture#cluster-1v1-up.program
      --no-link` succeeds.
- [ ] After build, the
      `result/lib/.../cluster-vm-runners-default/` or
      equivalent directory contains `vm-N/bin/microvm-run` for
      every N in `1..=vmCount` (default 7). Verify with `ls`.
- [ ] Disk cost measured and recorded in the PR description.
      Per-flavor total under 6 GB; per-VM closure stable across
      flavors. Numbers in the PR body.
- [ ] `nix flake check ./tests/fixture` exits 0.
- [ ] `nix flake check .` exits 0.
- [ ] The wrappers' `--vm-count` argument is unchanged from
      before (1 for 1v1, 7 for tournament). Verified by
      reading the generated scripts via
      `nix eval .#packages.x86_64-linux.cluster-1v1-up
      --raw` and grepping for `--vm-count`.

## Files likely touched

- [nix/lib/test-cluster.nix](../../../../nix/lib/test-cluster.nix) —
  `mkRunnersDir`, `takeFirstVMs`, possibly `mkAllFlavorScripts`.
- [tests/fixture/flake.nix](../../../../tests/fixture/flake.nix) —
  no logic changes expected, but verify `nix flake show` still
  matches.
- (No `.rs` changes in this phase.)

## Pitfalls

- **Cache invalidation cascade.** Changing `mkRunnersDir`'s
  output for unchanged inputs invalidates downstream
  cache hits for every consumer (regicide, atlas's
  `services.steampipe-cluster`, the fixture). Expect a
  one-time rebuild storm. Communicate this in the PR.
- **`takeFirstVMs` is used elsewhere.** Grep for it before
  refactoring; if anything outside `mkRunnersDir` depends on
  the "first N" semantics, decide whether to keep both
  helpers or rename.
- **Disk cost surprise from store-disk derivation.**
  Each `vm-N` is a microvm runner that drags in a NixOS
  closure (~1.5 GB per VM in default flavor). Seven of those
  is ~10 GB. If the audit underestimated, you'll discover it
  here. Mitigation: confirm `nix-store --realise
  --no-output` against one runner first, multiply by 7,
  decide before committing to eager build.
- **Symlink-farm vs copy.** `linkFarm` writes
  symlinks to store paths — no extra disk over the closure
  itself. Confirm `du -L` is what's being measured if you
  follow links, vs the (smaller) symlink-only size.
- **Wrapper extracts `--runners-dir` by regex from the script
  text.** The fixture's
  [flake.nix:48-54](../../../../tests/fixture/flake.nix#L48-L54)
  parses `--runners-dir <path>` out of the generated script
  with grep+awk. If the new runner-dir path has spaces or odd
  characters, the parse breaks. Verify the extracted path is
  a single nix store path on the live wrapper output.

## Reference

- Originating context: 2026-05-11 diagnostic run failed with
  `Runner not found: …/cluster-vm-runners-uifull/vm-7/bin/microvm-run`.
- Runner-dir builder:
  [nix/lib/test-cluster.nix:283-296](../../../../nix/lib/test-cluster.nix#L283-L296).
- Wrapper that consumes the runner-dir (fixture):
  [tests/fixture/flake.nix:48-78](../../../../tests/fixture/flake.nix#L48-L78).
- Related phases: A (audit — disk-cost decision feeds this
  phase), B (Rust lease — parallel), D (integration sweep
  consumes both), E (tests + docs + fixture cleanup).
