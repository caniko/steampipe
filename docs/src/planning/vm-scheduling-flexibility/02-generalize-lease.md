# Phase B — Generalize lease allocation so reservation returns any free slot

> **Recommended Codex model: GPT-5.4 / high effort**
>
> Core Rust refactor with cross-module reach: `src/vm/lease.rs`,
> `src/core/config.rs`'s `ClusterConfig::new` derivation, plus the
> `up`/`down` lifecycle wiring in `src/vm/lifecycle.rs` and
> `src/main.rs`. The change is surgical but touches the typestate
> path the README documents
> ([README.md:836-855](../../../../README.md#L836-L855)), so quiet
> regressions are easy. 5.4 at high gives the reasoning room for the
> integration without the 5.5 premium; mini-tier is risky given the
> typestate complexity.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Depends on Phase A's
`AUDIT.md` landing on trunk. Touches Rust core code in
`src/vm/`, `src/core/`. Independent of Phase C (Nix runner-dir
work) at the file level — C and B can run in parallel after A
lands, but their outputs need to mesh before Phase D's
integration sweep.

## Goal

`cluster-ctl --vm-count N` reserves any `N` free VM slots from
`1..=max_vms`, in lease-pool order (lowest free ID first; the
selection algorithm is documented but the *contract* is "any N
of the free slots — caller must not assume contiguity"). The
returned IDs flow through `ClusterConfig.vms` so that every
downstream operation (`up`, `down`, `status`, etc.) acts on the
actually-leased set, not on a sequential `1..N` generator. Unit
tests prove non-contiguous reservation.

## Why this matters now

Today, `ClusterConfig::new` generates VMs at
[src/core/config.rs:1270-1271](../../../../src/core/config.rs#L1270-L1271):

```rust
.map(|i| VmDef {
    name: VmName(format!("vm-{i}")),
    // ...
})
```

`i` runs over `1..=vm_count`, *before* the lease layer ever
reserves anything. The lease then iterates the pool, skips
held slots, and *renames* nothing — but downstream code uses
the names from the generator, not the leased slots. So if
vm-1 is held, lease returns `[2]`, but `ClusterConfig.vms`
still contains `vm-1` and the boot fails with "Runner not
found: …/vm-1/bin/microvm-run".

This phase moves the VM list derivation to *after* lease
resolution. Phase C handles the Nix-side runner-dir so that
`vm-7`'s microvm-run actually exists. Phase D propagates the
new ordering invariants into the lifecycle commands.

## Out of scope

- Changing the `try_reserve` / `probe_holder` semantics. The
  lease's claim-file format and PID-liveness check stay
  identical.
- Building runners for vm-1..vm-`max_vms`. That's Phase C.
- Updating `down`, `status`, `deploy`, etc. — they already
  consume `ClusterConfig.vms`, but Phase D verifies and fixes
  remaining edge cases.
- Adding an allocation-strategy flag (`--vm-allocation any|seq`)
  unless Phase A's audit decided to add one. If A's decision was
  "no flag, switch the default," respect it.
- Cross-host scheduling. Single host pool.

## Plan

1. **Read AUDIT.md.** Confirm the decisions Phase A recorded
   before writing any code. If the decisions changed since the
   plan was drafted, re-read the affected steps below and adjust.
2. **Annotate `reserve_n`** with a docstring spelling out the
   new contract: "returns up to `count` VM IDs that were free
   at scan time; caller must not assume the returned IDs are
   contiguous or that they start at 1." Add a
   `#[must_use]` if not already present.
3. **Refactor `ClusterConfig::new`** to take a list of
   `vm_ids: &[u8]` instead of (or alongside) `vm_count`. The
   new signature should accept the lease's output verbatim.
   Construct `VmDef`s by mapping each ID to `vm.name`, `vm.ip`,
   `vm.tap_name`, `vm.mac_address`.
4. **Wire the call sites in `src/main.rs`.** Find the `up`
   command's flow: it currently goes `ClusterConfig::new(...)
   → validate_bridge() → up()`. Move the lease call *before*
   `ClusterConfig::new`, then pass the chosen IDs in. Confirm
   the typestate flow at
   [README.md:836-855](../../../../README.md#L836-L855) still
   makes sense after this rewiring; update if the order
   changed.
5. **For commands that don't lease** (`status`, `down`,
   `logs`, `deploy`, `snapshot-*`, `screenshot`,
   `accounts`): they need the *set of already-leased VMs*,
   which is what `list_cluster_vms` at
   [src/vm/lease.rs:117](../../../../src/vm/lease.rs#L117)
   returns. Confirm each of these commands derives its VM list
   from `list_cluster_vms`, not from `1..=vm_count`. Where the
   audit flagged a "consumer expecting contiguous" call site,
   fix it here. Anything more invasive defer to Phase D.
6. **Unit tests in `src/vm/lease.rs`.** Add or extend:
   - `reserve_n_returns_non_contiguous_when_vm1_held` — hold
     vm-1 with an alien cluster, ask for count=1 with
     max_vms=7, assert returned ID == 2.
   - `reserve_n_skips_multiple_held_slots` — hold vm-1, vm-3,
     vm-5, ask for count=3 with max_vms=7, assert returned IDs
     == [2, 4, 6].
   - `reserve_n_errors_when_pool_insufficient` — hold 6 of 7,
     ask for count=2, expect error.
7. **Integration smoke** via the fixture's diagnostic. With
   Phase C *not yet landed*, deliberately put a placeholder
   claim on vm-1 (using the existing trick in
   [tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh)),
   then run `cluster-ctl --project-root tests/fixture
   --vm-count 1 up --runners-dir <tournament-runner-dir>`. With
   Phase B alone, the reserve picks vm-2 and tries to boot from
   the tournament runner-dir which *does* have vm-2's runner —
   so this should succeed end-to-end if the
   `cluster-tournament-uifull-up` script is used as the source
   of the runner-dir. Document this in the PR as the
   integration test until phase C ships.
8. **Type check + clippy.**
   `cargo check --workspace --all-targets`,
   `cargo clippy --all-targets -- -D warnings`.
9. **Update inline doc comments** anywhere that referenced
   "vm-1 through vm-N" or assumed contiguity. Search for the
   phrases "1 through", "vm-1 through", "first N VMs" in
   `src/`. Replace with descriptions that match the new
   semantics.

## Acceptance criteria

- [ ] `cargo test -p steampipe vm::lease` passes, including the
      three new tests in step 6 (non-contiguous, skip-multiple,
      pool-insufficient).
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] `cargo check --workspace --all-targets` clean.
- [ ] Manual smoke: with a placeholder claim on vm-1, running
      `cluster-ctl --project-root tests/fixture --vm-count 1
      --cluster fixture-test up --runners-dir <tournament-dir>`
      boots vm-2 (or whichever slot lease returned), and
      `cluster-ctl … status` shows that VM as `UP / OK`.
- [ ] The placeholder-claim workaround in
      [tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh)
      is *still in place* — phase E removes it after C/D land.
- [ ] `cargo test --workspace` overall is green (no test files
      regressed by name-collision or hardcoded assumptions).
- [ ] Inline doc comments referencing "vm-1..vm-N" or "first N
      VMs" have been updated (verified by grep showing zero
      remaining occurrences in `src/`).

## Files likely touched

- [src/vm/lease.rs](../../../../src/vm/lease.rs) — docstring +
  3 new tests.
- [src/core/config.rs](../../../../src/core/config.rs) —
  `ClusterConfig::new` signature, VM derivation from leased
  IDs.
- [src/main.rs](../../../../src/main.rs) — `up` command wiring;
  call lease before ClusterConfig construction.
- [src/vm/lifecycle.rs](../../../../src/vm/lifecycle.rs) — may
  need to honor the new vm-id contract.
- Any consumer the audit flagged (the audit table is the
  authoritative file list).

## Pitfalls

- **Re-leasing on every command run.** Every cluster-ctl
  invocation currently constructs `ClusterConfig` from
  `--vm-count`. After this phase, `up` must *write* claims,
  but `status` / `down` must *read* claims and not re-lease.
  Subtle bug: if `down` calls `reserve_n` again, it might
  succeed at reserving fresh slots and then "down" the wrong
  set. Recovery: separate the "lease new slots" path from the
  "find existing slots" path explicitly in main.rs's
  dispatch.
- **Existing snapshots reference vm-N paths.**
  `cluster-ctl snapshot-restore after-login` expects to find
  vm-1, vm-2, ... If a snapshot was saved from a sequential
  run and is restored under flexible scheduling, the IDs may
  not match. Behaviour decision: snapshots restore to their
  recorded IDs, not to currently-leased IDs. Document this
  in the snapshot README.
- **The README typestate diagram lies.**
  [README.md:836-855](../../../../README.md#L836-L855) shows
  `ClusterConfig<Unchecked>` constructed before
  `validate_bridge`. If you move lease ahead of
  ClusterConfig construction, that diagram needs an update.
  Easy to forget.
- **`--vm-count` semantics drift.** If a user runs
  `cluster-ctl --vm-count 7 up` against a pool where vm-3 is
  held by another cluster, they now boot vm-1, vm-2, vm-4,
  vm-5, vm-6, vm-7, plus one more (vm-8 if max_vms ≥ 8) or
  error if not. That's the new contract; ensure the error
  message at
  [src/vm/lease.rs:81-89](../../../../src/vm/lease.rs#L81-L89)
  is clear about it.
- **Tests that assert exact VM names.** Even outside
  `lease.rs`, tests may grep for "vm-1" in output. Hunt
  these down before they fail in CI; the audit table should
  enumerate them.

## Reference

- Originating decisions:
  `docs/src/planning/vm-scheduling-flexibility/AUDIT.md`
  (produced by Phase A).
- Lease module:
  [src/vm/lease.rs](../../../../src/vm/lease.rs).
- ClusterConfig generator:
  [src/core/config.rs:1270-1271](../../../../src/core/config.rs#L1270-L1271).
- Typestate flow in the README:
  [README.md:836-855](../../../../README.md#L836-L855).
- Related phases: A (audit — prerequisite), C (Nix runner-dir
  to make non-vm-1 boots actually work — parallel), D
  (lifecycle integration sweep — sequential), E (cleanup).
