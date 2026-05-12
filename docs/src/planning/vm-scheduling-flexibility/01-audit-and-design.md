# Phase A — Audit "vm-1..vm-N" sequential assumptions and design the flexible allocation contract

> **Recommended Codex model: GPT-5.5 / medium effort**
>
> Design-heavy top-of-plan work spanning Rust (`src/vm/lease.rs`,
> `src/core/config.rs`, every consumer of `ClusterConfig.vms`) and Nix
> (`nix/lib/test-cluster.nix`, fixture flake, downstream regicide
> flake). Wrong decisions here ripple through phases B/C/D/E. 5.5 at
> medium gives the design discretion and cross-surface coherence
> without paying for 5.5/high — the actual *amount* of code reviewed
> here is small; the bar is judgement.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Read-mostly phase — output is a
design note + audit table committed under
`docs/src/planning/vm-scheduling-flexibility/AUDIT.md`. No production
code changes. Independent of every other phase in this plan; B, C, D,
E all depend on its output.

## Goal

A committed `AUDIT.md` that contains:

1. **A table of every call site that assumes "VM names are vm-1, vm-2,
   ..., vm-N in order"** — file, line, the assumption made, and the
   blast radius if the assumption is violated.
2. **A decision record** answering these questions:
   - What does `cluster-ctl --vm-count N` mean under flexible
     allocation? "Reserve N VMs from the pool, picking any N of the
     `max_vms` slots that are free." Confirm or amend.
   - Does the new lease scan from `1..=max_vms` and take the first N
     free (skip-and-take), or randomise, or honour a per-cluster
     preference list?
   - How are the chosen VM IDs surfaced back to the operator?
     Reserved-list line in `up` output is already present —
     formalise the format so scripts can parse it.
   - Is there a `--vm-allocation sequential|any` flag for
     backwards-compat, or do we just switch the default?
   - Does the runner-dir from `mkTestCluster` build runners for
     vm-1..vm-N (current) or vm-1..vm-`max_vms` (proposed)? What's
     the disk-cost delta?
   - Down semantics: `cluster-ctl down` operates on which VMs —
     "everything my cluster currently claims" (already the case) or
     "vm-1..vm-N" (legacy)?
3. **An interface sketch** for `reserve_n` and the
   `ClusterConfig.vms` derivation path that B will implement.

## Why this matters now

The pain came from a concrete incident on 2026-05-11: the in-tree
fixture diagnostic could not boot when vm-1 was held by an unrelated
running microvm (cluster `1v1`, pid 1618562). The diagnostic resorted
to writing placeholder claim files for vm-1..vm-6 to force the lease
onto vm-7 — a workaround that lives in
[tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh)
and is fragile. The fix the user asked for: "Just pick any vm that
is available. That should be the target."

The reservation logic is already nearly there
([src/vm/lease.rs:62-79](../../../../src/vm/lease.rs#L62-L79)):

```rust
for id in 1..=max_vms {
    if try_reserve(id, lock_dir, cluster)? {
        ids.push(id);
    } else {
        // ... "held by other, skipping"
    }
}
```

It already skips held slots. The blocker is not the scheduler — it's
that downstream Nix and Rust assume the runner-dir contains vm-1..vm-N
contiguously. So `--vm-count 1` succeeds at reserving vm-7, then dies
with "Runner not found: …/vm-7/bin/microvm-run" because only vm-1
was built. Phase A surfaces this and the other assumptions in one
place; phase B onwards fixes them.

## Out of scope

- Writing any production code. This is pure audit + design.
- Changing TAP / bridge configuration. canix already declares
  `tap-vm-1..tap-vm-8` owned by uid 1000 — the new scheduler operates
  inside that pool.
- Cross-host scheduling (running fixture VMs on different physical
  hosts). Single-host only.
- Persistent slot pinning (e.g. "always run integration tests on
  vm-7"). Reservation is ephemeral; if a user needs a specific slot,
  the fixture diagnostic's placeholder-claim trick is fine.
- Per-VM resource overrides (`[[vm]] index = N ram_mb = 4096`). Those
  already exist; we just need to confirm they keep working when
  allocation is non-sequential.

## Plan

1. **Build the assumption inventory.** From the repo root, run a
   broad grep for the patterns:
   ```bash
   rg -nP 'vm-1(?!\d)' --type rust --type nix
   rg -nP 'vm-\{i\}|vm-\{n\}|format!\("vm-' --type rust
   rg -n '1\.\.=' src/vm src/core src/net src/harness
   rg -n 'vmCount|count = ' nix/lib/test-cluster.nix
   ```
   Read every hit. Categorise into:
   - **Generator** — produces VM names sequentially (e.g. the
     `ClusterConfig::new` site at
     [src/core/config.rs:1270-1271](../../../../src/core/config.rs#L1270-L1271)
     `name: VmName(format!("vm-{i}"))`).
   - **Consumer expecting contiguous** — assumes vm.name parses to
     an index 1..N (e.g. anywhere that re-derives an IP from the
     index instead of reading `vm.ip`).
   - **Display-only** — just prints names; tolerates any.
2. **Inventory Nix-side runner-dir construction.**
   `nix/lib/test-cluster.nix` `mkRunnersDir` takes `count` and
   `takeFirstVMs count`
   ([nix/lib/test-cluster.nix:283-296](../../../../nix/lib/test-cluster.nix#L283-L296)).
   Document:
   - Does `vmCount` (default 7 at line 96) bound runner-dir size?
   - What happens if the runner-dir has 7 runners but cluster-ctl
     leases vm-3 only — does it work? Answer is "yes, microvm-run
     for vm-3 is in the dir."
   - Disk cost: how much does one runner derivation take? (Check via
     `du -sh result-default/vm-1`.)
3. **Decide the new semantics.** Write the decision record. Default
   recommendation, to be confirmed in audit:
   - `--vm-count N` means "reserve N from the pool, any IDs". No
     contiguity guarantee. No flag — switch the default. Old
     behaviour is recoverable by passing `--vm-ids 1,2,3` (a
     follow-up phase if anyone actually needs it).
   - Runner-dir is built for vm-1..vm-`max_vms` (full slot range).
     Disk cost is approximately linear; if `max_vms=8` × per-runner
     closure is unacceptable, fall back to building per-need at boot
     (more complex; do not pick this without numbers).
   - Down operates on claim-files-owned-by-this-cluster (already the
     case in [src/vm/lease.rs](../../../../src/vm/lease.rs)
     `list_cluster_vms`). Verify by reading the down path.
4. **Sketch the new `reserve_n` signature.** Probably unchanged:
   the function already does the right thing. What changes is
   the *guarantee documented in the docstring* — "returns up to
   `count` VM IDs from `1..=max_vms` that were free". Phase B
   asserts non-contiguity in tests.
5. **Sketch how `ClusterConfig.vms` gets its IDs.** Currently it
   builds `vm-1..vm-vm_count` upfront. Proposed: build the
   `VmDef` list *after* `reserve_n`, from the returned IDs. This
   means the typestate flow becomes: parse config → call lease →
   construct ClusterConfig with chosen IDs. Verify whether
   the existing `ClusterConfig<Unchecked> → BridgeReady` flow
   ([README.md:836-855](../../../../README.md#L836-L855)) needs
   adjustment.
6. **Cost the disk impact of the runner-dir change.**
   `du -sh result-default/vm-{1,2,3,4,5,6,7}` against a freshly
   built tournament runner-dir tells you the cost. Record in
   AUDIT.md. If it exceeds ~5 GB total, escalate before phase C
   commits to building all slots.
7. **Write `AUDIT.md`** committing: the assumption table, the
   decision record, the disk-cost number, the interface sketches.
8. **Sanity-check against the fixture diagnostic.** Confirm the
   placeholder-claim trick in
   [tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh)
   can be deleted in phase E once flexible allocation lands.

## Acceptance criteria

- [ ] `docs/src/planning/vm-scheduling-flexibility/AUDIT.md` exists.
- [ ] The assumption table has at minimum 10 rows (the file/line/
      assumption/blast-radius columns are filled — no `TBD` cells).
- [ ] The decision record answers all 5 questions enumerated in
      Goal (2). Each answer has a one-sentence rationale.
- [ ] The disk-cost number for building vm-1..vm-7 runners is
      recorded with the `du` invocation that produced it.
- [ ] Interface sketches for `reserve_n` and `ClusterConfig.vms`
      derivation exist and compile in pseudocode (no live Rust —
      just enough for phase B to implement against).
- [ ] No production code (`.rs`, `.nix` outside docs/) is
      modified by this phase.

## Files likely touched

- `docs/src/planning/vm-scheduling-flexibility/AUDIT.md` (new).
- (Optional) `docs/src/SUMMARY.md` to surface AUDIT.md alongside
  this plan's index. Skip if the SUMMARY edit feels premature.

## Pitfalls

- **The grep misses `format!("tap-{name}", name = vm.name)` etc.**
  Some assumptions are spelled as templating that doesn't match a
  literal "vm-1". Read the Tap/IP derivation paths
  ([src/net/bridge.rs](../../../../src/net/bridge.rs),
  [src/net/netem.rs](../../../../src/net/netem.rs)) explicitly to
  confirm they consume `vm.name` rather than reconstructing it.
- **Skipping the runner-dir disk-cost number.** Without it, phase
  C will be tempted to "just always build everything" and may
  push the closure past 10 GB on tournament-uifull. Always
  measure first.
- **Assuming Down works without inspection.** Read the down path
  end-to-end and prove it operates on `list_cluster_vms` output,
  not on `1..=vm_count`. If it doesn't, that's a Phase D bullet
  to add explicitly.
- **Bikeshedding `--vm-allocation`.** If audit reveals zero call
  sites rely on contiguity, do not add the flag. Defaults are a
  cleaner contract than option soup. The decision record should
  call this out, not punt to "add a flag just in case."

## Reference

- Originating incident: 2026-05-11 diagnostic run where vm-1 was
  held by pid 1618562, and the workaround required pre-writing
  placeholder claim files. See
  [tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh).
- Lease scanner (already 90 % of the fix):
  [src/vm/lease.rs:51-90](../../../../src/vm/lease.rs#L51-L90).
- Sequential VM name generator:
  [src/core/config.rs:1270-1271](../../../../src/core/config.rs#L1270-L1271).
- Runner-dir builder:
  [nix/lib/test-cluster.nix:283-296](../../../../nix/lib/test-cluster.nix#L283-L296).
- Related phases: B (implements per audit decisions), C (Nix
  changes per audit decisions), D (integration sweep), E (tests +
  docs + remove the placeholder-claim workaround).
