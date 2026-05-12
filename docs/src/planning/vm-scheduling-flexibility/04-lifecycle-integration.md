# Phase D — Sweep lifecycle commands so every operation honours the leased VM set

> **Recommended Codex model: GPT-5.4 / high effort**
>
> Integration sweep across `down`, `status`, `deploy`, `run`,
> `stop-game`, `logs`, `screenshot`, `accounts`, `snapshot-*`,
> `netem*`, `watch`, `steam-*`. Each command has its own way of
> deriving "which VMs to operate on," and any one of them slipping
> back to "vm-1..vm-N" silently produces wrong-target operations.
> Risk is moderate (silent miscoordination, not data loss) but the
> surface is wide. 5.4 at high gives the reasoning room for the
> per-command verification; 5.5/medium would be defensible if a
> typestate refactor turns out to be necessary.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Depends on **both** Phase B
(lease returns chosen IDs, ClusterConfig honours them) and Phase C
(runner-dir has every slot's binary). Land both before starting
this phase; otherwise integration tests can't tell whether a
failure is in the command path or in the prerequisite.

## Goal

Every `cluster-ctl` subcommand operates on the actually-leased VM
set, not on a sequential `1..N` generator. Concretely:

- `cluster-ctl status` shows rows for the VMs *currently claimed
  by this cluster*. Slots held by other clusters appear as
  "(unowned)" or are omitted entirely — pick one and document it.
- `cluster-ctl down` stops only the VMs whose claim files this
  cluster owns. Already mostly true; verify and tighten.
- `cluster-ctl deploy / run / stop-game / logs / screenshot /
  accounts / steam-* / snapshot-*` all source their VM list from
  `list_cluster_vms(cluster, max_vms, lock_dir)`, not from
  `ClusterConfig::vms_sequential`.
- `cluster-ctl netem <vm-N>` accepts any vm-N currently owned by
  this cluster — explicit per-vm targets that name a non-owned
  slot fail loudly with a clear message.

A new acceptance smoke ("non-contiguous lease, every command
works") demonstrates the contract end-to-end.

## Why this matters now

Phase A's audit enumerated the consumers. Phase B fixed the most
critical one (`ClusterConfig::new` after lease) but did not touch
each command's individual derivation path. Many commands today
read `ClusterConfig.vms`, which is now non-contiguous — those work
for free. A handful re-derive a VM list locally (likely the
TUI watch, the netem helper, or the snapshot-restore path). Those
must be hunted down.

The downside of skipping this phase is subtle: `cluster-ctl
status` would still show vm-1, vm-2, …, vm-N with "—" for slots
that were never leased, confusing operators. Or `snapshot-save`
might persist state for vm-1 (sequential) when the actual leased
VM is vm-7 — losing the snapshot of the running VM and saving an
empty placeholder.

## Out of scope

- Adding new commands. Sweep existing ones only.
- Cross-cluster operations ("snapshot all clusters at once").
  Single-cluster scope per invocation.
- Per-command CLI flag changes beyond what's needed to honor the
  contract. Don't add `--vm-id` flags unless the audit found a
  command that *needs* one.
- TUI redesign in `watch`. Just make sure the rows it shows are
  the leased set.

## Plan

1. **Open Phase A's AUDIT.md** and read every entry tagged
   "Consumer expecting contiguous." That's the punch list for
   this phase.
2. **For each affected command:**
   - Find the `Commands::<Name> { … } => …` branch in
     [src/main.rs](../../../../src/main.rs).
   - Trace how it builds its VM list. If it uses
     `config.vms` (post-B) and `config.vms` was constructed
     from the lease, the command is fine — verify with a
     unit test or by reading the call.
   - If it constructs its own list (e.g. iterates `1..=N`),
     refactor to source from `config.vms` or
     `list_cluster_vms`.
3. **The full command list to sweep** (cross-check against
   [README.md:908-969](../../../../README.md#L908-L969)):
   `status`, `up`, `down`, `restart`, `deploy`, `run`,
   `stop-game`, `steam-login`, `steam-check`, `steam-start`,
   `steam-guard`, `accounts`, `clean-logins`, `test`,
   `bisect`, `history`, `logs`, `watch`, `screenshot`,
   `netem`, `netem-show`, `netem-reset`, `snapshot-save`,
   `snapshot-restore`, `snapshot-list`, `snapshot-delete`,
   `init`, `yh-config`. Mark each ✓/✗ in a checklist in the
   PR description.
4. **Status semantics decision.** When a cluster is at
   `--vm-count 1` and lease picked vm-7, should `status`
   display:
   - (a) One row for vm-7, ignoring slots not held by this
     cluster, *or*
   - (b) Eight rows — vm-7 with cluster-owned state, others
     blank or marked as `owned by <cluster-name>`?
   Pick one. Recommendation: (a) by default;
   `cluster-ctl status --all` shows the full pool with
   ownership annotation. Add `--all` as a follow-up only if
   asked.
5. **Snapshot path.** Open
   [src/vm/snapshot.rs](../../../../src/vm/snapshot.rs)
   and confirm save/restore key snapshots by VM name, not by
   sequential index. Add a test case where the leased VM is
   vm-3 (non-1, non-7) and save/restore round-trips.
6. **`netem` per-vm target.** Read
   [src/net/netem.rs](../../../../src/net/netem.rs):
   `netem vm-N --latency 80` should error if vm-N is not in
   this cluster's lease (`list_cluster_vms`), with a
   message like "vm-3 is not owned by cluster 'fixture'
   (owned by 'regicide')".
7. **TUI `watch` panel.** Read
   [src/ui/watch.rs](../../../../src/ui/watch.rs). It
   refreshes status. Ensure its row source is the leased
   set.
8. **Integration smoke.** Write a shell script (or extend the
   diagnostic ladder) that:
   - Holds claims on vm-1, vm-3, vm-5 (alien cluster).
   - Runs `cluster-ctl --vm-count 2 up` → expect to lease
     vm-2 and vm-4.
   - Runs each command from the punch list against the
     cluster.
   - Asserts every command's output mentions only vm-2 and
     vm-4, never vm-1/3/5/6/7.
   Commit the script under
   `tests/fixture/diagnose/integration/non-contiguous-lease.sh`.
9. **Validate cargo tests + clippy.**
10. **Run the integration smoke** end-to-end. Document the
    raw output in the PR.

## Acceptance criteria

- [ ] Every command in the Plan step 3 list has a ✓ next to
      it in the PR description, with the file:line where its
      VM list is sourced.
- [ ] `cargo test --workspace` green, including a new
      `snapshot_round_trip_with_non_sequential_vm` test in
      `vm::snapshot`.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] The non-contiguous-lease integration smoke from step 8
      runs to completion and asserts the expected VM set on
      every command's output.
- [ ] `cluster-ctl status` rendered against a cluster
      holding vm-2 + vm-4 shows two rows, both labeled
      correctly.
- [ ] `cluster-ctl netem vm-3 --latency 80` against the same
      cluster fails with "not owned" rather than silently
      doing nothing.
- [ ] No regression on the legacy sequential path: a cluster
      with empty pool and `--vm-count 3` leases vm-1, vm-2,
      vm-3 and all commands work as before.

## Files likely touched

- [src/main.rs](../../../../src/main.rs) — possibly per-
  command branches.
- [src/core/config.rs](../../../../src/core/config.rs) —
  `ClusterConfig` helpers if the audit found shared VM-list
  derivation logic worth factoring.
- [src/vm/snapshot.rs](../../../../src/vm/snapshot.rs).
- [src/vm/status.rs](../../../../src/vm/status.rs).
- [src/net/netem.rs](../../../../src/net/netem.rs).
- [src/ui/watch.rs](../../../../src/ui/watch.rs).
- [src/harness/runner.rs](../../../../src/harness/runner.rs)
  (test command's deploy + run loop).
- `tests/fixture/diagnose/integration/non-contiguous-lease.sh`
  (new).

## Pitfalls

- **Snapshot directory layout assumes sequential names.**
  If `snapshot-save after-login` writes
  `$XDG_STATE_HOME/steampipe/<cluster-name>/snapshots/after-login/vm-1/…`
  but the actual leased VM was vm-7, snapshots key off the
  real name (good) or off the slot index (bad). Read first.
- **The `test` command's player → VM mapping.**
  `cluster-ctl test --players 4` maps players to VMs.
  Today probably "host + vm-1 + vm-2 + vm-3." Under flexible
  scheduling this becomes "host + lease[0] + lease[1] +
  lease[2]." Verify the mapping is deterministic enough that
  test history remains comparable (it should, because
  results are keyed by run-N timestamp, not by VM ID).
- **`bisect` reuses the same lease across iterations.**
  Verify the bisect harness reserves once and re-uses, not
  re-leases per iteration (which could shift IDs and confuse
  log collection).
- **`history` displays accumulate VM IDs.** Future history
  views must not group runs by "vm-1" if successive runs
  leased different slots. Group by cluster and run timestamp;
  the per-VM detail is in the artefacts.
- **netem's TAP-name derivation.** `format!("tap-{}",
  vm.name)` — fine. But ensure the cmdline allowlist for
  netem doesn't reject slot-7 by virtue of a substring match
  against "tap-vm-7" with no vm-7 ever expected.

## Reference

- AUDIT.md from Phase A.
- Per-command code map:
  [README.md:908-969](../../../../README.md#L908-L969).
- Lease pool functions:
  [src/vm/lease.rs:117](../../../../src/vm/lease.rs#L117)
  (`list_cluster_vms`).
- Related phases: A (audit — prerequisite), B (lease + config
  — prerequisite), C (runner-dir — prerequisite), E (tests +
  docs + cleanup — consumer).
