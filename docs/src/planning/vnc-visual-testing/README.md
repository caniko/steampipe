# Plan: VNC visual testing for sway/wayland sessions

Decomposition of the visual-testing feature gap analysis from 2026-05-11
into seven independently-executable phases. The motivating downstream
symptom is Regicide UI-full VMs returning pure-noise VNC framebuffers and
Bevy/wgpu failing with `Fallback system failed to choose present mode` /
`Unable to find a GPU!` (see
[../../migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md)).

Each phase markdown is self-contained: a fresh Codex/Claude session can
pick up any one of these files cold and execute it without reading the
others. This index is for routing humans, not for chaining phase
context.

## Routing summary

| Phase | File | Model / effort | Why |
|---|---|---|---|
| A | [01-vnc-client-correctness.md](./01-vnc-client-correctness.md) | GPT-5.4 / medium | Bounded protocol-level bug fix; moderate complexity, leaf-role. |
| B | [02-compositor-readiness.md](./02-compositor-readiness.md) | GPT-5.4 / medium | New IPC integration; moderate complexity, sub-agent role. |
| C | [03-gpu-vulkan-preflight.md](./03-gpu-vulkan-preflight.md) | GPT-5.4 / medium | Mechanical CLI + parser plus error taxonomy design. |
| D | [04-visual-profile-config.md](./04-visual-profile-config.md) | GPT-5.5 / medium | Schema design that ripples into E/F/G; design discretion needed. |
| E | [05-validation-engine.md](./05-validation-engine.md) | GPT-5.5 / medium | SSIM + masks + ROI + bless workflow; complex orchestrator. |
| F | [06-scene-scripting.md](./06-scene-scripting.md) | GPT-5.4 / high | Sync-timing logic is the complexity driver; sub-agent with judgement. |
| G | [07-capture-lifecycle-and-report.md](./07-capture-lifecycle-and-report.md) | GPT-5.4 / medium | Integration + UI; not novel, just connect upstream pieces. |

No phase routes to `xhigh` — none of this work is frontier-complex. The
two `5.5/medium` phases (D and E) are design-heavy enough to warrant 5.5
over 5.4-high; everything else stays on 5.4.

## Dependency and parallelism matrix

```
Phase | Depends on    | Touches files                        | Can parallel with
A     | —             | src/game/capture.rs                  | C, D
B     | —             | src/game/capture.rs (new module),    | C, D
      |               | src/game/compositor.rs (new),        |
      |               | src/ui/cli.rs                        |
C     | —             | src/vm/preflight.rs, src/ui/cli.rs,  | A, B, D, E, F
      |               | nix/vm-graphics.nix                  |
D     | —             | src/core/config.rs, src/ui/cli.rs,   | A, B, C
      |               | docs/src/configuration/, SUMMARY.md  |
E     | D             | src/game/visual/* (new),             | F (once F's tail
      |               | src/game/capture.rs, src/main.rs,    |  is stable)
      |               | src/ui/cli.rs                        |
F     | B, D          | src/game/scene/* (new),              | E
      |               | src/game/capture.rs, src/ui/cli.rs   |
G     | A, E, F       | src/harness/runner.rs,               | —
      |               | src/harness/history.rs,              |
      |               | src/ui/report/* (new),               |
      |               | src/game/capture.rs                  |
```

### File-level serialisation

`src/game/capture.rs` is touched by **A, B, E, F, G**. Recommended landing
order: **A → B → (E ‖ F) → G** with rebases. If E and F run in parallel,
the first to land forces the second to rebase its edits to
`wayvnc_start_script` / `ensure_wayvnc` callsites; this is mechanical.

`src/ui/cli.rs` is touched by **B, C, D, E, F, G**. Conflicts are
add-only (each phase adds a new subcommand variant). Resolve by ordering
clap derive variants by phase letter; do not delete or reorder existing
variants.

### Logical sequencing

- **D is the schema gate.** E and F both consume D's config types. Land D
  before either E or F starts coding the consuming logic.
- **A is the capture-correctness gate.** Until A lands, every captured
  frame is suspect; visual-validation work on top of broken frames teaches
  bad lessons about thresholds. Land A before tuning any SSIM threshold
  in E.
- **B unblocks F.** F's sync predicates reuse Phase B's `check_readiness`.
  Without B, F has to inline the same logic and the duplication will need
  to be deleted later.
- **G is terminal.** It consumes outputs from A, E, and F.

## Suggested parallel waves

```
Wave 1 (start in parallel):  A, C, D
Wave 2 (after Wave 1):       B (rebase on A), E (after D), F (after B + D)
Wave 3 (after Wave 2):       G
```

If a single executor is running the plan sequentially, follow the file
order: 01 → 02 → 03 → 04 → 05 → 06 → 07. Phase C can shift earlier or
later within that order without consequence — it's fully independent.

## Setup before starting

- Confirm the repo's `cargo test` and `cargo clippy` baselines are
  green on trunk: `cargo test --workspace && cargo clippy --all-targets
  -- -D warnings`. Every phase's acceptance criteria assume these
  remain green.
- For phases that touch `nix/cluster-vm-base.nix` (C, G), confirm a
  cluster VM still builds:
  `nix build --no-link .#vm-runners` (or the project's specific
  attribute path).
- The plan's mdBook integration is wired through
  [docs/src/SUMMARY.md](../../SUMMARY.md). After landing phase docs,
  run `mdbook build` from the docs root to verify the planning section
  renders.

## Reference

- Source diagnosis: chat overview of 2026-05-11 enumerating the nine
  feature gaps (capture correctness, readiness, VNC client, validation,
  scene scripting, GPU preflight, lifecycle, config, reporting).
- Downstream evidence:
  [../../migration-baseline/smoke-status.md](../../migration-baseline/smoke-status.md)
  and [../../migration-baseline/README.md](../../migration-baseline/README.md).
- Existing surface area inventoried before planning:
  [../../../../src/game/capture.rs](../../../../src/game/capture.rs),
  [../../../../nix/vm-graphics.nix](../../../../nix/vm-graphics.nix),
  [../../../../nix/cluster-vm-base.nix](../../../../nix/cluster-vm-base.nix),
  [../../../../src/ui/cli.rs](../../../../src/ui/cli.rs),
  [../../../../src/main.rs](../../../../src/main.rs).
