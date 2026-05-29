# Phase 03 — Env & library propagation (direnv → cluster-ctl → cargo-run → helper)

> **Recommended Codex model: GPT 5.4 medium**
>
> A concrete, bounded tracing task: follow `LD_LIBRARY_PATH` and the GPU/Wayland
> env from the dev shell through `cluster-ctl`'s `cargo run -p` spawn down to the
> helper process, and confirm what each layer sees. It's leaf-shaped
> evidence-gathering with no design decisions, so the production workhorse at
> medium effort is the right cost/quality point; a frontier model would be wasted
> here.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Depends on Phase 01** — read its
`## Findings`; skip if 01 resolved the failure. **Investigation only**; record in
`## Findings`.

## Goal

Prove exactly what environment each layer in the launch chain sees, and where (if
anywhere) the GUI runtime env is lost: the interactive shell (direnv-loaded dev
shell?) → `cluster-ctl` (via `cargo run`) → the nested `cargo run -p
cluster-guard-prompt` spawn → the helper process. Output: a per-layer env table
identifying the layer at which `LD_LIBRARY_PATH` (wayland/GL) and any GPU ICD env
is present or missing, and whether the nested-cargo spawn preserves or strips it.

## Why this matters now

The dev-shell fix added `LD_LIBRARY_PATH` via `extraShellHook` in
[nix/flake/dev-shells.nix](../../../../nix/flake/dev-shells.nix). But: (1) direnv's
`use flake` watches `flake.nix`/`flake.lock`, **not** `nix/flake/dev-shells.nix`,
so the interactive shell may still have the *old* env (README H2); and (2)
`cluster-ctl` spawns the helper through `cargo run -p cluster-guard-prompt`
([src/game/steam.rs](../../../../src/game/steam.rs) `guard_prompt_command`), so the
helper's env is whatever `cluster-ctl` inherited *and* whatever cargo passes
through. If `LD_LIBRARY_PATH` is absent at the helper, winit fails with
`NoWaylandLib` (exit 101) regardless of renderer — which would look exactly like
the reported symptom.

## Out of scope

- *What* GPU inputs are needed (ICD/EGL specifics) — Phase 02.
- iced/wgpu internal behavior — Phase 04.
- Landing the durable env/wrapper fix — Phase 05 (this phase says *where* the env
  is lost so 05 fixes it at the right layer).

## Plan

1. **Confirm dev-shell load state.** In the interactive shell that fails: `direnv
   status`, `echo "$DIRENV_DIR"`, and whether `LD_LIBRARY_PATH` contains a
   `wayland` store path. Then `direnv reload` (or re-enter `nix develop`) and
   re-check. Record before/after — this directly tests README H2.
2. **Layer 1 — interactive shell env.** Capture `LD_LIBRARY_PATH`,
   `WAYLAND_DISPLAY`, `DISPLAY`, `XDG_RUNTIME_DIR`, and any `VK_*`/`__EGL_*`
   (cross-ref Phase 01/02).
3. **Layer 2 — what `cluster-ctl` sees.** Add a temporary debug print (or use
   `/proc/self/environ`) so `cluster-ctl` logs its own `LD_LIBRARY_PATH` at
   startup, OR run `cargo run -- <something trivial>` under
   `strace -f -e trace=execve -s 4096` and inspect the env passed to the
   `cluster-ctl` exec. Revert any code print.
4. **Layer 3 — what the nested `cargo run -p` spawn passes.** This is the crux:
   does `cluster-ctl`'s `tokio::process::Command` for `cargo run --quiet -p
   cluster-guard-prompt` inherit the full env (it should, by default), and does
   the inner **cargo** forward it to the helper unchanged? Verify by temporarily
   pointing the helper at a tiny script (via `STEAMPIPE_GUARD_PROMPT_BIN`) that
   dumps `env` to a file, then trigger the guard path (or call the provider in a
   focused harness). Compare the helper's env to Layer 1.
5. **Layer 4 — direct helper run.** Run `target/debug/cluster-guard-prompt …`
   directly in the interactive shell (Phase 01 may have done this) and confirm
   whether it has the libs. This isolates "env reaches the helper" from "cluster-ctl
   spawn strips env".
6. **Pinpoint the loss.** Identify the first layer where `LD_LIBRARY_PATH`
   (wayland) is missing. Common culprits: direnv not reloaded (Layer 1), or the
   spawn path not actually taken (it ran a sibling with a different env), or cargo
   sanitizing env (unlikely but verify).
7. **Note the installed-binary gap.** Record that an installed (non-dev)
   `cluster-ctl` has no dev shell at all, so Phase 05 must bake the env via a
   wrapper regardless of the dev-shell outcome.

## Acceptance criteria

- [ ] `## Findings` shows the before/after of `direnv reload` on the interactive
      shell's `LD_LIBRARY_PATH` (wayland present?), resolving README H2.
- [ ] A per-layer env table (interactive shell → `cluster-ctl` → nested `cargo
      run -p` → helper) identifies the exact layer where the GUI runtime env is
      first missing (or confirms it is present end-to-end).
- [ ] It is proven whether the helper process actually receives `LD_LIBRARY_PATH`
      with the wayland/GL libs when launched via `cluster-ctl`'s spawn path.
- [ ] A one-line statement of the propagation defect (if any) and which layer
      Phase 05 must fix, plus the note that the installed binary needs a wrapper.
- [ ] No durable repo changes; instrumentation reverted.

## Files likely touched

- None durably. Temporary, reverted: a startup `eprintln!` of `LD_LIBRARY_PATH`
  in `cluster-ctl`, and/or a throwaway env-dump script pointed to by
  `STEAMPIPE_GUARD_PROMPT_BIN`.
- This phase file's `## Findings`.

## Pitfalls

- **direnv watches only `flake.nix`/`flake.lock`.** Editing
  `nix/flake/dev-shells.nix` does not auto-reload the shell; a stale shell is the
  likeliest H2 culprit. *Recovery:* `direnv reload` (or `touch flake.nix`) and
  re-test; if that fixes it, the durable answer is to make the reload trigger
  obvious (Phase 05 may `touch flake.nix` or document it).
- **`cargo run` env passthrough.** cargo generally forwards the parent env to the
  run target, but confirm — and remember the helper is launched by an *inner*
  cargo, so the chain is shell → cargo(cluster-ctl) → cluster-ctl → cargo(helper)
  → helper. *Recovery:* the env-dump script at Layer 3 is the definitive probe.
- **Sibling vs cargo-run path.** If `cluster-ctl` took the sibling path (stale or
  not), the env analysis still applies but the binary may differ. Cross-ref Phase
  01's determination of which path ran. *Recovery:* force a known path via
  `STEAMPIPE_GUARD_PROMPT_BIN` while probing.
- **Don't conflate env-missing with GPU-missing.** If the env is fine and the
  helper still fails, it's a Phase 02 (GPU) or Phase 04 (iced) issue, not
  propagation. *Recovery:* hand off with the env table proving env is sufficient.

## Reference

- README H2 (unreloaded env) and the env-delta framing: [README.md](./README.md).
- Dev shell hook: [nix/flake/dev-shells.nix](../../../../nix/flake/dev-shells.nix).
- Spawn logic: `guard_prompt_command` in [src/game/steam.rs](../../../../src/game/steam.rs).
- Phase 01 env diff (reuse it): [01-ground-truth-capture.md](./01-ground-truth-capture.md).
- Feeds Phase 05 (fix the right layer + installed-binary wrapper).

## Findings

Captured on 2026-05-29 from `/data/nvme0/can/Projects/steampipe`.

Phase 01 already resolved the observed exit-101 failure as H2 (`LD_LIBRARY_PATH`
missing in a stale direnv shell), so this phase stayed investigation-only and
reused that failing-shell evidence rather than forcing a real Steam Guard login
again.

### Direnv reload before/after

The failing interactive-shell evidence comes from Phase 01's exact before/after
probe:

| Shell state | `LD_LIBRARY_PATH` | Wayland lib present? | Result |
| --- | --- | --- | --- |
| `direnv exec .` before reload (`nix-direnv: Using cached dev shell`) | empty | no | helper failed with `WaylandError(Connection(NoWaylandLib))`, exit 101 |
| after `direnv reload` | `/nix/store/...-wayland-1.24.0/lib:/nix/store/...-libxkbcommon-1.13.1/lib:/nix/store/...-libglvnd-1.7.0/lib:/nix/store/...-vulkan-loader-1.4.341.0/lib:...` | yes | helper opened and timed out normally, exit 11 |
| `nix develop -c` | same GUI lib path as post-reload `direnv exec .` | yes | helper opened and timed out normally, exit 11 |

That resolves README H2 directly: the loss happened in the stale interactive
shell before `cluster-ctl` was even launched.

### Current shell context

This Codex process itself is not inside the user's direnv shell, so its raw
environment is not the failing launch context:

```text
DIRENV_DIR=
WAYLAND_DISPLAY=
DISPLAY=
XDG_RUNTIME_DIR=
LD_LIBRARY_PATH=
```

Fresh dev-shell entrypoints are correct today:

- `direnv exec .` now exports the GUI library path and `MESA_VK_DEVICE_SELECT=1002:744c`.
- `nix develop -c` exports the same GUI library path and the same
  `MESA_VK_DEVICE_SELECT=1002:744c`.
- No `VK_*` or `__EGL_*` variables were present in either fresh shell.

### Per-layer env table

The relevant launch chain was traced with `strace` for the outer `cargo run ->
cluster-ctl` exec, plus a temporary `CARGO` wrapper and Cargo target `runner`
for the inner `cargo run -p cluster-guard-prompt` path. No source files were
edited.

| Layer | Probe | `LD_LIBRARY_PATH` | Display vars | GPU/ICD env | Conclusion |
| --- | --- | --- | --- | --- | --- |
| 1. Interactive shell, stale cache | Phase 01 `direnv exec .` before reload | empty | caller provided `XDG_RUNTIME_DIR=/run/user/1000`, `WAYLAND_DISPLAY=wayland-1` during repro | none | First missing layer. This is the propagation defect behind exit 101. |
| 1b. Interactive shell, renewed | Phase 01 `direnv reload`; current `direnv exec .` / `nix develop -c` | `/nix/store/...-wayland...:/nix/store/...-libxkbcommon...:/nix/store/...-libglvnd...:/nix/store/...-vulkan-loader...:...` | not set by the dev shell itself | `MESA_VK_DEVICE_SELECT=1002:744c` only | Correct GUI library env is present once the shell is fresh. |
| 2. `cargo run -> cluster-ctl` | `strace -f -e execve -v -s 4096 direnv exec . cargo run --quiet -- --help` | present via direnv shell hook (`export LD_LIBRARY_PATH="...wayland...libglvnd...vulkan-loader..."`) on the `cluster-ctl` exec path | only whatever caller exports; none were added here | same shell env | `cluster-ctl` inherits the renewed dev-shell env; no stripping observed. |
| 3. `cluster-ctl -> cargo run -p cluster-guard-prompt` | direct execution of the existing ignored unit-test binary with `CARGO=/tmp/.../cargo-wrapper.sh` | wrapper saw the same dev-shell GUI library path, including the Wayland/libGL/Vulkan entries | none were added here | no extra `VK_*`/`__EGL_*`; runner env preserved `MESA_VK_DEVICE_SELECT` only when present upstream | `guard_prompt_command` used `cargo run --quiet --package cluster-guard-prompt -- ...` and preserved the parent env. |
| 4. helper process launched by inner Cargo | Cargo target runner `/tmp/.../helper-runner.sh` | still contains the same Wayland/libGL/Vulkan entries, but Cargo prepends `target/debug`, `target/debug/deps`, Rust stdlib, and `libsecret` paths | same caller-provided display env model | same as parent | Inner Cargo does not strip the GUI library path; it augments `LD_LIBRARY_PATH` and forwards it to the helper launch. |
| 5. Direct helper run | `env XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 direnv exec . target/debug/cluster-guard-prompt ... --timeout-secs 1` | present | `XDG_RUNTIME_DIR=/run/user/1000`, `WAYLAND_DISPLAY=wayland-1` | `MESA_VK_DEVICE_SELECT=1002:744c` | direct helper launch timed out cleanly (`exit=11`), confirming the renewed shell env is sufficient. |

### Concrete traces

Outer `cluster-ctl` launch path:

```text
strace -f -e trace=execve -v -s 4096 direnv exec . cargo run --quiet -- --help
...
shellHook=... export LD_LIBRARY_PATH="/nix/store/...-wayland-1.24.0/lib:/nix/store/...-libxkbcommon-1.13.1/lib:/nix/store/...-libglvnd-1.7.0/lib:/nix/store/...-vulkan-loader-1.4.341.0/lib:..."
```

Inner helper spawn path as seen by the temporary wrappers:

```text
cargo wrapper argv:
run
--quiet
--package
cluster-guard-prompt
--
--vm vm-1 --user account --reason "test login" --timeout-secs 120

helper runner argv:
target/debug/cluster-guard-prompt
--vm vm-1 --user account --reason "test login" --timeout-secs 120
```

Relevant env diff between nested Cargo and helper boundary:

```text
cargo env:
  CARGO=/tmp/.../cargo-wrapper.sh
  LD_LIBRARY_PATH=/nix/store/...-wayland...:/nix/store/...-libxkbcommon...:/nix/store/...-libglvnd...:/nix/store/...-vulkan-loader...:...

helper env:
  CARGO=/nix/store/.../bin/cargo
  LD_LIBRARY_PATH=/data/nvme0/can/Projects/steampipe/target/debug:/data/nvme0/can/Projects/steampipe/target/debug/deps:/nix/store/.../rustlib/...:/nix/store/...-libsecret...:/nix/store/...-wayland...:/nix/store/...-libxkbcommon...:/nix/store/...-libglvnd...:/nix/store/...-vulkan-loader...:...
```

The only changes at the helper boundary were additive. The required Wayland/GL
libraries remained present end-to-end.

### Pinpointed defect

Propagation defect: `LD_LIBRARY_PATH` was lost only in the stale interactive
direnv shell before `cluster-ctl` started; `cluster-ctl`, the nested
`cargo run -p cluster-guard-prompt`, and the helper process all preserved the
GUI runtime library path once the shell was reloaded.

Phase 05 therefore needs to fix the entry layer and the installed-binary case:

- make the dev-shell reload requirement impossible to miss or unnecessary, and
- ship an installed `cluster-ctl`/`cluster-guard-prompt` wrapper that bakes the
  GUI library path, because installed binaries do not have a dev shell at all.
