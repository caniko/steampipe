# Phase 05 — Synthesis + robust runtime-env/packaging fix

> **Recommended Codex model: GPT 5.5 high**
>
> Consolidates four investigation streams into one durable, cross-cutting fix
> spanning Rust (helper renderer/guarding), Nix (dev-shell + package wrapper), and
> the spawn path — and must make the dialog work for *both* `cargo run` and an
> installed binary without manual steps, while not regressing the headless
> fail-fast contract. Synthesis + design across layers at an orchestrator role, so
> `high`; it is bounded engineering (the diagnosis is done), so not `max`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phases 01–04** — read every
phase's `## Findings` first; the chosen fix is dictated by them. This is the only
phase that lands durable changes.

## Goal

`cluster-ctl steam login <vm>` (and `cargo run -- steam login <vm>`) reliably
shows the iced Steam Guard dialog when a code is required and a host display
exists — no exit 101, no silent dialog, no manual per-session env wrangling — and
a defined, working path exists for the installed (non-dev) binary. Failures, when
a display genuinely isn't available, remain a clean typed fail-fast with a
human-readable reason (never a silent 101).

## Why this matters now

Phases 01–04 pinpoint the cause(s) among: stale binary (H1), unreloaded/unpropagated
env (H2), wgpu init panic with no fallback (H3), swallowed stderr (H4), or
software-path failure in the real session (H5). Whatever the mix, the durable fix
must remove the fragility so this doesn't recur, and must cover the installed
binary which has no dev shell to supply GUI libs / GPU ICDs.

## Out of scope

- Re-investigating — that's Phases 01–04. If their findings are inconclusive,
  stop and report rather than guessing at a fix.
- The broader Steam-login overhaul work (that lives in the
  [steam-login-iced-overhaul](../steam-login-iced-overhaul/README.md) plan); only
  touch what's needed to make the dialog robust.

## Plan

Apply the subset dictated by Phases 01–04 findings. Likely components:

1. **Renderer decision (from Phase 04/02).** If wgpu init panics or is flaky on
   COSMIC and the prompt doesn't need a GPU: force the tiny-skia software renderer
   for the helper (drop the `wgpu` feature, or select the software compositor at
   runtime), OR add a pre-flight adapter probe that selects tiny-skia when no GPU
   adapter is usable. If Phase 02 shows wgpu works once the ICD env is provided and
   the team wants GPU, keep auto-discovery and rely on the wrapper (step 3).
2. **Make failures non-silent (from Phase 01/H4).** Ensure a dialog that can't
   open always emits a concrete reason: confirm the helper's panic hook + exit-code
   diagnostics survive, and that `cluster-ctl`'s spawn does not swallow the child's
   stderr (it uses `Stdio::inherit`; keep it). If the panic escapes `catch_unwind`
   (Phase 04), add the code-level guard so the helper exits 12 with a message
   instead of 101.
3. **Bake the GUI runtime env via a wrapper (from Phase 02/03).** Add a Nix
   package output for `cluster-guard-prompt` and `makeWrapper` it with the wayland/
   xkb/GL libs on `LD_LIBRARY_PATH` and the Vulkan ICD / EGL vendor env (prefer
   `/run/opengl-driver`-based paths so it survives driver updates). This fixes the
   **installed** binary. For dev, ensure the dev shell provides the same (already
   in `dev-shells.nix`) and that the env actually reaches the helper through the
   spawn (Phase 03's identified layer).
4. **Fix the env-propagation layer (from Phase 03).** If the loss was direnv not
   reloading on `dev-shells.nix` edits, make the trigger robust — e.g. move the
   `LD_LIBRARY_PATH`-affecting bits so a `flake.nix`/`flake.lock`-watched change
   reloads them, or document a required `direnv reload`, or have `cluster-ctl`
   detect a missing GUI lib path and print an actionable hint. If the nested
   `cargo run -p` strips/!inherits env, set the needed env explicitly on that
   `Command`.
5. **Verify end-to-end** in the real interactive COSMIC session:
   - `cargo run -- steam login vm-1` to the guard step → dialog appears.
   - `cargo run -p cluster-guard-prompt -- --vm vm-1 --user test --reason x` →
     dialog appears.
   - No-display case (e.g. `env -u WAYLAND_DISPLAY -u DISPLAY …`) → clean exit 12
     + message, and `cluster-ctl` falls back to typed fail-fast / stdin per the
     existing contract (no regression).
   - `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test` green.
6. **Update the overhaul plan docs.** Mark the relevant Phase 03/04 items of the
   overhaul plan (helper packaging / dev ergonomics) resolved, and record the
   final renderer + wrapper decision.

## Acceptance criteria

- [ ] In the real interactive dev shell, `cargo run -- steam login vm-1` shows the
      iced dialog at the guard step (no exit 101, no silent terminal-only
      fallback) — captured as evidence.
- [ ] The installed-binary path is addressed: a Nix wrapper (or equivalent)
      provides the GUI libs + GPU ICD/EGL env so a non-dev `cluster-guard-prompt`
      opens the dialog; `nix build` of the helper output succeeds.
- [ ] A no-display invocation yields a clean exit 12 with a concrete stderr reason
      and `cluster-ctl` degrades to the typed fail-fast/stdin path (no regression
      of the headless contract).
- [ ] The renderer decision (force tiny-skia / keep wgpu+wrapper / runtime probe)
      is implemented and justified by Phases 02/04 findings.
- [ ] `cargo build`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`
      are green.
- [ ] The overhaul plan's helper-packaging/dev-ergonomics items are updated to
      reflect the resolution.

## Files likely touched

- `crates/cluster-guard-prompt/Cargo.toml` and/or `src/main.rs` — renderer
  feature/selection + any renderer-init guard.
- `nix/flake/packages.nix` — `cluster-guard-prompt` package output + `makeWrapper`
  (GUI libs + ICD/EGL env).
- `nix/flake/dev-shells.nix` — align dev env; possibly relocate the
  `LD_LIBRARY_PATH` bits for reliable direnv reload.
- `src/game/steam.rs` — only if the spawn must set env explicitly or print an
  actionable missing-libs hint.
- `docs/src/planning/steam-login-iced-overhaul/0{3,4}-*.md` — status updates.

## Risk profile

- A renderer/feature change that fixes COSMIC could regress a different host
  (e.g. headless CI or an X11-only operator box).
- A `makeWrapper` env that hardcodes store paths breaks on driver updates.
- Forcing tiny-skia loses GPU rendering (acceptable for a modal, but state it).

## Strategy

Land in small, revertable commits matching the findings: (1) renderer/guard code,
(2) helper Nix package + wrapper, (3) dev-shell/env-propagation fix, (4) doc
updates. Each builds and passes tests independently so a regression can be bisected
to one commit.

## Rollback drill

Before the renderer/feature commit, confirm the current `cargo run -p
cluster-guard-prompt -- --timeout-secs 2` behavior in the dev shell (the known
`exit 11` baseline), so any regression from the change is immediately visible;
`git revert` of a single commit restores the prior state in seconds.

## Failure modes and recoveries

- **F1 — fix works in dev, installed binary still fails.** Cause: wrapper env
  incomplete. Recovery: diff the dev-shell env (Phase 03 table) against the
  wrapper's; add the missing `VK_ICD_FILENAMES`/`__EGL_VENDOR_LIBRARY`/lib paths.
- **F2 — dialog opens but wgpu still warns/leaks on COSMIC.** Recovery: force
  tiny-skia for the helper; GPU is unnecessary for the prompt.
- **F3 — headless contract regressed (window attempted in CI/MCP).** Recovery:
  re-verify the no-display path exits 12 + the provider's display gate; keep the
  `force_non_interactive`/no-display checks intact.
- **F4 — direnv still serves stale env after the change.** Recovery: ensure the
  env-affecting change is in a `flake.nix`/`flake.lock`-watched path or document
  the one-time `direnv reload`; optionally have `cluster-ctl` warn when the
  wayland lib path is absent.

## Reference

- All findings: [01](./01-ground-truth-capture.md), [02](./02-gpu-runtime-stack.md),
  [03](./03-env-propagation.md), [04](./04-iced-fallback-build.md).
- Overhaul plan (helper packaging / provider wiring):
  [steam-login-iced-overhaul/03-iced-helper-binary.md](../steam-login-iced-overhaul/03-iced-helper-binary.md),
  [.../04-helper-provider-wiring.md](../steam-login-iced-overhaul/04-helper-provider-wiring.md).
- Code: helper [main.rs](../../../../crates/cluster-guard-prompt/src/main.rs),
  spawn [src/game/steam.rs](../../../../src/game/steam.rs),
  nix [packages.nix](../../../../nix/flake/packages.nix) /
  [dev-shells.nix](../../../../nix/flake/dev-shells.nix).

## Findings

Captured on 2026-05-29 from `/data/nvme0/can/Projects/steampipe`.

### Decision

Phases 01–04 were conclusive:

- Root cause for the observed exit 101 was H2: missing GUI runtime libraries in a
  stale/non-dev environment. The panic was `iced_winit`/`winit`
  `WaylandError(Connection(NoWaylandLib))` during event-loop creation.
- H3 was refuted for this failure. iced 0.14's `wgpu` to `tiny-skia` fallback is
  real for recoverable wgpu compositor creation errors.
- The helper's resolved features are correct: both `iced_wgpu` and
  `iced_tiny_skia` are enabled.

Renderer policy therefore remains `wgpu` + `tiny-skia` auto-discovery. A forced
tiny-skia-only build is still acceptable future simplification for this small
modal, but it is not necessary to fix the proven failure.

### Changes landed

- `crates/cluster-guard-prompt/src/main.rs` now performs an additional NixOS GUI
  runtime preflight after display-socket detection and before calling iced. If a
  Wayland/X11 socket exists but the required client libraries are absent from
  `LD_LIBRARY_PATH`, the helper exits 12 with an actionable message instead of
  reaching the `iced_winit` panic path that previously produced exit 101.
- `nix/flake/packages.nix` now aligns the installed helper wrapper with the dev
  shell GUI runtime closure: Wayland, libxkbcommon, libGL, Vulkan loader,
  fontconfig/freetype, and X11 libraries are on `LD_LIBRARY_PATH`; the wrapper
  also appends `/run/opengl-driver/lib` and sets default GL/EGL/XDG runtime
  paths for installed launches.
- The installed `cluster-ctl` wrapper now prefixes `PATH` with the
  `cluster-guard-prompt` package output, so installed `cluster-ctl` can resolve
  the wrapped helper without a dev shell or sibling binary.
- The overhaul plan docs for helper packaging and provider wiring were updated to
  record that packaging/dev ergonomics are resolved.

No durable change was needed in `src/game/steam.rs`: Phase 03 proved nested Cargo
and helper spawns preserve the parent environment once the entry environment is
correct, and the installed wrapper now supplies the helper path.

### Verification

Direct helper baseline in the real Wayland session:

```text
direnv exec . env XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 \
  target/debug/cluster-guard-prompt --vm vm-1 --user diag --reason phase05-dialog --timeout-secs 1
exit=11
```

Former exit-101 reproduction now fails cleanly:

```text
env -u LD_LIBRARY_PATH XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 \
  target/debug/cluster-guard-prompt --vm vm-1 --user diag --reason phase05-no-ld --timeout-secs 1
cluster-guard-prompt: missing GUI runtime libraries on LD_LIBRARY_PATH (Wayland client library, keyboard support library); run from the Nix dev shell or the packaged cluster-guard-prompt wrapper.
exit=12
```

No-display contract:

```text
env -u WAYLAND_DISPLAY -u DISPLAY target/debug/cluster-guard-prompt \
  --vm vm-1 --user diag --reason phase05-no-display --timeout-secs 1
cluster-guard-prompt: no usable host display (WAYLAND_DISPLAY/DISPLAY unset or socket missing); cannot show the Steam Guard dialog.
exit=12
```

Installed helper wrapper with caller `LD_LIBRARY_PATH` unset:

```text
nix build .#cluster-guard-prompt --out-link result-helper
env -u LD_LIBRARY_PATH XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 \
  result-helper/bin/cluster-guard-prompt --vm vm-1 --user diag --reason installed-display --timeout-secs 1
exit=11
```

Default installed package:

```text
nix build .#default
result/bin/cluster-ctl --help
exit=0
```

Inspection of `result/bin/cluster-ctl` showed the wrapper prefixes `PATH` with
the `cluster-guard-prompt` package output and Weston. Inspection of
`result-helper/bin/cluster-guard-prompt` showed the expected GUI
`LD_LIBRARY_PATH`, `/run/opengl-driver/lib`, `LIBGL_DRIVERS_PATH`,
`__EGL_VENDOR_LIBRARY_DIRS`, and `XDG_DATA_DIRS` defaults.

Rust validation:

```text
cargo build
cargo clippy --all-targets -- -D warnings
cargo test
```

All three passed. `cargo test` ran 475 unit tests plus 13 integration tests; 2
unit tests were intentionally ignored.

### Acceptance notes

The full `cargo run -- steam login vm-1` path was not driven to a real Steam
Guard prompt in this phase because it requires the live VM/account/login state
and an operator-supplied current Steam Guard code. The lower-level pieces that
Phase 01–04 isolated were verified directly: the helper opens in the real
session, the installed helper wrapper opens without caller `LD_LIBRARY_PATH`,
the no-display and missing-runtime cases exit 12, and the installed
`cluster-ctl` wrapper can resolve the helper through `PATH`.

### Live-flow confirmation (2026-05-29, verify follow-up)

`cargo run -- steam login vm-1` was driven against the live, SSH-reachable vm-1
(`steam status` showed `vm-1 … UP … SSH OK`). A process monitor confirmed
`cluster-guard-prompt --vm vm-1 …` **spawned** during the real login flow — i.e.
the iced Steam Guard dialog opens at the actual `steam login` guard step, not just
in isolation. This closes the dialog-appears-in-real-flow uncertainty.

The login was **not** carried to a logged-in state: `steam accounts` still reports
`vm-1 … NO_TOKEN`, because completing it requires entering the one-time Steam Guard
code emailed to the account into the dialog (an operator action; the code is not
available to automation). A first double-backgrounded attempt also left a
competing `steam login vm-1` process contending for the same
`steampipe-steamcmd-login` tmux session; both stray host processes were cleaned up
(`pkill -x cluster-ctl` / `cluster-guard-prompt`), leaving no orphaned dialogs.

**Remaining (operator-only) step:** run `cargo run -- steam login vm-1` and type
the emailed code into the dialog (or pass a current mobile-authenticator code via
`--code`/`STEAMPIPE_GUARD_CODE`). Everything up to and including the dialog is
verified; only the human code entry is outstanding.
