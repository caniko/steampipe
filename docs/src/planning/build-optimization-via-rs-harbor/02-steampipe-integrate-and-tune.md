# Phase 02 — steampipe: bump rs-harbor input, integrate, add project-specific tuning, verify peak memory

> **Recommended Codex model: GPT 5.5 medium**
>
> Three coordinated edits: flake-input bump, `flake.nix` call-site
> change, project-specific `Cargo.toml` profile additions. Plus a
> measurement step that records actual peak memory on atlas.
> Mechanical given Phase 01 is in, but the design call is "which
> project-specific profile bits to add" (panic=abort variant?
> custom release-fast profile?). `low` would skip the measurement
> step; `high` is overkill for what's still under 50 lines of edits.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase:

1. `flake.lock` pins rs-harbor at a revision containing **Phase 01's
   amendment** (the split into `enableDevProfileOpts` +
   `devCodegenUnits`), not just the original bundled commit.
2. `flake.nix` calls `mkCargoConfig` (via rs-harbor's lib) such
   that the generated `.cargo/config.toml` includes the new
   `[profile.dev]` section. **`devCodegenUnits` is left unset
   (null)** — steampipe accepts cargo's default `codegen-units = 256`
   because empirical measurement showed `32` regresses workspace
   rustc peak by ~14% with no offsetting link-step benefit in dev
   mode. The call site may set `devCodegenUnits = null` explicitly
   for clarity.
3. Any pre-existing hand-rolled `.cargo/config.toml` content that
   conflicted with rs-harbor's generator is removed or moved to
   `extraConfig` passed to `mkCargoConfig`.
4. Steampipe-specific profile tuning lives in
   `/data/nvme0/can/Projects/steampipe/Cargo.toml`'s `[profile.*]`
   sections (workspace root). Concrete additions:
   - A `release` profile with `lto = "thin"`, `codegen-units = 16`,
     `strip = "symbols"`. (If those exist already, verify the values
     match the design.)
   - A `release-fast` custom profile that inherits from `release`
     and adds `panic = "abort"`, `strip = true`, used by callers
     who can tolerate "abort on panic" semantics.
   - **No** override of the dev profile keys that rs-harbor sets —
     accept the upstream floor.
5. Peak rustc VmHWM during `cargo clean && cargo build --workspace`
   on atlas with a warm pagecache is **no worse than 5% above** the
   pre-amendment baseline (~1.10 GB measured 2026-05-27). Recorded
   in the commit message body. The original "40% lower" target is
   abandoned — it was based on cargo's misleading MaxRSS metric
   and the assumption that `codegen-units = 32` was a pure win;
   neither held up.
6. No sccache references anywhere in steampipe.

## Why this matters now

Phase 01 lands the generic dev-profile bits in rs-harbor. Until
steampipe bumps its rs-harbor pin and re-evaluates, nothing in
this repo benefits. The bump is a single line in `flake.lock`; the
integration is roughly checking that nothing in `flake.nix` already
hand-rolls a `[profile.dev]` config that would conflict.

The project-specific additions are downstream of rs-harbor by
intent: every Rust project picks a different `[profile.release]`
shape based on its deployment targets and tolerance for panic-abort
semantics. Steampipe's choice goes here, not upstream.

## Out of scope

- Anything in rs-harbor (that was Phase 01).
- canix-side `systemd.user MemoryHigh` cap. That's host-level and
  belongs in canix, not steampipe — separate plan if wanted.
- Compilation cache restoration in any form. sccache removed from
  scope; no replacement.
- Release-cutting of steampipe itself. Independent.
- `target-dir` / `CARGO_TARGET_DIR` externalisation. Has serial-
  build limits; not worth the user-facing wrinkle.

## Plan

1. **Read the current `flake.nix` integration**:

   ```bash
   cd /data/nvme0/can/Projects/steampipe
   grep -nE "rs-harbor|mkCargoConfig|mkDevShell|cargoConfig" flake.nix nix/flake/*.nix 2>/dev/null
   ```

   Confirm steampipe already imports rs-harbor (it does — verified
   in the build-optimization research). Note the current call site
   for `mkCargoConfig`, if any. If `mkCargoConfig` isn't currently
   called, note that the dev shell uses a hand-written
   `.cargo/config.toml` or has none.

2. **Bump rs-harbor input** to include Phase 01's amendment:

   ```bash
   nix flake update rs-harbor
   ```

   The rs-harbor input lives on Codeberg with branch `main` (not
   `trunk`); confirm via `nix flake metadata` that the bumped rev
   matches the latest `main` HEAD.

   Verify the new pin includes the **amendment** (split parameters):

   ```bash
   cd /data/nvme0/can/Projects/rs-harbor
   git log --oneline main -3
   cd -
   nix flake metadata 2>&1 | grep -A2 rs-harbor
   ```

   The pinned rev's `lib/cargo-config.nix` must contain
   `devCodegenUnits` as a parameter name; if it only contains
   `enableDevProfileOpts` then the amendment hasn't landed yet and
   this phase blocks.

3. **Wire `mkCargoConfig` into steampipe's flake**. The call site
   already exists in the dev-shells code (from the prior
   integration attempt). Update it to explicitly leave
   `devCodegenUnits` unset:

   ```nix
   # In nix/flake/dev-shells.nix or flake.nix devShell:
   cargoConfig = rs-harbor.lib.${system}.mkCargoConfig {
     inherit pkgs;
     channel = "nightly";  # already used elsewhere
     # devCodegenUnits left at default null (no codegen-units line).
     # Rationale: measured 2026-05-27 — setting it to 32 regressed
     # workspace rustc peak by ~14% with no dev-mode link-step
     # benefit on this codebase. Leave it to cargo's default (256).
     # All other flags pick up rs-harbor defaults:
     #   enableMold = true
     #   enableCranelift = true (nightly)
     #   enableShareGenerics = true (nightly)
     #   enableParallelFrontend = true (nightly)
     #   enableDevProfileOpts = true   (debuginfo cuts only)
   };
   ```

   Pass that `cargoConfig` to `mkDevShell` (rs-harbor's
   `mkDevShell` already takes `cargoConfig` and writes it to
   `.cargo/config.toml` via the `cargoConfigHook`).

4. **Audit for residual hand-rolled config**:

   ```bash
   ls -la .cargo/ 2>/dev/null
   cat .cargo/config.toml 2>/dev/null
   ```

   If a hand-rolled `.cargo/config.toml` exists and contains
   anything rs-harbor's `mkCargoConfig` doesn't already emit, move
   those bits to `extraConfig` passed into the `mkCargoConfig`
   call:

   ```nix
   cargoConfig = rs-harbor.lib.${system}.mkCargoConfig {
     # ... defaults ...
     extraConfig = ''
       # steampipe-specific config that doesn't fit a flag
       [http]
       check-revoke = false  # example only
     '';
   };
   ```

   Delete the hand-rolled file; let the devshell hook write the
   generated one on shell entry.

5. **Add steampipe-specific `[profile.*]` to `Cargo.toml`**. In
   the workspace root `Cargo.toml`:

   ```toml
   [profile.release]
   lto = "thin"
   codegen-units = 16
   strip = "symbols"

   [profile.release-fast]
   inherits = "release"
   panic = "abort"
   strip = true

   # DO NOT add [profile.dev] here — rs-harbor's enableDevProfileOpts
   # already writes a sensible floor; setting our own here overrides
   # rs-harbor's per cargo precedence. Trust upstream.
   ```

   If `[profile.release]` already exists with different values,
   reconcile. Don't blindly replace.

6. **Measure peak memory on atlas with the right methodology**.
   Cargo's MaxRSS (`/usr/bin/time -v cargo build`) reports the
   cargo orchestrator's peak — not rustc/ld.lld. Use a VmHWM
   sampler across all build-tree processes instead.

   Pre-flight: warm the pagecache so cold-vs-warm doesn't dominate
   the diff:

   ```bash
   cargo build --workspace && cargo clean
   ```

   Sampler script (saved at `/tmp/measure-build.sh` from the
   2026-05-27 investigation; key bit reproduced here):

   ```bash
   sample_loop() {
     while true; do
       awk '/^MemAvailable:/ {print systime(), "MemAvailable", $2}' /proc/meminfo
       ps -e -o pid=,comm= 2>/dev/null \
         | awk '$2 ~ /^(ld\.lld|rustc|cargo|clang|mold)$/ {print $1, $2}' \
         | while read pid comm; do
             [ -r "/proc/$pid/status" ] || continue
             vmhwm=$(awk '/^VmHWM:/ {print $2}' /proc/$pid/status 2>/dev/null)
             [ -n "$vmhwm" ] && echo "$(date +%s) $comm $pid VmHWM=$vmhwm"
           done
       sleep 0.08
     done
   }
   ```

   Measure with the new config (devCodegenUnits = null):

   ```bash
   cargo clean
   sample_loop > /tmp/build-with-amendment-samples.log &
   SAMPLER=$!
   command time -v cargo build --workspace > /tmp/build-with-amendment.log 2>&1
   sleep 0.1; kill $SAMPLER
   awk '$2=="rustc" {print $4}' /tmp/build-with-amendment-samples.log \
     | sed 's/VmHWM=//' | sort -n | tail -1
   ```

   Compare against the **baseline** measurement from the
   2026-05-27 investigation: pre-amendment (rs-harbor with
   `codegen-units = 32` bundled) measured workspace rustc peak at
   ~1.25 GB across two runs. Post-amendment expectation: ~1.10 GB
   (the without-opts baseline from the same investigation, since
   without `codegen-units = 32` we should match cargo's default
   parallelism profile).

   Record both numbers in the commit message body. Target: rustc
   peak no worse than 5% above the without-opts baseline (~1.10 GB).

7. **Verify no sccache anywhere**:

   ```bash
   grep -rn "sccache\|SCCACHE\|rustc-wrapper" \
     --include="*.nix" --include="*.toml" --include="*.md" \
     --include="*.rs" --include="*.sh" \
     /data/nvme0/can/Projects/steampipe 2>/dev/null \
     | grep -v "/.git/\|/target/\|/result"
   ```

   Expected: zero matches (or only in this plan file's "no sccache"
   acceptance assertion text). If anything else turns up, remove it.

8. **`nix flake check --no-build`** must stay green.

9. **`cargo test --workspace`** must stay green (the new dev
   profile should not change correctness; if a test depends on
   debuginfo size or DWARF presence, that's a test bug).

10. Commit:

    ```
    build: bump rs-harbor for dev-profile opts; tune release profile

    Bumps rs-harbor flake input to <REV> (Phase 01 amendment that
    split codegen-units out of enableDevProfileOpts). Steampipe's
    devshell now writes a .cargo/config.toml that includes
    [profile.dev] with debug=line-tables-only,
    split-debuginfo=unpacked, and [profile.dev.package."*"]
    debug=false. devCodegenUnits left null per the 2026-05-27
    measurement showing codegen-units=32 regressed workspace rustc
    peak by ~14% with no dev-mode benefit.

    Adds steampipe-specific release profile shapes to Cargo.toml:
      [profile.release]: lto=thin, codegen-units=16, strip=symbols
      [profile.release-fast]: inherits release + panic=abort

    Peak rustc VmHWM, cargo clean && cargo build --workspace,
    warm pagecache, VmHWM sampler at 80 ms:
      pre-amendment (codegen-units=32): ~1,253,500 kB
      post-amendment (devCodegenUnits=null): <MEASURED> kB

    No sccache anywhere in the repo; explicit non-goal of the
    build-optimization plan.
    ```

## Acceptance criteria

- [ ] `git log -1 --oneline` shows the new commit.
- [ ] `cat .cargo/config.toml` from inside `nix develop` contains
      `debug = "line-tables-only"` and `[profile.dev.package."*"]`
      AND **does not** contain `codegen-units = ` (anywhere under
      `[profile.dev]`).
- [ ] `grep -rnE 'sccache|SCCACHE|rustc-wrapper' --include="*.nix"
      --include="*.toml" --include="*.md" /data/nvme0/can/Projects/steampipe`
      returns zero meaningful matches (planning-doc mentions of
      "no sccache" are acceptable; binary or env var invocations
      are not).
- [ ] `nix flake check --no-build` is green.
- [ ] `cargo test --workspace` is green.
- [ ] Commit message body records the measured peak rustc VmHWM
      (warm pagecache, VmHWM sampler).
- [ ] Recorded post-amendment rustc peak is **no worse than 5% above**
      the without-opts baseline measured 2026-05-27 (~1.10 GB).
      A post-amendment number in the 1.05-1.20 GB range counts as
      passing.
- [ ] `Cargo.toml` contains `[profile.release-fast]` inheriting
      `release` with `panic = "abort"`.

## Files likely touched

- `flake.lock` — rs-harbor input pin bump.
- `flake.nix` and/or `nix/flake/dev-shells.nix` — `mkCargoConfig`
  call (or pass-through to existing `mkDevShell`).
- `Cargo.toml` (workspace root) — new `[profile.release]` and
  `[profile.release-fast]` sections.
- `.cargo/config.toml` — gets regenerated on `nix develop` entry;
  the existing hand-rolled file (if any) gets backed up to
  `.cargo/config.toml.bak` per rs-harbor's `cargoConfigHook`.
  May warrant a `.gitignore` rule for the `.bak` file.

## Pitfalls

- **`flake.lock` bump granularity**: `nix flake update rs-harbor`
  bumps only the rs-harbor input. Don't accidentally
  `nix flake update` (no arg) — that bumps every input.
- **Hand-rolled `.cargo/config.toml`**: rs-harbor's
  `cargoConfigHook` backs up to `.bak` and overwrites. If the
  hand-rolled file contained something rs-harbor's generator can't
  emit, the loss is silent. Audit the file before triggering the
  hook.
- **`[profile.dev]` override in `Cargo.toml`**: do NOT add one in
  this phase. The whole point of Phase 01 was to make this
  upstream. If steampipe overrides locally, the bump is wasted.
- **Measurement methodology**: `/usr/bin/time -v`'s "Maximum
  resident set size" reports the parent `cargo` process's peak —
  not the child `rustc` / `ld.lld`'s peak. To capture the linker
  peak specifically, run `/usr/bin/time -v ld.lld --version` for
  reference, then watch with `pidstat -r 1 -p $(pgrep ld.lld)`
  during a fresh build. Both numbers are useful; the commit
  message can mention either or both.
- **`cargo clean` between measurements**: required. Cargo's
  incremental cache makes the second run trivially smaller.
  Force-clean each time.
- **panic = "abort" semantic change**: drops landing pads. If
  any test relies on `std::panic::catch_unwind` to recover, it
  fails under `release-fast`. Document the trade-off in
  `Cargo.toml` next to the profile.
- **`release-fast` profile name collision**: nothing in current
  cargo uses that name as a default, but check that no project
  flake script (e.g. `cluster-tournament-up`) refers to a profile
  by name that this phase happens to introduce.

## Reference

- Phase 01 (this plan's predecessor):
  [01-rs-harbor-dev-profile.md](./01-rs-harbor-dev-profile.md).
- rs-harbor `mkCargoConfig` source:
  `/data/nvme0/can/Projects/rs-harbor/lib/cargo-config.nix`.
- rs-harbor `mkDevShell` source:
  `/data/nvme0/can/Projects/rs-harbor/lib/dev-shell.nix` —
  `cargoConfigHook` at lines 46-62 is what writes the
  generated config into the worktree's `.cargo/config.toml`.
- Cargo `[profile.release-fast]` custom-profile pattern:
  <https://doc.rust-lang.org/cargo/reference/profiles.html#custom-profiles>.
- Originating evidence: 2026-05-26 SIGTRAP cascade journal at
  atlas 21:54. Phase 02's measurement step closes the loop by
  proving the mitigation works.
