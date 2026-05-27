# Plan: Build Optimization via rs-harbor

> **Recommended Codex model for orchestration: GPT 5.5 medium**
>
> Two-phase linear chain (rs-harbor extension precedes steampipe
> consumption). One design call per phase. No frontier complexity;
> the upstream surface is one new flag on an existing helper.

## Scope and current state

Triggered by the SIGTRAP cascade on atlas: a `cargo build` inside
one VS Code session exhausted host memory enough that `ld.lld`
couldn't allocate a 4 KB page; Chromium's renderer hit an internal
assertion and SIGTRAPped, taking every VS Code window with it.

The non-trivial portions of the mitigation are already upstream in
`rs-harbor`'s `mkCargoConfig` / `mkDevShell` helpers (mold linker,
cranelift dev backend, share-generics, parallel frontend). What's
still missing upstream:

- **`enableDevProfileOpts`** (default `true`): pure peak-memory wins
  in the generated `.cargo/config.toml`. Emits
  `debug = "line-tables-only"`, `split-debuginfo = "unpacked"`, and
  `[profile.dev.package."*"] debug = false` for dependencies. Cuts
  what `ld.lld` and dep-rustcs have to swallow. Does **not** include
  codegen-units consolidation — that's a separate tradeoff and gets
  its own knob.
- **`devCodegenUnits ? null`**: optional integer parameter. When
  non-null, emits `codegen-units = <N>` under `[profile.dev]`.
  Default `null` means rs-harbor stays out of the codegen-units
  choice; cargo's dev default (256) stands. Projects that want
  consolidation (lower wall-clock, higher per-process peak) set the
  number explicitly. Steampipe leaves it null.

**History note** (2026-05-27): the first cut of `enableDevProfileOpts`
on rs-harbor's main (commits e2c47cb + 2052c39) bundled
`codegen-units = 32` into the flag. Empirical measurement on
steampipe showed that bundling regressed per-process rustc peak by
~14% with no offsetting link-step win in dev mode. The split into
two parameters is the corrective amendment.

What does **not** belong in rs-harbor and is therefore steampipe-side:

- `cargo build -j2` to serialise compiler parallelism (project-shape
  choice; some projects prefer the throughput).
- `[profile.release]` tuning variants (LTO mode, panic=abort,
  strip, codegen-units choice) — every project picks differently.
- Any per-project `Cargo.toml` overrides that win against
  rs-harbor's generated config.

Explicitly **not** in this plan: sccache. The user removed it from
scope. Cross-worktree compilation caching is forgone; the
mitigation relies on per-build peak-memory reduction alone.

## Phases

| Phase | File | Depends on | Touches | Tier | Wave |
|-------|------|------------|---------|------|------|
| 01 | [01-rs-harbor-dev-profile.md](./01-rs-harbor-dev-profile.md) | — | rs-harbor `lib/cargo-config.nix`, docs; commit + push to rs-harbor trunk | 5.5 medium | 0 |
| 02 | [02-steampipe-integrate-and-tune.md](./02-steampipe-integrate-and-tune.md) | 01 | steampipe `flake.nix`, `flake.lock`, `Cargo.toml` | 5.5 medium | 1 |

## Dependency table

| Phase | Depends on | Touches files | Can parallel with |
|-------|------------|---------------|-------------------|
| 01 | — | rs-harbor `lib/cargo-config.nix`, `docs/src/` | — (sole Wave 0 phase) |
| 02 | 01 | steampipe `flake.nix`, `flake.lock`, project `Cargo.toml` profile additions | — |

Strictly linear. Phase 02 needs Phase 01's amended commit visible
on rs-harbor's main branch (the split `enableDevProfileOpts` +
`devCodegenUnits` shape, not the initial bundled version) before
its flake-input bump can resolve the corrected helpers.

## Parallelism layer

**Wave 0** (Phase 01 alone): extend `mkCargoConfig` with two new
parameters: `enableDevProfileOpts` (default `true`, three pure
peak-memory wins) and `devCodegenUnits` (default `null`, optional
codegen-units number). Update rs-harbor's mdBook docs. Commit on
rs-harbor's main; push. Includes the amendment that splits
codegen-units out of the bundled flag.

**Wave 1** (Phase 02): bump steampipe's rs-harbor flake input,
have `flake.nix` call the extended `mkCargoConfig` (with
`devCodegenUnits` left unset / null), add steampipe-specific
Cargo.toml profile bits, and measure peak rustc + system
commitment on atlas with the right methodology (warm pagecache,
VmHWM sampler on all build-tree processes).

Plan exhausts at end of Wave 1.

## Whole-set acceptance criteria

- [ ] `rs-harbor`'s `mkCargoConfig` accepts `enableDevProfileOpts`
      (default `true`); when on, emits a `[profile.dev]` section
      with `debug = "line-tables-only"`,
      `split-debuginfo = "unpacked"`, plus
      `[profile.dev.package."*"] debug = false`. **No
      `codegen-units` line emitted by this flag alone.**
- [ ] `rs-harbor`'s `mkCargoConfig` accepts `devCodegenUnits`
      (default `null`, type `nullOr int`); when set to an integer
      `N`, emits `codegen-units = <N>` under `[profile.dev]`. When
      `null`, no `codegen-units` line is emitted and cargo's default
      (256) stands.
- [ ] When both `enableDevProfileOpts = false` and
      `devCodegenUnits = null`, the generated config is
      byte-identical (modulo unrelated cosmetic header) to the
      pre-extension output for the same other args.
- [ ] rs-harbor mdBook documents both new parameters and the
      cargo-precedence interaction with downstream `Cargo.toml`.
- [ ] rs-harbor's main has the amended commit visible via
      `git log` and pushed to remote.
- [ ] steampipe's `flake.nix` consumes the extended
      `mkCargoConfig` with `devCodegenUnits` left unset (or
      explicitly `null`); resulting `.cargo/config.toml` contains
      no `codegen-units = ` line under `[profile.dev]`.
- [ ] steampipe adds whatever project-specific Cargo.toml profile
      overrides it wants (Phase 02's design call).
- [ ] Peak rustc VmHWM during `cargo clean && cargo build
      --workspace` on a warm-pagecache atlas is **no worse than
      5% above** the pre-amendment baseline (measured ~1.10 GB
      with `codegen-units = 32`; the amendment should bring it
      back near or below this baseline since `codegen-units = 32`
      was the regression source).
- [ ] Phase 02's commit message body records the measured peak
      rustc VmHWM (warm-pagecache, before/after the amendment).
- [ ] No sccache binary or env var anywhere in steampipe,
      rs-harbor, or canix.

## Global constraints

- **Backward compat**: rs-harbor's existing consumers keep building.
  The new flag is additive (default-true is acceptable because the
  emitted sections are floors that downstream `Cargo.toml` can
  override).
- **No new linker / compiler dependencies**: all bits (mold,
  cranelift, share-generics) already live in rs-harbor. No
  binaries enter the dev shell that weren't there before.
- **No sccache**. Anywhere. The plan must keep the repo clean of
  sccache references.
- **No nightly-only additions to stable channel**: the new
  `enableDevProfileOpts` works on stable Rust (cargo's
  `.cargo/config.toml` profile precedence is stable).

## Reference

- Originating evidence: SIGTRAP at 21:54:14 +
  ld.lld page-allocation-failure at 21:54:17 in atlas's journal
  (vm-2-incident-adjacent investigation).
- rs-harbor helper source-of-truth:
  - `/data/nvme0/can/Projects/rs-harbor/lib/cargo-config.nix`
  - `/data/nvme0/can/Projects/rs-harbor/lib/dev-shell.nix`
- Adjacent steampipe planning context:
  [hypervisor-restructure/](../hypervisor-restructure/) (memory
  budgets and admission-gate work, distinct surface but related
  reliability theme).
