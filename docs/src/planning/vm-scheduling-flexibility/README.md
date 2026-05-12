# Plan: Flexible VM scheduling

Decomposition of the 2026-05-11 decision to make `cluster-ctl`
pick any available VM slot from the pool rather than always
starting from vm-1.

The originating incident: while iterating on the in-tree fixture
diagnostic, vm-1 was held by an unrelated cluster (`1v1`,
firecracker, pid 1618562). `cluster-ctl --vm-count 1 up` failed
because the lease scanner still picked vm-1, then failed to find
its (live) runner conflict. The fixture diagnostic worked around
this with a placeholder-claim hack — a workaround this plan
eliminates by making the scheduler properly skip held slots and
having every downstream consumer honour the leased set.

Five phases, all independently consumable.

## Routing summary

| Phase | File | Model / effort | Blocking? |
|---|---|---|---|
| A | [01-audit-and-design.md](./01-audit-and-design.md) | GPT-5.5 / medium | Gates B, C, D, E |
| B | [02-generalize-lease.md](./02-generalize-lease.md) | GPT-5.4 / high | Gates D, E |
| C | [03-runner-dir-full-range.md](./03-runner-dir-full-range.md) | GPT-5.4 / medium | Gates D, E |
| D | [04-lifecycle-integration.md](./04-lifecycle-integration.md) | GPT-5.4 / high | Gates E |
| E | [05-tests-docs-cleanup.md](./05-tests-docs-cleanup.md) | GPT-5.4 / medium | — |

No phase routes to 5.5/high or xhigh — none of this work is
frontier-complex. Phase A is the only 5.5-tier phase, and only
because its decisions ripple through the rest of the plan.

## Dependency and parallelism matrix

```
Phase | Depends on | Touches files                          | Can parallel with
A     | —          | docs/.../AUDIT.md                      | —
B     | A          | src/vm/lease.rs, src/core/config.rs,   | C
      |            | src/main.rs, src/vm/lifecycle.rs       |
C     | A          | nix/lib/test-cluster.nix,              | B
      |            | tests/fixture/flake.nix                |
D     | B, C       | src/main.rs branches,                  | —
      |            | src/vm/{snapshot,status}.rs,           |
      |            | src/net/netem.rs, src/ui/watch.rs      |
E     | B, C, D    | README.md, tests/fixture/diagnose/*,   | —
      |            | docs/src/SUMMARY.md                    |
```

### Suggested waves

- **Wave 1:** A (alone).
- **Wave 2:** B + C in parallel.
- **Wave 3:** D (single).
- **Wave 4:** E (single).

## Relationship to other plans

This plan is a prerequisite for clean execution of:

- [`../in-tree-test-bed/`](../in-tree-test-bed/README.md) — Phase D's
  diagnostic ladder currently relies on a placeholder-claim hack
  to force vm-7. Phase E here removes that hack.
- [`../vnc-visual-testing/`](../vnc-visual-testing/README.md) — every
  phase that boots a fixture VM benefits from "land on any free
  slot."

Neither blocks this plan; the cleanup just becomes mechanical
once flexible scheduling lands.

## Concrete next-step suggestion

Start with Phase A. The audit is the single most expensive
deliverable but unblocks everything; ~2 hours of focused
grep+read+write. After A lands, fan out B and C to two
sessions in parallel; serialise D and E behind them.
