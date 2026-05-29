# Plan: Steam Guard dialog exit-101 investigation

> **Recommended Codex model for plan-set orchestration: GPT 5.5 high**
>
> This is a diagnosis across several interacting layers (a Rust GUI binary, the
> iced/winit/wgpu stack, the NixOS GPU/Wayland runtime, direnv + nested-cargo env
> propagation, and a COSMIC compositor). The failure is *masked* — exit 101 with
> no panic text — so the orchestrator must reason about which evidence is
> trustworthy and sequence the angles so a cheap finding (e.g. a stale binary)
> short-circuits the expensive ones. Diagnosis, not novel design, so `high` not
> `max`.

## Scope and current state

When `cargo run -- steam login vm-1` reaches the Steam Guard step, the host-side
iced dialog helper (`crates/cluster-guard-prompt`, spawned by `cluster-ctl`) exits
**101** and `cluster-ctl` prints:

```
  Steam Guard dialog exited unexpectedly (code 101); falling back to terminal input if available.
  Steam Guard dialog unavailable; falling back to terminal input.
Steam Guard code for caniko_jarvis_1 on vm-1 (no refresh token found):
```

The terminal fallback works, but the **dialog never appears**, and — critically —
**no helper panic message is printed** even though the helper installs a panic
hook that writes to stderr and `cluster-ctl` spawns it with inherited stderr.

Already attempted (and verified working *in isolation*):
- Dev-shell `LD_LIBRARY_PATH` for wayland/libxkbcommon/libGL/vulkan-loader/x11
  ([nix/flake/dev-shells.nix](../../../../nix/flake/dev-shells.nix)).
- wgpu + tiny-skia auto-discovery renderer
  ([crates/cluster-guard-prompt/Cargo.toml](../../../../crates/cluster-guard-prompt/Cargo.toml)).
- `cluster-ctl` spawns the helper via `cargo run -p cluster-guard-prompt` when run
  from a cargo tree ([src/game/steam.rs](../../../../src/game/steam.rs)
  `guard_prompt_command`).
- Helper panic hook prints the reason to stderr
  ([crates/cluster-guard-prompt/src/main.rs](../../../../crates/cluster-guard-prompt/src/main.rs)).

### The central clue (orient every phase around this)

Running the helper inside `nix develop -c bash -lc '… cluster-guard-prompt … --timeout-secs 2'`
**opens a window** (exit 11, clean). Running it via `cargo run -- steam login vm-1`
in an interactive shell **exits 101 with no stderr**. **The root cause is the
delta between those two environments / invocation paths.** Candidate deltas:
binary freshness, `LD_LIBRARY_PATH`/GPU-ICD env, whether the `cargo run -p` spawn
is even taken, and how stderr is routed.

### Leading hypotheses (to confirm/refute, not assume)

- **H1 — stale binary.** The running `cluster-ctl` predates the `guard_prompt_command`
  cargo-run-prefer change, so it executes a *stale* `target/debug/cluster-guard-prompt`
  sibling (a pre-panic-hook build) → silent exit 101. (`cargo run` showed
  `Finished in 0.58s` with no `Compiling` line — cluster-ctl was not rebuilt.)
- **H2 — env not propagated.** direnv wasn't reloaded after the dev-shell change,
  so the interactive shell lacks the wayland/GPU libs → winit `NoWaylandLib` (or
  wgpu ICD failure) panic; the `nix develop -c` test had them, hence the delta.
- **H3 — wgpu init panics (not Errs) and iced's fallback can't catch it.** Without
  `VK_ICD_FILENAMES`/EGL vendor on NixOS, wgpu may abort instead of returning an
  error iced can fall back from.
- **H4 — stderr swallowed.** The panic text is produced but lost (cargo, the
  spawn's stdio wiring, or buffering), making every failure look silent.
- **H5 — tiny-skia/softbuffer works under `nix develop -c` but not the real
  interactive COSMIC session** (seat/permission/protocol difference).

## Phase table

| Phase | File | Depends on | Touches | Can parallel with | Blocking? |
|---|---|---|---|---|---|
| 01 — Ground-truth capture & classification | [01-ground-truth-capture.md](./01-ground-truth-capture.md) | — | none (read/run only; temp instrumentation reverted) | — | **Yes** (gates 02–05) |
| 02 — wgpu/Vulkan/EGL/mesa runtime on NixOS + COSMIC | [02-gpu-runtime-stack.md](./02-gpu-runtime-stack.md) | 01 | none (investigation) | 03, 04 | No |
| 03 — Env & library propagation (direnv → cluster-ctl → cargo-run → helper) | [03-env-propagation.md](./03-env-propagation.md) | 01 | none (investigation) | 02, 04 | No |
| 04 — iced 0.14 renderer-fallback & build/feature correctness | [04-iced-fallback-build.md](./04-iced-fallback-build.md) | 01 | none (investigation; minimal throwaway repro) | 02, 03 | No |
| 05 — Synthesis + robust runtime-env/packaging fix | [05-synthesis-and-fix.md](./05-synthesis-and-fix.md) | 01–04 | `Cargo.toml`(helper), `src/game/steam.rs`, `nix/flake/{dev-shells,packages}.nix`, helper `main.rs` | — | No |

## Parallelism layer (execution waves)

- **Wave 0:** **Phase 01** alone. It is the lynchpin: it determines *which binary
  runs*, captures the *real* backtrace (unsilenced, `RUST_BACKTRACE=full`), and
  snapshots the env in the failing interactive context vs the working
  `nix develop -c` context. **If 01 conclusively proves H1 or H2 (stale binary /
  unreloaded env), STOP and apply the trivial fix — phases 02–04 become
  unnecessary.** Record that in 01's findings.
- **Wave 1 (only if 01 does not fully resolve it):** **Phases 02, 03, 04** run in
  parallel — three independent angles on the surviving hypotheses (GPU stack,
  env propagation, iced/wgpu behavior). They modify no shared files.
- **Wave 2:** **Phase 05** consolidates all findings and lands the durable fix
  (runtime-env wrapper / backend selection / packaging) so the dialog opens
  reliably from both `cargo run` and an installed binary, without manual steps.
- **Plan exhausted** after Wave 2.

## Whole-set acceptance criteria

- [ ] The exact cause of exit 101 is identified with evidence: which binary ran,
      the real panic/backtrace, and the precise env/invocation delta from the
      working `nix develop -c` case.
- [ ] `cargo run -- steam login vm-1` (in the project dev shell) shows the iced
      Steam Guard dialog when a code is required — no exit 101, no silent
      terminal-only fallback when a display is present.
- [ ] The fix works without manual per-session steps beyond entering the dev
      shell (and a documented one-time `direnv reload` if a flake change requires
      it), and a path is defined for the installed (non-dev) binary.
- [ ] Failures are never silent again: a dialog that cannot open prints a concrete
      reason to stderr.
- [ ] `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
      remain green.

## Global constraints

- **Investigation phases (01–04) must not change behavior.** Any instrumentation
  (extra logging, forced backends, `RUST_BACKTRACE`) is temporary and reverted;
  findings are recorded in each phase file's `## Findings` section. Only Phase 05
  lands durable changes.
- **Secrets:** never print a real Steam Guard code or password while capturing
  diagnostics; the dialog/provider already route through `redact_sensitive`.
- **Don't regress the working pieces:** the terminal-fallback, the typed
  `GuardCodeNeeded` outcome, and the dev-shell libs are correct — build on them.
- Record each phase's evidence in its own `## Findings` block (no shared
  findings file to avoid write conflicts); Phase 05 reads them all.

## Reference

- Overhaul dossier & plan: [steam-login-iced-overhaul-research.md](../steam-login-iced-overhaul-research.md),
  [steam-login-iced-overhaul/README.md](../steam-login-iced-overhaul/README.md)
  (esp. Phase 03 helper binary, Phase 04 provider wiring).
- Run each phase in a fresh `codex exec` session pointed at its file. Prompt
  `verify` when done to audit acceptance criteria.
