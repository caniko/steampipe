# Phase 01 — rs-harbor: extend `mkCargoConfig` with `enableDevProfileOpts` + `devCodegenUnits`

> **Recommended Codex model: GPT 5.5 medium**
>
> Two new parameters with a deliberate split between "always-wins"
> (`enableDevProfileOpts`) and "tradeoff" (`devCodegenUnits`). The
> non-trivial bit is making sure the emitted `[profile.dev]` section
> composes cleanly with the existing cranelift section and that the
> two parameters compose independently. `low` would re-bundle them
> into one flag; `high` is overkill for ~40 lines of Nix.

## Working tree

`/data/nvme0/can/Projects/rs-harbor` (external repo to steampipe).

## Status as of 2026-05-27

The first cut of `enableDevProfileOpts` was already pushed to
rs-harbor's main:

- `e2c47cb lib: add mkCargoConfig dev profile opts`
- `2052c39 dev-shell: write cargo config with writable mode`

That initial cut **bundled** `codegen-units = 32` into the flag.
Empirical measurement on steampipe (warm pagecache, VmHWM sampler
across all build-tree processes, two runs) showed `codegen-units =
32` regresses per-process rustc peak by ~14% (from ~1.10 GB to
~1.25 GB) with no offsetting link-step benefit, because cargo dev
mode with cranelift barely exercises ld.lld and the codegen-units
consolidation just makes the workspace rustc handle a bigger CGU.

This phase's remaining work is the **amendment** that splits
`codegen-units` out of `enableDevProfileOpts` into its own
optional parameter `devCodegenUnits`. After the amendment lands:

- `enableDevProfileOpts = true` (default) emits only the three
  always-wins (line-tables-only debug, split-debuginfo unpacked,
  deps debug=false). No `codegen-units` line.
- `devCodegenUnits = N` (optional integer) emits `codegen-units =
  <N>`. Default `null` means no line emitted; cargo's default
  (256) stands.

## Goal

After this phase:

1. `rs-harbor`'s `mkCargoConfig` accepts `enableDevProfileOpts ?
   true`. When true (the default), the generated config includes:

   ```toml
   [profile.dev]
   debug = "line-tables-only"
   split-debuginfo = "unpacked"

   [profile.dev.package."*"]
   debug = false
   ```

   **No `codegen-units` line.**

2. `rs-harbor`'s `mkCargoConfig` accepts `devCodegenUnits ? null`
   (type `nullOr int`). When set to an integer `N`, the generated
   config includes `codegen-units = <N>` as an additional key
   under `[profile.dev]`. When `null`, no `codegen-units` line is
   emitted at all.

3. When both `enableDevProfileOpts = false` and `devCodegenUnits =
   null`, the generated config is byte-identical to the
   pre-extension output (modulo cosmetic header text).

4. The two parameters interoperate cleanly with the existing
   `enableCranelift` flag. All three may touch `[profile.dev]`-
   adjacent keys; either rs-harbor merges them into a single
   section, or emits multiple `[profile.dev]` table headers
   (cargo accepts repeated headers).

5. rs-harbor's mdBook documents both new parameters separately,
   their memory-saving intent vs codegen-units tradeoff, and
   cargo's precedence rules for downstream override.

6. The amendment is committed on rs-harbor's main and pushed.

## Why this matters now

mold (already in `mkCargoConfig`) cuts peak link memory but not
the codegen + dependency-debuginfo bloat. For a steampipe-sized
dependency graph, full DWARF debuginfo across every `tokio.rlib`
/ `serde.rlib` / `clap.rlib` adds ~2 GB to the in-memory link
working set. Most of that is unused — backtraces only need line
tables; per-dependency debuginfo is almost never inspected.

This phase is the only generic upstream-able piece of the build-
optimisation work (sccache removed from scope; everything else
already in rs-harbor). Without it, every consumer that wants the
debuginfo cut has to write the same `[profile.dev]` boilerplate
in their own project's `Cargo.toml`.

## Out of scope

- sccache or any other compilation cache. Removed from scope per
  user directive.
- `prefer-dynamic` flag. Dynamic-stdlib linking interacts with
  Nix's RPATH patching; reliability risk not worth the marginal
  memory saving on top of mold.
- Cargo wrapper changes (`mkDevShell` is untouched).
- Steampipe-side integration. Phase 02 handles that.
- Per-project `[profile.release]` tuning.
- A `wild` linker option. Still alpha; reliability priority says no.
- mdBook restructuring; only an additive section.

## Plan

This phase has two halves. The first half (`enableDevProfileOpts`
bundled with `codegen-units = 32`) already landed as commits
`e2c47cb` + `2052c39` on rs-harbor's main. The amendment below
splits `codegen-units` out into its own parameter.

1. **Audit existing `mkCargoConfig` consumers**. Inside rs-harbor:

   ```bash
   cd /data/nvme0/can/Projects/rs-harbor
   grep -rnE "mkCargoConfig\b" --include="*.nix" .
   ```

   Outside rs-harbor: steampipe is the only known external consumer
   (and its integration hasn't landed yet on `trunk` — it's still
   working-tree, so the amendment doesn't break any pinned consumer).

2. **Amend `lib/cargo-config.nix`** to split `codegen-units` out of
   `enableDevProfileOpts` and into a new optional parameter:

   ```nix
   {
     pkgs,
     channel ? "nightly",
     crossTargets ? [ ... ],
     enableMold ? true,
     enableCranelift ? (channel == "nightly"),
     enableShareGenerics ? (channel == "nightly"),
     enableParallelFrontend ? (channel == "nightly"),
     enableDevProfileOpts ? true,
     devCodegenUnits ? null,        # NEW (split out)
     extraConfig ? "",
   }:
   ```

   Validate the new parameter:

   ```nix
   assert pkgs.lib.assertMsg
     (devCodegenUnits == null || (builtins.isInt devCodegenUnits && devCodegenUnits > 0))
     "rs-harbor: mkCargoConfig 'devCodegenUnits' must be null or a positive integer";
   ```

   Change `devProfileSection` to drop the `codegen-units = 32`
   line (the three always-wins remain):

   ```nix
   devProfileSection = ''
     [profile.dev]
     debug = "line-tables-only"
     split-debuginfo = "unpacked"

     [profile.dev.package."*"]
     debug = false
   '';
   ```

   Add a separate `codegenUnitsSection` builder:

   ```nix
   codegenUnitsSection =
     if devCodegenUnits == null
     then ""
     else ''
       [profile.dev]
       codegen-units = ${toString devCodegenUnits}
     '';
   ```

   Compose into `configText`:

   ```nix
   configText = header
     + optionalString (targetSections != "") "\n${targetSections}"
     + optionalString enableCranelift "\n${craneliftSection}"
     + optionalString enableDevProfileOpts "\n${devProfileSection}"
     + optionalString (codegenUnitsSection != "") "\n${codegenUnitsSection}"
     + optionalString (extraConfig != "") "\n${extraConfig}\n";
   ```

   Update the file's docblock to describe both parameters and
   call out that `devCodegenUnits` is a tradeoff (wall-clock vs
   per-process peak), not a pure win.

3. **Handle the `[profile.dev]` repetition** that arises when both
   `enableCranelift` and `enableDevProfileOpts` are on. Two
   options:

   - **Option A (simpler)**: emit two separate `[profile.dev]`
     table headers. Cargo's TOML parser accepts repeated table
     headers and merges their keys. Verify with:

     ```bash
     nix run --inputs-from . nixpkgs#cargo -- read-manifest \
         --manifest-path /tmp/test-config.toml 2>&1 | head
     ```

     Or just trust the cargo manual that says "tables can be
     re-opened" (per TOML spec).

   - **Option B (cleaner output)**: merge the two sections inside
     `craneliftSection` and `devProfileSection` into one
     `[profile.dev]` block when both flags are on. Costs ~10 lines
     of Nix shape munging. Worth it for human-readable output.

   Pick A unless the merged output looks materially uglier.

4. **Verify the amended generated config**:

   ```bash
   cd /data/nvme0/can/Projects/rs-harbor
   nix eval --raw '(import ./lib/cargo-config.nix) {
     pkgs = import <nixpkgs> { };
     channel = "nightly";
   }' 2>&1
   ```

   Expected output (defaults: `enableDevProfileOpts = true`,
   `devCodegenUnits = null`):
   - `[target.x86_64-unknown-linux-gnu]` block with mold
   - `[unstable]` + cranelift `[profile.dev]` (if nightly)
   - A `[profile.dev]` section with **only** `debug = "line-tables-only"`
     and `split-debuginfo = "unpacked"`. **No `codegen-units` line.**
   - `[profile.dev.package."*"]` with `debug = false`

   Test with `devCodegenUnits = 16` (or any positive int): output
   gains a `codegen-units = 16` line under `[profile.dev]`.

   Test with both `enableDevProfileOpts = false` and
   `devCodegenUnits = null`: output is byte-identical to
   pre-amendment behaviour (modulo cosmetic header).

5. **Update or add integration checks** at
   `/data/nvme0/can/Projects/rs-harbor/checks.nix`:
   - `mkCargoConfig` defaults: contains `debug = "line-tables-only"`
     and `[profile.dev.package."*"]` but **does not** contain
     `codegen-units = `.
   - `mkCargoConfig { devCodegenUnits = 16; }`: contains
     `codegen-units = 16`.
   - `mkCargoConfig { enableDevProfileOpts = false; devCodegenUnits = null; }`:
     does not contain `debug = "line-tables-only"`,
     `split-debuginfo`, or `codegen-units`.

6. **Update mdBook docs** to describe both parameters separately:

   ```markdown
   ### `enableDevProfileOpts` (default: `true`)

   When enabled, `mkCargoConfig` writes a `[profile.dev]` section
   that reduces peak codegen and link memory. **All wins, no
   tradeoffs**:

   - `debug = "line-tables-only"` — backtraces still resolve;
     full DWARF is dropped (~50% debuginfo cut).
   - `split-debuginfo = "unpacked"` — what debuginfo remains lives
     in side `.dwo` files; never enters the linker's working set.
   - `[profile.dev.package."*"] debug = false` — drops dependency
     debuginfo entirely; only the workspace gets backtraces.

   This flag does **not** set `codegen-units`. That's a separate
   tradeoff with its own parameter below.

   ### `devCodegenUnits` (default: `null`)

   Optional integer. When set to `N`, writes `codegen-units = <N>`
   under `[profile.dev]`. When `null`, no `codegen-units` line is
   emitted and cargo's dev default (256) stands.

   **This is a tradeoff, not a pure win.** Lower codegen-units (e.g.
   32 or 16) means each rustc invocation handles a bigger CGU:
   lower wall-clock for the workspace crate, but higher per-process
   peak RSS. Pick a number when you've measured your specific
   project and decided the tradeoff is worth it.

   Cargo's profile precedence: keys in your project's
   `Cargo.toml [profile.dev]` override the same keys here. Set
   `enableDevProfileOpts = false` and `devCodegenUnits = null` to
   restore the full default cargo dev profile.

   Both parameters are pure `.cargo/config.toml` annotations; no
   new binaries enter your build environment.
   ```

7. **Commit on rs-harbor main**:

   ```bash
   cd /data/nvme0/can/Projects/rs-harbor
   git add lib/cargo-config.nix checks.nix docs/
   git commit -m "lib: split codegen-units out of enableDevProfileOpts

   The initial enableDevProfileOpts (e2c47cb) bundled
   codegen-units = 32. Empirical measurement on steampipe showed
   that bundling regresses workspace rustc peak by ~14% (1.10 GB
   to 1.25 GB) with no offsetting link-step win in cranelift +
   dev mode. The debuginfo cuts are pure wins; codegen-units is
   a tradeoff and deserves its own parameter.

   Adds devCodegenUnits :: nullOr int (default null). When set,
   emits codegen-units = <N> under [profile.dev]. When null, no
   line emitted; cargo's default (256) stands.

   Drops codegen-units = 32 from enableDevProfileOpts. Consumers
   that want the previous behaviour set devCodegenUnits = 32
   explicitly."
   ```

8. **Push to main**:

   ```bash
   git push origin main
   ```

## Acceptance criteria

- [ ] `(import /data/nvme0/can/Projects/rs-harbor/lib/cargo-config.nix)
      { pkgs = ...; channel = "nightly"; }` returns an attrset
      whose `.configText` contains
      `debug = "line-tables-only"` and
      `[profile.dev.package."*"]` but **does not** contain
      `codegen-units = `.
- [ ] The same call with `devCodegenUnits = 16` produces a
      `.configText` containing `codegen-units = 16`.
- [ ] The call with `enableDevProfileOpts = false; devCodegenUnits = null;`
      produces `.configText` that does not contain
      `debug = "line-tables-only"`, `split-debuginfo`, or
      `codegen-units`.
- [ ] Pre/post diff of the no-opt output is byte-identical
      (modulo cosmetic header) to pre-extension behaviour.
- [ ] `nix flake check` on rs-harbor is green.
- [ ] `mdbook build docs/` on rs-harbor is clean.
- [ ] rs-harbor's `main` has the amendment commit visible via
      `git log -1` and pushed to remote.
- [ ] **No sccache reference is added anywhere in this commit.**

## Files likely touched

- `lib/cargo-config.nix` — one new parameter + one new section
  emitter + one new line in `configText` composition + docblock
  update.
- `checks.nix` — optional new check asserting the flag's behaviour.
- `docs/src/` — mdBook section documenting the new flag.

## Pitfalls

- **`[profile.dev]` collision with cranelift section**. Both
  emit a `[profile.dev]` table header. Most TOML parsers accept
  repeated table headers and merge keys; cargo's does. Verify by
  generating an output, then running `nix run nixpkgs#cargo -- check
  --manifest-path /tmp/fake-Cargo.toml` against it — but easier
  to just emit one merged `[profile.dev]` block when both flags
  are on. Pick whichever shape passes the integration check.
- **`split-debuginfo = "unpacked"` only works on Linux + macOS**.
  Cargo silently ignores the key on Windows. Acceptable for now
  (rs-harbor's primary targets); document in the mdBook section
  that Windows users get no-op behaviour.
- **`codegen-units = 32` is a hard tradeoff**. Lower = less peak
  memory, slower wall-clock. 32 is a midpoint pick. Document the
  number explicitly so consumers know they can override.
- **Don't add sccache**. The whole reason for the rewrite was
  scope-removal. Double-check that no `rustc-wrapper`,
  `RUSTC_WRAPPER`, `SCCACHE_*`, or `pkgs.sccache` references slip
  in.
- **Default-on for the flag**. Old consumers that update their
  rs-harbor pin get the new sections automatically. That's
  intentional (memory savings should be universal), but it does
  mean a downstream consumer with `[profile.dev] debug = true`
  in their own `Cargo.toml` will keep their `debug = true` (cargo
  precedence) — verify the consumer's expectation matches.
- **Don't extend `mkDevShell`**. No new binaries belong in the
  dev shell for this phase; the dev-profile change is config-only.

## Reference

- rs-harbor source-of-truth:
  - `lib/cargo-config.nix` (the file this phase edits)
  - `lib/default.nix` (re-exports; no change)
  - `docs/` (mdBook to update)
- Cargo profile precedence:
  <https://doc.rust-lang.org/cargo/reference/config.html#profile>
- Cargo `split-debuginfo` reference:
  <https://doc.rust-lang.org/cargo/reference/profiles.html#split-debuginfo>
- Originating evidence (atlas SIGTRAP cascade): the
  hypervisor-restructure conversation thread + journal at
  2026-05-26 21:54.
