# Phase 01 — Ground-truth capture & classification

> **Recommended Codex model: GPT 5.5 high**
>
> This is the lynchpin: a masked failure (exit 101, no stderr) that gates three
> downstream angles. The work is procedurally simple but the *interpretation* is
> not — distinguishing "stale binary" from "env-not-propagated" from "wgpu panic"
> from "swallowed stderr" requires careful, skeptical reading of which binary ran
> and which env it had, and a wrong call sends the whole set down a dead end. Its
> gating role and the cost of mis-classification put it at `high`, not `medium`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. No prerequisite phases. **Read/run only** —
any instrumentation (env vars, extra prints, forced backends) is temporary and
reverted before finishing; record evidence in this file's `## Findings` section.

## Goal

Produce a definitive, evidence-backed classification of *why* the helper exits
101 and *why no panic text appears*, by capturing: (a) exactly which
`cluster-guard-prompt` binary executes and how it was launched, (b) the real
panic/backtrace with `RUST_BACKTRACE=full` and the silencing hook bypassed, and
(c) a side-by-side env snapshot of the failing interactive `cargo run` context vs
the working `nix develop -c` context. End state: the surviving hypotheses (from
the README's H1–H5) are narrowed to the actual cause(s); if it's H1/H2 (stale
binary / unreloaded env), the trivial fix is applied and the rest of the plan is
marked unnecessary.

## Why this matters now

The README's central clue: the helper opens a window under
`nix develop -c … --timeout-secs 2` (exit 11) but exits 101 via `cargo run -- steam login vm-1`.
The observed `cargo run` output shows `Finished in 0.58s` (no `Compiling`
line — `cluster-ctl` was not rebuilt) and **no** `cluster-guard-prompt:` stderr,
despite the panic hook at
[crates/cluster-guard-prompt/src/main.rs](../../../../crates/cluster-guard-prompt/src/main.rs)
and `Stdio::inherit()` on the child's stderr in
[src/game/steam.rs](../../../../src/game/steam.rs) (`request_guard_code_from_helper`).
That silence is the loudest clue — it strongly implies either a stale helper
binary or a `cluster-ctl` that predates the cargo-run-prefer change. Confirm
before anyone investigates GPU drivers.

## Out of scope

- Designing or landing the durable fix — Phase 05 (except a trivial H1/H2
  binary-rebuild / `direnv reload` confirmation, which you may apply and record).
- Deep GPU/Vulkan analysis — Phase 02. Deep env-matrix analysis — Phase 03.
  iced-internals/repro — Phase 04. This phase only captures the ground truth that
  focuses them.

## Plan

1. **Pin the source state.** `git -C . rev-parse HEAD`, `git status --porcelain`,
   and confirm `src/game/steam.rs` contains `guard_prompt_command` with the
   cargo-run-prefer branch and that `crates/cluster-guard-prompt/Cargo.toml` lists
   both `wgpu` and `tiny-skia`. Record whether the working tree matches the
   intended code.
2. **Determine which `cluster-ctl` runs and whether it has the fix.** Rebuild
   explicitly: `cargo build` (workspace) and note the `cluster-ctl` mtime
   (`stat -c '%y' target/debug/cluster-ctl`). Compare to the binary `cargo run`
   would use. Confirm whether the *previously observed* run used a `cluster-ctl`
   built before the cargo-run-prefer change (H1).
3. **Determine which helper path is taken.** Temporarily add a one-line
   `eprintln!` in `guard_prompt_command` reporting which branch is chosen
   (override / cargo-run / sibling) and the resolved program + args — or run under
   `strace -f -e trace=execve` to capture the actual `execve` of the helper /
   `cargo`. Establish definitively: sibling binary vs `cargo run -p`. Revert the
   `eprintln!` after.
4. **Capture the real failure of the helper directly**, in the *interactive*
   shell (not `nix develop -c`), with diagnostics forced on:
   ```sh
   echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY DISPLAY=$DISPLAY XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"
   echo "LD_LIBRARY_PATH=$LD_LIBRARY_PATH"
   RUST_BACKTRACE=full target/debug/cluster-guard-prompt \
     --vm vm-1 --user diag --reason "capture" --timeout-secs 3; echo "exit=$?"
   ```
   Then rebuild the helper fresh (`cargo build -p cluster-guard-prompt`) and repeat
   to rule out staleness. Record the exact stderr/backtrace (it should now name
   the failing call — winit `NoWaylandLib`, a wgpu/Vulkan error, a tiny-skia/
   softbuffer error, etc.).
5. **Side-by-side env diff.** Capture `env | sort` (or at least
   `LD_LIBRARY_PATH`, `WAYLAND_DISPLAY`, `DISPLAY`, `XDG_RUNTIME_DIR`,
   `VK_ICD_FILENAMES`, `VK_DRIVER_FILES`, `__EGL_VENDOR_LIBRARY`,
   `LIBGL_DRIVERS_PATH`, `NIXOS_OZONE_WL`, `DIRENV_*`) in BOTH:
   - the interactive shell where `cargo run` fails, and
   - `nix develop -c bash -lc 'env | sort'` (the working context).
   Diff them. The delta is the prime suspect. Note especially whether
   `LD_LIBRARY_PATH` contains a wayland path in the interactive shell, and whether
   `direnv status` shows the current dev shell loaded.
6. **Confirm/refute H4 (swallowed stderr).** From step 3/4, determine whether the
   panic text is produced but lost vs never produced. If a stale silent binary was
   the cause, H4 is moot.
7. **Classify and (if trivial) fix.** Write a `## Findings` section stating the
   confirmed cause(s) and the status of H1–H5. If the cause is H1 (stale binary)
   or H2 (unreloaded env), apply the trivial remedy (`cargo build` + `direnv
   reload`), re-run `cargo run -- steam login vm-1` to the guard step, and record
   whether the dialog now appears. If so, mark phases 02–04 **unnecessary** in the
   findings and hand off to Phase 05 only for the durable/installed-binary fix.

## Acceptance criteria

- [ ] `## Findings` records: the `git` HEAD + dirty state; which `cluster-ctl`
      binary ran and whether it contained `guard_prompt_command`'s cargo-run
      branch; and which helper-launch path was taken (sibling vs `cargo run -p`),
      proven by `eprintln`/`strace` evidence.
- [ ] The **real** helper failure is captured with `RUST_BACKTRACE=full` and a
      non-silenced panic: the exact failing call/error string is quoted (not just
      "exit 101").
- [ ] A side-by-side env diff (interactive `cargo run` shell vs `nix develop -c`)
      is recorded, with the load-bearing delta(s) called out (esp.
      `LD_LIBRARY_PATH` wayland entry + `direnv` load state).
- [ ] Each of H1–H5 is marked confirmed / refuted / still-open with the evidence.
- [ ] If H1/H2: the trivial fix is applied and the result (dialog appears or not)
      recorded; phases 02–04 marked unnecessary iff the dialog now appears.
- [ ] All temporary instrumentation is reverted (`git diff` clean except this
      doc's `## Findings`).

## Files likely touched

- None durably. Temporary, reverted: a one-line `eprintln!` in
  `src/game/steam.rs` `guard_prompt_command` (step 3).
- This phase file's `## Findings` section (the deliverable).

## Pitfalls

- **Testing in the wrong shell.** `nix develop -c` and an interactive direnv shell
  can differ in `LD_LIBRARY_PATH`/GPU env. Always capture in the *exact* shell
  where `cargo run -- steam login` fails. *Recovery:* run the capture commands in
  that terminal, not a fresh `nix develop`.
- **Stale binary masquerading as a code bug.** If `cargo run` shows no `Compiling`
  line, the binary is whatever was last built — possibly pre-fix. *Recovery:*
  force `cargo build` and compare mtimes before drawing conclusions.
- **The silencing hook hiding the cause.** Older helper builds installed a no-op
  panic hook (silent exit 101). Ensure you run a freshly-built helper whose hook
  prints, or temporarily bypass the hook. *Recovery:* `cargo build -p
  cluster-guard-prompt` then re-run.
- **`cargo run -p` nested under `cargo run`.** If the spawn path is taken, cargo
  may emit build output to stderr and the helper's panic after it — don't mistake
  cargo noise for the helper's error. *Recovery:* use `--quiet` and isolate the
  helper run directly (step 4).
- **Don't fix GPU drivers yet.** If the cause is H1/H2, GPU analysis is wasted.
  Resolve the cheap hypotheses first.

## Reference

- README hypotheses H1–H5 and the central `nix develop -c` vs `cargo run` clue:
  [README.md](./README.md).
- Spawn logic: `guard_prompt_command` / `request_guard_code_from_helper` in
  [src/game/steam.rs](../../../../src/game/steam.rs).
- Helper panic hook + `has_display`:
  [crates/cluster-guard-prompt/src/main.rs](../../../../crates/cluster-guard-prompt/src/main.rs).
- Gates phases 02 (GPU), 03 (env), 04 (iced) and feeds 05 (fix).

## Findings

Captured on 2026-05-29 from `/data/nvme0/can/Projects/steampipe`.

### Source and binary state

- HEAD: `24d6a0adb970222e96aabf346144e984c1831783`.
- Dirty state before this phase was already broad. `git status --porcelain=v1`
  included modified top-level manifests, `src/game/steam.rs`, Nix files, and new
  `crates/cluster-guard-prompt/*` plus this planning directory. No durable source
  files were changed by this phase except this findings section.
- `src/game/steam.rs` contains the intended `guard_prompt_command` development
  branch: when `current_exe` is under a Cargo `target/<profile>/` tree, it builds
  `cargo run --quiet --package cluster-guard-prompt -- ...` before falling back
  to a sibling/PATH helper.
- `crates/cluster-guard-prompt/Cargo.toml` enables both iced renderers:
  `wgpu` and `tiny-skia`, plus `x11`, `wayland`, and `tokio`.
- Before explicit rebuild, `target/debug/cluster-ctl` existed with mtime
  `2026-05-29 13:40:39.285132937 +0200`; `strings -a target/debug/cluster-ctl`
  already showed `--package`, `--quiet`, `cluster-guard-prompt`,
  `STEAMPIPE_GUARD_PROMPT_BIN`, and
  `cargo_workspace_root_from_current_exe`, so the currently present binary
  already contained the cargo-run-prefer code.
- `cargo build` rebuilt `cluster-ctl` and finished successfully. Afterward:
  `target/debug/cluster-ctl 2026-05-29 15:06:20.171365043 +0200 65614088`.
- The helper was rebuilt after temporary backtrace instrumentation was reverted:
  `target/debug/cluster-guard-prompt 2026-05-29 15:08:27.840370808 +0200
  98129360`.
- `cargo run -vv -- --help` proved the `cargo run` executable path:
  Cargo ran `target/debug/cluster-ctl --help`.

### Helper launch path

The development helper command is the cargo-run path, not a stale sibling path,
for Cargo-built binaries under this workspace:

```text
strace -f -e trace=execve cargo run --quiet --package cluster-guard-prompt -- --vm vm-1 --user diag --reason strace --timeout-secs 1
execve("/nix/store/.../bin/cargo", ["cargo", "run", "--quiet", "--package", "cluster-guard-prompt", "--", "--vm", "vm-1", ...]) = 0
[pid ...] execve("target/debug/cluster-guard-prompt", ["target/debug/cluster-guard-prompt", "--vm", "vm-1", ...]) <unfinished ...>
```

The full `cargo run -- steam login vm-1` path was not driven to Steam Guard in
this Codex process because that requires the real VM/login/account state. The
code path that `cluster-ctl` will use from `target/debug/cluster-ctl` is still
mechanically pinned: `cargo run -vv` runs `target/debug/cluster-ctl`, that binary
contains `cargo_workspace_root_from_current_exe`, and the branch constructs the
same `cargo run --quiet --package cluster-guard-prompt --` command shown above.

### Real helper failure

This Codex process initially did not inherit a display:

```text
WAYLAND_DISPLAY= DISPLAY= XDG_RUNTIME_DIR=
LD_LIBRARY_PATH=
cluster-guard-prompt: no usable host display (WAYLAND_DISPLAY/DISPLAY unset or socket missing); cannot show the Steam Guard dialog.
exit=12
```

The host did have a real Wayland socket at `/run/user/1000/wayland-1`. Re-running
with that display but with `LD_LIBRARY_PATH` intentionally absent reproduced the
exit 101:

```text
env -u LD_LIBRARY_PATH XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 RUST_BACKTRACE=full target/debug/cluster-guard-prompt --vm vm-1 --user diag --reason capture --timeout-secs 3
cluster-guard-prompt: GUI backend panic: panicked at .../iced_winit-0.14.0/src/lib.rs:81:10:
Create event loop: Os(OsError { line: 89, file: ".../winit-0.30.13/src/platform_impl/linux/wayland/event_loop/mod.rs", error: WaylandError(Connection(NoWaylandLib)) })
exit=101
```

Temporary instrumentation added `std::backtrace::Backtrace::force_capture()` to
the helper panic hook, was rebuilt, captured, then reverted and rebuilt again.
The load-bearing frames were:

```text
0: cluster_guard_prompt::main::{closure#0}
   at ./crates/cluster-guard-prompt/src/main.rs:90:13
8: Result<...>::expect
9: iced_winit::run
   at .../iced_winit-0.14.0/src/lib.rs:79:22
11: cluster_guard_prompt::main::{closure#1}
   at ./crates/cluster-guard-prompt/src/main.rs:95:9
15: cluster_guard_prompt::main
   at ./crates/cluster-guard-prompt/src/main.rs:94:18
```

The exact failing call is iced/winit event-loop creation:
`Create event loop: Os(... WaylandError(Connection(NoWaylandLib)))`.

### Environment comparison

Display-bearing failing context, reconstructed from the existing socket:

```text
WAYLAND_DISPLAY=wayland-1
XDG_RUNTIME_DIR=/run/user/1000
LD_LIBRARY_PATH unset
```

Working `nix develop -c` context with the same display:

```text
WAYLAND_DISPLAY=wayland-1
XDG_RUNTIME_DIR=/run/user/1000
LD_LIBRARY_PATH=/nix/store/...-wayland-1.24.0/lib:/nix/store/...-libxkbcommon-1.13.1/lib:/nix/store/...-libglvnd-1.7.0/lib:/nix/store/...-vulkan-loader-1.4.341.0/lib:/nix/store/...-fontconfig-2.17.1-lib/lib:/nix/store/...-freetype-2.14.2/lib:/nix/store/...-libx11-1.8.13/lib:/nix/store/...-libxcursor-1.2.3/lib:/nix/store/...-libxi-1.8.2/lib:/nix/store/...-libxrandr-1.5.5/lib:/nix/store/...-libxcb-1.17.0/lib
```

`nix develop -c bash -lc 'XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 target/debug/cluster-guard-prompt ... --timeout-secs 3'`
opened the dialog and timed out cleanly: `exit=11`.

Before reload, `direnv exec .` used a stale cached dev shell that did **not**
export `LD_LIBRARY_PATH`:

```text
direnv: nix-direnv: Using cached dev shell
LD_LIBRARY_PATH=
cluster-guard-prompt: ... WaylandError(Connection(NoWaylandLib))
exit=101
```

After the trivial remedy:

```text
direnv reload
direnv: nix-direnv: cache invalidated: files newer than cache:
nix-direnv: /data/nvme0/can/Projects/steampipe/.envrc
direnv: nix-direnv: Renewed cache
```

`direnv exec .` exported the same GUI library path as `nix develop -c`, and both
direct helper and nested Cargo helper opened successfully:

```text
env XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 direnv exec . target/debug/cluster-guard-prompt ... --timeout-secs 3
LD_LIBRARY_PATH=/nix/store/...-wayland-1.24.0/lib:...
exit=11

env XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 direnv exec . cargo run --quiet --package cluster-guard-prompt -- ... --timeout-secs 3
exit=11
```

### Classification

- **H1 — stale binary:** refuted for the current `target/debug/cluster-ctl`.
  The pre-rebuild binary already contained the cargo-run-prefer branch, and an
  explicit workspace rebuild was applied. Historical silence may have come from
  an older artifact, but the present exit 101 does not require H1.
- **H2 — env not propagated / stale direnv cache:** confirmed. Missing
  `LD_LIBRARY_PATH` in the direnv/cargo context causes winit
  `NoWaylandLib`; `nix develop -c` works because it provides the required
  Wayland/libxkbcommon/libGL/Vulkan/X11 library path. `direnv reload` renewed
  the cache and made `direnv exec .` work.
- **H3 — wgpu init panic / Vulkan ICD:** refuted for this failure. The panic
  occurs before renderer/GPU initialization, while creating the winit Wayland
  event loop because `libwayland` is not discoverable.
- **H4 — stderr swallowed:** refuted for current binaries. Direct helper and
  `cargo run --quiet --package cluster-guard-prompt -- ...` both print
  `cluster-guard-prompt: GUI backend panic: ... NoWaylandLib` to stderr. The
  current silence is not caused by `Stdio::inherit()` or Cargo swallowing stderr.
- **H5 — real COSMIC/session tiny-skia difference:** refuted for this phase's
  reproduced failure. With the same Wayland socket and the dev-shell
  `LD_LIBRARY_PATH`, the helper opens and times out cleanly (`exit=11`).

### Result

Trivial H2 remedy applied: `cargo build`, `cargo build -p cluster-guard-prompt`,
and `direnv reload`. The helper dialog now appears from the renewed direnv/dev
environment (`exit=11` on timeout), so phases 02, 03, and 04 are unnecessary for
the observed exit-101 root cause. Phase 05 should still make the runtime
environment durable for installed/non-direnv launches so the helper never depends
on a stale interactive shell cache.
