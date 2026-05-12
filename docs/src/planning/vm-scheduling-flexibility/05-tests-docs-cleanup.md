# Phase E — Tests, docs, and removal of the fixture's placeholder-claim workaround

> **Recommended Codex model: GPT-5.4 / medium effort**
>
> Cleanup phase: extend test coverage for the new contract, update
> the README + planning docs to reflect "any free VM" scheduling,
> and remove the placeholder-claim trick from the in-tree fixture
> diagnostic now that flexible allocation is the default. Moderate
> complexity (test surface is broad; doc edits are mechanical);
> 5.4 at medium is the matrix-aligned routing.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Depends on **Phases B, C,
D** all landed. Last phase in this plan; no consumers downstream
within this set, but the
[../vnc-visual-testing/](../vnc-visual-testing/README.md) plan
becomes much easier to execute once this lands because the
fixture diagnostic no longer needs to manage placeholder claims.

## Goal

A user reading the project docs understands that:

- `--vm-count N` reserves N free slots from the pool (any IDs).
- The reserved set is reported in the `up` command's output.
- All other commands operate on the leased set, not on a
  sequential index range.

The README's "Architecture overview" section reflects the new
typestate flow. The fixture diagnostic ladder no longer writes
placeholder claim files. `cargo test --workspace` and `nix flake
check` are green across the board.

## Why this matters now

After Phases B + C + D land, the implementation is correct but
the documentation and test suite still describe the old world.
The placeholder-claim trick in
[tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh)
is now dead weight that confuses future contributors ("why does
the diagnostic write fake leases?"). Cleaning up *now*, while
the recent context is fresh, prevents technical debt from
solidifying.

## Out of scope

- Writing new features beyond what Phases A–D produced.
- Re-architecting the documentation. Just update the sections
  affected by the scheduling change.
- New `cluster-ctl` subcommands (e.g. an explicit `lease list`).
  Out of scope; can be a follow-up if the audit recommends it.
- Per-host scheduling docs (cross-host coordination is out of
  scope for the whole plan).

## Plan

1. **Update [README.md](../../../../README.md):**
   - "VM lifecycle" section
     ([README.md:328-376](../../../../README.md#L328-L376)) —
     describe the lease contract: "Reserves N slots from the
     pool, skipping any held by other clusters. The reserved
     IDs are printed in the `up` output and can also be seen
     in `cluster-ctl status`."
   - "Architecture overview" → "Typestate pattern"
     ([README.md:836-855](../../../../README.md#L836-L855)) —
     fix the diagram if Phase B moved lease ahead of
     `ClusterConfig::new`.
   - "Architecture overview" → "Parallel operations" — no
     change expected; verify.
   - "Architecture overview" → "State management"
     ([README.md:873-892](../../../../README.md#L873-L892))
     — note that snapshots key off VM names, which under
     flexible allocation may not be vm-1..vm-N.
2. **Update the planning docs** in this plan set if any phase
   doc references "land in this order" but the actual order
   you took differed. Drift should be minimal but worth one
   pass.
3. **Update [docs/src/SUMMARY.md](../../SUMMARY.md)** —
   confirm the planning section already lists this plan.
4. **Remove the placeholder-claim trick from the fixture
   diagnostic.** In
   [tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh),
   delete the `PLACEHOLDER_CLAIMS` block and the
   `cleanup_placeholder_claims` trap. The script becomes a
   straight invoke-and-classify flow. The
   [tests/fixture/steampipe.toml](../../../../tests/fixture/steampipe.toml)
   `max_vms = 8` setting stays — it gives the lease pool
   enough headroom to find a free slot even when several are
   held by other clusters.
5. **Update the diagnostic's `01-boot.sh`** to read which VM
   was leased from the `cluster-ctl up` stdout (the
   "Reserved: vm-N" line) and propagate the VM IP downstream
   via `RUN_DIR/leased_vm.id`. Steps 02..06 read that file
   to populate `VM_IP` (`10.0.100.${N}`) instead of
   hardcoding 10.0.100.7.
6. **Tests for the new contract:**
   - `src/vm/lease.rs` — Phase B already added skip-and-take
     tests. Confirm they exercise count > 1, count == max,
     pool-empty, pool-partial cases. Add any gaps.
   - `tests/fixture/diagnose/integration/non-contiguous-lease.sh`
     from Phase D — confirm it still passes after this phase
     removes the placeholder workaround.
   - New: a small bash test
     `tests/fixture/diagnose/integration/round-robin.sh` that
     runs the diagnostic three times in succession, with each
     run holding the previous run's slot, and asserts three
     different VMs were leased.
7. **Run the full test suite from scratch:**
   ```bash
   cargo test --workspace
   cargo clippy --all-targets -- -D warnings
   nix flake check .
   nix flake check ./tests/fixture
   just diag-fixture --flavor uifull   # end-to-end ladder
   ```
   All green.
8. **Update [tests/fixture/diagnose/README.md](../../../../tests/fixture/diagnose/README.md)**
   to remove the "Verdict sanity-check" line that references
   `ssh fixture@10.0.100.7` — replace with "the actual VM IP
   varies; check `verdict.json.leased_vm`".
9. **PR description checklist.** Confirm every phase
   (A → E) is referenced with its merge commit. The PR for
   this phase is the final landing point of the plan, so its
   body should serve as a recap.

## Acceptance criteria

- [ ] `cargo test --workspace` green.
- [ ] `cargo clippy --all-targets -- -D warnings` clean.
- [ ] `nix flake check .` and `nix flake check
      ./tests/fixture` both green.
- [ ] `just diag-fixture --flavor uifull` runs to GREEN
      verdict against a fixture with no placeholder claims
      and an unrelated cluster holding vm-1.
- [ ] The placeholder-claim block in
      [tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh)
      no longer exists (grep for `PLACEHOLDER_CLAIMS` returns
      zero matches outside of the planning docs).
- [ ] The diagnostic's `verdict.json` includes a
      `leased_vm` field (or equivalent) naming the actual
      VM ID used.
- [ ] README's typestate diagram matches the actual lease →
      ClusterConfig order. Spot-checked by reading the
      diagram against `src/main.rs`'s up flow.
- [ ] `tests/fixture/diagnose/integration/round-robin.sh`
      runs three iterations, holds the prior slot each time,
      and reports three distinct VMs leased.

## Files likely touched

- [README.md](../../../../README.md) — VM lifecycle, architecture
  overview sections.
- [docs/src/SUMMARY.md](../../SUMMARY.md) — only if SUMMARY needs
  re-wiring (probably already correct).
- [tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh).
- [tests/fixture/diagnose/steps/01-boot.sh](../../../../tests/fixture/diagnose/steps/01-boot.sh)
  — read leased VM ID from `up` output.
- [tests/fixture/diagnose/steps/02-compositor.sh](../../../../tests/fixture/diagnose/steps/02-compositor.sh)
  through `06-wayvnc-log.sh` — consume `VM_IP` from the file 01
  wrote, not a hardcoded default.
- [tests/fixture/diagnose/README.md](../../../../tests/fixture/diagnose/README.md).
- `tests/fixture/diagnose/integration/round-robin.sh` (new).
- (No `src/` changes expected; Phases B + D should have
  finished the Rust work.)

## Pitfalls

- **Parsing `Reserved: vm-N` is fragile.** A future log-line
  reformat breaks the diagnostic. Add a regex with a stable
  fixture and a unit test for the parser (in the diagnostic
  script's test harness — a tiny `bats` or shell-based
  assertion).
- **README drift across releases.** If anyone updates the
  architecture overview before this PR lands, the diff
  conflict is messy. Land Phase E promptly after D; don't
  let it sit for weeks.
- **The placeholder-claim removal regresses if Phase B was
  incomplete.** If you remove the placeholders and the
  diagnostic now lands on vm-1 (because lease still defaults
  to lowest-free), and vm-1 is held by another cluster, you
  see `vm-1 held by 'other-cluster', skipping` then no slots left.
  That's actually the *expected* behavior under flexible
  allocation; the user runs `cluster-ctl status` against the
  other cluster, decides to coexist or kick it, etc. Document
  this in the diagnostic's README — don't treat it as a
  Phase E bug.
- **`verdict.json` schema bump.** Adding `leased_vm` is a
  schema change. If anyone consumes verdict.json (CI?
  scripts?), bump the schema version too.

## Reference

- AUDIT.md from Phase A.
- Originating incident: 2026-05-11 diagnostic run; the
  placeholder-claim trick in
  [tests/fixture/diagnose/run.sh](../../../../tests/fixture/diagnose/run.sh).
- Related phases: A (audit), B (lease), C (runner-dir), D
  (integration) — all prerequisites.
- Downstream consumer: every phase of
  [../vnc-visual-testing/](../vnc-visual-testing/README.md)
  becomes simpler after this lands.
