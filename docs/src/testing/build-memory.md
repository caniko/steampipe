# Measuring build memory

This page documents how to measure peak memory use of a steampipe
`cargo build`. It exists because the obvious metric is misleading,
and getting the measurement wrong has produced false regressions.

## The obvious metric is misleading

`/usr/bin/time -v cargo build --workspace`'s `Maximum resident set
size` measures the **cargo orchestrator process** plus its terminated
children, which is dominated by whichever rustc invocation ran
biggest. It is **not** the peak link memory and not the system-wide
memory commitment.

When the dev profile is tuned (mold linker, cranelift dev backend,
`debug = "line-tables-only"`, `[profile.dev.package."*"] debug =
false` — all set by rs-harbor's `mkCargoConfig` defaults), the bulk
of system memory pressure during a build is the *parallel sum* of
rustc child processes, not any single child's peak. The MaxRSS
metric will not move when those settings change, even when the
build's actual host-side memory pressure changes substantially.

## What to measure instead

Two metrics worth tracking:

- **rustc VmHWM peak** — the highest `VmHWM` reached by any single
  rustc child during the build. Indicates per-process consolidation:
  when this rises, `codegen-units` was lowered (each rustc handles
  larger codegen units).
- **System memory committed** —
  `MemAvailable(start) - min(MemAvailable during build)`. Indicates
  aggregate host-side pressure. Noisy because other host processes
  consume/release memory during the few seconds the build takes;
  prefer relative comparisons across back-to-back runs.

`ld.lld` and `mold` are too short-lived to sample reliably at
80 ms cadence on this codebase (cranelift dev mode + cargo's
incremental linker complete the link step in well under 100 ms),
so neither shows up. The link step is not the bottleneck for
steampipe-shaped dev builds; rustc codegen is.

## Reproduction script

```bash
#!/usr/bin/env bash
# Measure peak VmHWM of cargo/rustc/clang/mold across a build.
set -uo pipefail
LABEL="${1:-unlabelled}"
OUT_DIR="${2:-/tmp}"
mkdir -p "$OUT_DIR"
SAMPLE_LOG="$OUT_DIR/build-${LABEL}-samples.log"
SUMMARY_LOG="$OUT_DIR/build-${LABEL}-summary.log"
BUILD_LOG="$OUT_DIR/build-${LABEL}-cargo.log"
: > "$SAMPLE_LOG"

sample_loop() {
  while true; do
    avail=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
    ts=$(date +%s.%N)
    echo "$ts MemAvailable $avail" >> "$SAMPLE_LOG"
    ps -e -o pid=,comm= 2>/dev/null \
      | awk '$2 ~ /^(ld\.lld|rustc|cargo|clang|mold)$/ {print $1, $2}' \
      | while read pid comm; do
          [ -r "/proc/$pid/status" ] || continue
          vmhwm=$(awk '/^VmHWM:/ {print $2}' /proc/$pid/status 2>/dev/null)
          [ -n "$vmhwm" ] && echo "$ts $comm $pid VmHWM=$vmhwm" >> "$SAMPLE_LOG"
        done
    sleep 0.08
  done
}

sample_loop & SAMPLER=$!
trap "kill $SAMPLER 2>/dev/null" EXIT

start_avail=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
command time -v cargo build --workspace > "$BUILD_LOG" 2>&1
sleep 0.1; kill $SAMPLER 2>/dev/null; wait $SAMPLER 2>/dev/null

peak_rustc=$(awk '$2=="rustc" {print $4}' "$SAMPLE_LOG" | sed 's/VmHWM=//' | sort -n | tail -1)
min_avail=$(awk '$2=="MemAvailable" {print $3}' "$SAMPLE_LOG" | sort -n | head -1)
committed=$(( start_avail - min_avail ))
echo "rustc VmHWM peak (kB): $peak_rustc" | tee "$SUMMARY_LOG"
echo "system memory committed (kB): $committed" | tee -a "$SUMMARY_LOG"
```

## Methodology

1. **Warm the pagecache** before measuring:

   ```bash
   cargo build --workspace && cargo clean
   ```

   Without this, the first measured run hits cold disk I/O and looks
   much slower / smaller-RSS than the second run hits, swamping any
   real signal from config changes. A 13× difference in fs-input
   blocks between cold and warm runs has been observed.

2. **`cargo clean` between configs**. Cargo's incremental cache
   invalidates on profile changes anyway, but force the issue.

3. **Two back-to-back runs per config**. Single-run noise on
   `MemAvailable` deltas is high; the second-of-two readings on
   both configs gives stable per-rustc-peak deltas.

4. **Compare relative, not absolute**. Atlas's idle MemAvailable
   varies 30-50 GiB depending on what other host services are
   doing; the absolute "system committed" number is only
   meaningful as a delta between configs measured close in time.

## Known result (2026-05-27)

Atlas, warm pagecache, `cargo build --workspace` after `cargo
clean`, mold linker + cranelift dev backend (rs-harbor defaults):

| Config | rustc VmHWM peak | system committed | wall |
|---|---:|---:|---:|
| no dev-profile opts | 1.13 GB | ~4.1 GB | 7.4 s |
| dev-profile opts + `codegen-units = 32` | 1.15 GB | ~4.2 GB | 7.1 s |
| dev-profile opts only (current) | 1.13 GB | ~3.3 GB | 7.2 s |

Lessons:

- `codegen-units = 32` raised per-process rustc peak by ~1.5%
  without a wall-clock win for steampipe; rs-harbor's
  `devCodegenUnits` parameter is left null on this project.
- The debug-info cuts (`debug = "line-tables-only"`,
  `split-debuginfo = "unpacked"`, deps `debug = false`) hold
  per-process rustc peak flat while cutting system commitment by
  ~20%. They are the actual reliability win.
- ld.lld peak was not observable; cranelift + mold finish the
  link in well under 100 ms. The peak that matters on this
  codebase is rustc codegen, not link.

If you tune the dev profile further, re-run the script above and
compare against the previous-best baseline rather than against
absolute numbers from a different host or kernel.

## See also

- [VM resources](../configuration/vm-resources.md) — runtime
  memory budgets for steampipe-managed VMs (a different memory
  surface, but the same "measure with VmHWM, not RSS" rule
  applies).
- rs-harbor's `mkCargoConfig` `enableDevProfileOpts` and
  `devCodegenUnits` parameters — the upstream knobs that this
  project leaves at defaults.
