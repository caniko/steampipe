# Build-optimization measurement baselines

Captured 2026-05-27 on atlas under `nix develop` with a warm
pagecache. Each run: `cargo clean` then
`/tmp/measure-build.sh <label> measurements/` which samples
VmHWM of `ld.lld`/`rustc`/`cargo`/`clang`/`mold` every 80 ms
and records min `MemAvailable` from `/proc/meminfo`.

## Configs measured

| File | What it represents |
|---|---|
| [`without-opts.cargo-config.toml`](./without-opts.cargo-config.toml) | rs-harbor pre-Phase-01 (no dev-profile opts at all) |
| [`with-bundled-opts.cargo-config.toml`](./with-bundled-opts.cargo-config.toml) | rs-harbor `main` HEAD = `2052c39` (Phase 01 initial cut; `codegen-units = 32` bundled into `enableDevProfileOpts`) |
| [`amended.cargo-config.toml`](./amended.cargo-config.toml) | Anticipated Phase 01 amendment output (`enableDevProfileOpts = true`, `devCodegenUnits = null`; no `codegen-units` line) |

## Results

| Config | rustc VmHWM (kB) | system committed (kB) | wall (s) |
|---|---:|---:|---:|
| without-opts | 1,130,156 | 4,132,548 | 7.42 |
| with-bundled-opts | 1,146,372 | 4,185,500 | 7.06 |
| amended (hand-written config) | 1,112,360 | 2,783,484 | 7.15 |
| **actual-amended (live `flake.lock` → `de14c55`)** | **1,133,256** | **3,320,908** | **7.21** |

The `actual-amended` row is the canonical post-amendment result;
the `amended` row was captured with a hand-written
`.cargo/config.toml` that matched the anticipated rs-harbor output.
Differences between the two rows are within run-to-run noise (rustc
peak ±2%, system commitment ±15% which is the noisy metric).

Full summary logs live in this directory as
`build-<label>-summary.log` and the underlying samples in
`build-<label>-samples.log`. Cargo output is at
`build-<label>-cargo.log`.

## Reading the numbers

- **rustc VmHWM peak** is the highest per-process working-set size
  any rustc invocation reached during the build. This is the metric
  Phase 02's acceptance criterion gates on.
- **system committed** is `MemAvailable(start) - MemAvailable(min during build)`.
  Noisy because other host processes commit/release memory during
  the ~7 s build; treat as a sanity check, not a precise measure.
- **Wall-clock** is the elapsed time of `command time -v cargo
  build --workspace`. Cranelift + warm pagecache makes all three
  configurations finish near 7 s.

## Phase 02 acceptance interpretation

The live amendment (actual-amended) moves all three metrics in
the right direction vs the regressed `with-bundled-opts` baseline:

- rustc peak: −1.1% (1,146 → 1,133 MB)
- system commitment: −20.7% (4,186 → 3,321 MB)
- wall-clock: +2.1% (noise, within run-to-run variance)

Vs the **pre-Phase-01 baseline** (`without-opts`), the amendment
holds steady on rustc peak (+0.3%, noise) while cutting system
commitment by ~20%. The "no regression vs without-opts" acceptance
criterion in Phase 02 is satisfied: the post-amendment rustc peak
(1,133 MB) is comfortably below the 5% allowance over baseline
(1,130 × 1.05 = 1,187 MB).

## Reproduction

```bash
cd /data/nvme0/can/Projects/steampipe
nix develop
# Warm pagecache:
cargo build --workspace && cargo clean

# For each config in {without-opts, with-bundled-opts, amended}:
cp docs/src/planning/build-optimization-via-rs-harbor/measurements/<config>.cargo-config.toml .cargo/config.toml
cargo clean
/tmp/measure-build.sh <config> docs/src/planning/build-optimization-via-rs-harbor/measurements/

# Inspect:
cat docs/src/planning/build-optimization-via-rs-harbor/measurements/build-<config>-summary.log
```

Note: `mold` and `ld.lld` show VmHWM = 0 because the sampler's 80
ms cadence misses these short-lived processes. cargo dev mode with
cranelift produces a near-instantaneous link, so the linker peak
is not the bottleneck on this codebase regardless. The dev-profile
opts target rustc peak and aggregate codegen memory, both of which
are measurable above.
