# Plan: In-tree test bed for cluster validation

Decomposition of the 2026-05-11 decision to build an in-tree test
fixture so the steampipe cluster can be exercised — booted, captured,
diagnosed — without coupling to any specific downstream consumer
(currently regicide). After this plan lands, the
[../vnc-visual-testing/](../vnc-visual-testing/README.md) phases can
all run inside this repo against deterministic graphical workloads.

Four phases, all independently consumable by a fresh agent session:

## Routing summary

| Phase | File | Model / effort | Why |
|---|---|---|---|
| A | [01-fixture-scaffolding.md](./01-fixture-scaffolding.md) | GPT-5.4 / medium | Nix flake + keypair + minimal toml; moderate-complexity sub-agent. |
| B | [02-deterministic-workloads.md](./02-deterministic-workloads.md) | GPT-5.4 / medium | Three packaged workloads with determinism constraints; moderate. |
| C | [03-golden-image-library.md](./03-golden-image-library.md) | GPT-5.4 / low | Capture, compress, manifest, document — mostly procedural. |
| D | [04-diagnostic-ladder.md](./04-diagnostic-ladder.md) | GPT-5.4 / medium | Shell + classifier with non-trivial verdict logic. |

No phase needs 5.5 — none of this work is design-frontier. Phase C
is the cheapest tier in the plan and intentionally so: once Phases A
and B exist, capturing a golden is mechanical.

## Dependency and parallelism matrix

```
Phase | Depends on | Touches files                        | Can parallel with
A     | —          | tests/fixture/{flake.nix,            | — (gate phase)
      |            | steampipe.toml, cluster_key,         |
      |            | README.md}                           |
B     | A          | tests/fixture/workloads/*,           | — (touches flake.nix
      |            | tests/fixture/flake.nix              |    set by A)
C     | A, B       | tests/fixture/golden/*,              | D
      |            | tests/fixture/README.md              |
D     | A, B       | tests/fixture/diagnose/*,            | C
      |            | justfile, .gitignore                 |
```

### Sequencing notes

- **A is the gate.** Nothing else in this plan can start before
  Phase A produces a bootable fixture. The downstream
  `vnc-visual-testing` plan also benefits from A landing first,
  but does not strictly require it (those phases could use the
  regicide checkout until this fixture is ready).
- **B blocks both C and D.** Goldens (C) and diagnostic captures
  (D) both target on-screen content that the workloads (B)
  produce.
- **C and D can run in parallel** once B lands. They touch
  disjoint files. D references C's golden directory by path but
  can be developed with a placeholder golden until C is ready,
  then re-pointed at the real one.

### File-level serialisation

- `tests/fixture/flake.nix` is touched by **A and B**. A creates
  it, B extends it with `packages` and `workloads.nix` import.
  Land A first.
- `tests/fixture/README.md` is touched by **A, B, C**. Each phase
  appends a section: A writes the intro + key disclaimer + boot
  commands; B adds the workloads section; C adds the goldens
  section. Last-writer-wins on overlapping rebases — keep the
  section order stable.

## Relationship to the visual-testing plan

This plan is a *prerequisite test bed* for
[../vnc-visual-testing/](../vnc-visual-testing/README.md). Order of
operations across both plans:

```
in-tree-test-bed A → B → (C ‖ D)
                            ↓
vnc-visual-testing  A → B → (C ‖ D) → (E ‖ F) → G
```

The visual-testing plan's Phase A (VNC client correctness) and
Phase C (GPU preflight) can start in parallel with this plan's
Phase A — they touch entirely different files. But validating
either visual-testing phase end-to-end requires this plan's
fixture, so coordinate landing order: get this plan's A+B+C+D
green before declaring any visual-testing phase done.

## Setup before starting

- Confirm trunk is green: `cargo test --workspace && cargo
  clippy --all-targets -- -D warnings`.
- Read [nix/lib/test-cluster.nix](../../../../nix/lib/test-cluster.nix)
  end-to-end before starting Phase A. The `mkTestCluster`
  signature is the load-bearing API for the entire fixture and
  has never been called from inside this repo before.
- Confirm host has `nix`, `cargo`, `just`, `ssh`, `rsync`, and
  enough disk for the microvm runner store closure (~3 GB warm,
  ~6 GB cold).
- Confirm no other cluster (e.g. regicide's `br-cluster`) is up
  with a conflicting bridge name. Phase A uses `br-fixture` to
  avoid this.

## Concrete next-step suggestion

Start with Phase A. It's the gate; everything else is blocked on
it. Expect ~1 hour of focused Nix work to get the first VM boot
plus another 30 minutes for verification. After A lands, fan out
B in one session and start scaffolding D in parallel; bring C in
once B's `foot-banner` workload is reliably producing a visible
window.
