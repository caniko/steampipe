# Phase 03 — `cluster-guard-prompt` iced helper binary + Nix packaging

> **Status: shipped (Rust + Nix packaging, 2026-05-29).** The
> `crates/cluster-guard-prompt` workspace crate exists and builds: an iced 0.14
> modal (`wgpu` + `tiny-skia` auto renderer, `secure(true)` input,
> Cancel/Submit, timeout) that prints the code to stdout / exits 10 cancel, 11
> timeout, 12 no-display/runtime-unavailable. The helper pre-checks display
> sockets and NixOS GUI runtime libraries before iced so missing Wayland/X11 libs
> fail cleanly instead of surfacing as exit 101. `cluster-ctl` links no iced; the
> installed wrapper puts `cluster-guard-prompt` on `PATH` and the helper wrapper
> bakes the GUI runtime library path.

> **Recommended Codex model: GPT 5.5 high**
>
> Complex, self-contained deliverable with two hard subtleties the dossier flags
> as panic-class: iced 0.14 must own the OS main thread and *panics* (not
> returns `Err`) when no display exists, and adding the winit/wgpu/tiny-skia
> closure must not leak into `cluster-ctl`'s zero-`buildInputs` crane build. A
> standalone binary against a fixed protocol (sub-agent role), but mediocre work
> ships either a hard GPU dependency or a binary that crashes instead of exiting
> cleanly headless. `5.5 medium` would likely suffice for the iced code alone;
> the Nix-closure isolation and the headless-exit contract push it to `high`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **No phase dependency** — this binary is
standalone and built against the argv/stdout protocol defined below, so it runs in
parallel with Phases 01/02 (Wave 0). It does **not** touch `src/game/steam.rs` or
any login code; integration is Phase 04.

## Goal

A new binary `cluster-guard-prompt` renders a small pinentry-style modal that
prompts for a Steam Guard one-time code and prints the entered code to stdout,
exiting 0. On cancel it exits non-zero with empty stdout; on no usable display it
exits non-zero **cleanly** (never crashes the parent). It owns iced/winit on its
own OS main thread and uses the tiny-skia software renderer (no hard GPU
dependency). The iced/winit/Wayland/Vulkan dependency closure is confined to this
binary's Nix output; `cluster-ctl` remains GUI-free.

## Why this matters now

The brief wants a host-side iced dialog, and the dossier's recommended Option A
isolates iced in a separate process precisely because of two hard external
constraints:

- **Main-thread + blocking:** iced 0.14 runs on winit; `iced::run()` must be
  called on the OS main thread and blocks until the window closes. `cluster-ctl`
  builds its multi-thread tokio runtime by hand and `block_on`s on main
  (`src/lib.rs:543-548`) — a separate process keeps these from colliding.
- **Headless = panic, not Result:** with no `DISPLAY`/`WAYLAND_DISPLAY` or no
  client lib, iced panics during event-loop creation. A separate process turns
  that panic into an observable non-zero exit the parent can branch on; in-process
  it would need fragile `catch_unwind`.

Building the helper now (in parallel with the core refactor) de-risks the
integration in Phase 04 and keeps the heavy closure out of every headless
consumer (`src/mcp.rs`, the `warm` timer).

## Out of scope

- Wiring the helper into the login flow / `HelperGui` provider — Phase 04.
- Any change to `cluster-ctl`'s sources or its crane build inputs.
- TOTP generation, secret storage, or talking to steamcmd — the helper only
  renders a prompt and returns a string.

## Plan

1. **Decide crate layout.** Add `cluster-guard-prompt` as a second binary. Given
   the repo is currently a single-package crate (`Cargo.toml` has `[package]
   name = "steampipe"`, one `[[bin]]`), prefer a **separate workspace member
   crate** under `crates/cluster-guard-prompt/` (convert root to a workspace, or
   add a `[workspace] members=[...]`) so the iced dependency does **not** appear
   in `cluster-ctl`'s dependency graph at all. If a workspace conversion is too
   invasive, fall back to a second `[[bin]]` gated behind a `gui` feature that is
   off by default — but the separate-crate option is strongly preferred for clean
   closure isolation. Document the choice in the crate README.
2. **Add iced 0.14** with the tiny-skia renderer feature (disable the default
   wgpu renderer to avoid a hard GPU dependency). Pin to `0.14.x`; the repo is
   on edition 2024 / rust 1.88.
3. **Define the argv/stdout protocol** (pinentry-style, caller pushes context):
   - **Input (argv/env):** `--vm <name> --user <steam_user> --reason <text>
     [--invalid-retry] [--timeout-secs N]`. Do **not** accept the code on input.
   - **Output:** on submit, print the entered code to stdout (single line) and
     exit `0`. On cancel/window-close, print nothing and exit `10`. On
     timeout, exit `11`. On no-usable-display/init failure, exit `12`.
   - These exit codes are the contract Phase 04's `HelperGui` provider branches on.
     Document them in the crate README and reference them from Phase 04.
4. **Pre-check the display before constructing the event loop.** At `main()`
   start, check `$WAYLAND_DISPLAY`/`$DISPLAY`; if neither is usable, print nothing
   and `std::process::exit(12)` **before** any iced call (turns the would-be panic
   into a clean exit). Additionally wrap `iced::run(...)` in
   `std::panic::catch_unwind` as a belt-and-suspenders guard that maps a panic to
   exit `12`.
5. **Build the modal** with the iced 0.14 functional API (`iced::run`/
   `iced::application`): a `text_input` for the code (mask or not — Guard codes are
   low-sensitivity but treat as secret: do not echo to logs), an `on_submit` and a
   submit `button`, a Cancel button, and a label showing vm/user/reason. State
   holds the typed string; submit stores it and requests window close; on close,
   `main` prints the stored code and exits 0.
6. **Enforce the timeout.** Honor `--timeout-secs` (default e.g. 120) so an
   unattended prompt can't hang a parent forever; on expiry, exit `11`.
7. **Secret hygiene.** Zeroize the entered string buffer after printing; never log
   it; do not write it to a file or to stderr.
8. **Nix packaging.** In `flake.nix` / `nix/flake/packages.nix`, add a
   `cluster-guard-prompt` package built with crane, declaring the GUI
   `buildInputs` **only on this output**: `libxkbcommon`, `wayland`,
   `wayland-protocols`, `libGL`, `vulkan-loader` (loader only; tiny-skia avoids a
   GPU requirement at build), `fontconfig`, `freetype`, plus `makeWrapper` to set
   `LD_LIBRARY_PATH`/`XDG_*` as needed for runtime lib discovery. Confirm
   `cluster-ctl`'s existing package (`packages.nix:67-84`) is **unchanged** and
   still builds with zero `buildInputs`.
9. **Dev shell.** Add the GUI `pkgConfigDeps`/libs to `nix/flake/dev-shells.nix`
   so `cargo run -p cluster-guard-prompt` works in the dev shell, without forcing
   them on the `cluster-ctl`-only build.
10. Verify: `nix build .#cluster-guard-prompt` succeeds; `nix build .#default`
    (cluster-ctl) closure contains no iced/winit/wgpu; run the helper manually on
    a graphical host and confirm it returns the typed code on stdout and the right
    exit codes for cancel/timeout/headless.

## Acceptance criteria

- [ ] `cluster-guard-prompt` builds as its own crate/output; `cargo tree -p
      steampipe` (the `cluster-ctl` crate) shows **no** `iced`/`winit`/`wgpu`
      dependency.
- [ ] Running the helper on a graphical host prints the entered code to stdout and
      exits 0; Cancel exits 10; timeout exits 11.
- [ ] Running with `DISPLAY=`/`WAYLAND_DISPLAY=` unset (or invalid) exits 12
      **without a panic backtrace on stderr** and prints nothing to stdout.
- [ ] Uses the tiny-skia renderer (no wgpu feature enabled); `nix build
      .#cluster-guard-prompt` succeeds and the closure does not require a GPU at
      build time.
- [ ] `nix build .#default` (cluster-ctl) closure is unchanged w.r.t. GUI libs
      (no new `buildInputs`); diff the derivation inputs to confirm.
- [ ] The argv/stdout/exit-code protocol is documented in the crate README and
      matches what Phase 04 expects.
- [ ] The entered code is zeroized after printing and never logged.

## Files likely touched

- `Cargo.toml` (root) — workspace members, or `[[bin]]` + `gui` feature.
- `crates/cluster-guard-prompt/Cargo.toml`, `.../src/main.rs` *(new)* — the helper.
- `crates/cluster-guard-prompt/README.md` *(new)* — protocol + exit codes.
- `flake.nix`, `nix/flake/packages.nix` — new `cluster-guard-prompt` package with
  GUI `buildInputs` scoped to it.
- `nix/flake/dev-shells.nix` — GUI libs for the dev shell.
- `Cargo.lock` — regenerated.

## Pitfalls

- **iced default renderer is wgpu (GPU).** Forgetting to disable the wgpu feature
  and enable tiny-skia pulls a Vulkan/GL hard requirement and can crash on some
  Wayland+Vulkan combos. *Recovery:* set `default-features = false` and enable the
  `tiny-skia` feature explicitly; verify the closure.
- **Closure leak into cluster-ctl.** A second `[[bin]]` in the *same* crate makes
  iced a dependency of the whole package unless feature-gated *and* the feature is
  off in the cluster-ctl build — easy to get wrong. *Recovery:* the separate
  workspace crate avoids this entirely; prefer it.
- **Event loop off the main thread.** If `main` spawns a runtime or thread and
  calls `iced::run` from it, winit panics ("Initializing the event loop outside of
  the main thread"). *Recovery:* the helper has no tokio runtime; call `iced::run`
  directly from `main`.
- **Panic backtrace leaking to the parent.** A raw panic prints a backtrace to
  stderr that Phase 04 might surface. *Recovery:* the early env pre-check + the
  `catch_unwind` mapping to exit 12 keep stderr clean.
- **edition/MSRV mismatch.** iced 0.14 MSRV is 1.88; keep the helper crate's
  `rust-version` aligned with the root crate's `rust-version = "1.88"`.

## Reference

- Dossier: "iced imposes hard, non-negotiable constraints" + "pinentry is a
  validated blueprint" + incompatibility #13 (headless closure) + Option A.
- iced 0.14 functional API (`iced::run`/`text_input`/`button`); tiny-skia
  renderer feature.
- cluster-ctl crane build to keep GUI-free: `nix/flake/packages.nix:67-84`.
- Consumed by Phase 04 (the `HelperGui` provider spawns this binary).

## Execution Notes

2026-05-29:

- Shipped `crates/cluster-guard-prompt` (291-line `main.rs`): iced 0.14 functional
  API, `tiny-skia` + `x11` + `wayland` features (no wgpu), `secure(true)` code
  input, Cancel/Submit, `--timeout-secs` (default 120), and a `has_display()`
  pre-check + `catch_unwind` mapping no-display/init failure to exit 12.
- Verified: `nix`-free `cargo build -p cluster-guard-prompt` succeeds;
  `cargo tree` shows iced only under the helper crate, not `cluster-ctl`; the
  headless contract holds (`env -u DISPLAY -u WAYLAND_DISPLAY cluster-guard-prompt
  … → exit 12`, empty stdout, no panic backtrace on stderr); `--help` matches the
  argv protocol Phase 04 passes.
- **Dev-shell runtime libs: done.** The dialog initially failed under `cargo run`
  with `WaylandError(Connection(NoWaylandLib))` — winit `dlopen`s
  `libwayland-client.so` at runtime and it was not on the loader path. Fixed in
  `nix/flake/dev-shells.nix` by exporting `LD_LIBRARY_PATH` over
  wayland/libxkbcommon/libGL/vulkan-loader/fontconfig/freetype + libx11/libxcursor/
  libxi/libxrandr/libxcb. Verified: in the dev shell the helper opens the window
  (`--timeout-secs 2 → exit 11`, no panic). The helper's panic hook now reports the
  reason to stderr instead of a silent exit-101.
- **Renderer: wgpu + tiny-skia auto-discovery.** The crate enables both iced
  renderer features (`wgpu`, `tiny-skia`): iced uses the GPU (Vulkan/GL) when
  available and falls back to the software renderer otherwise — no hard GPU
  dependency, no manual backend selection. Verified the window opens via this
  build (`exit 11`).
- **Dev spawn prefers `cargo run`.** `cluster-ctl` running from a cargo
  `target/<profile>/` tree launches the helper via `cargo run -p
  cluster-guard-prompt` rather than a sibling binary, because `cargo run -- steam
  login` only rebuilds `cluster-ctl` and a sibling helper would be **stale**.
  (Installed binaries still use the sibling/`$PATH`.)
- **Installed packaging done.** `nix/flake/packages.nix` now builds
  `cluster-guard-prompt` with a `makeWrapper` that prefixes `LD_LIBRARY_PATH`
  with the same Wayland/libxkbcommon/libGL/vulkan-loader/fontconfig/freetype/X11
  libraries as the dev shell, appends `/run/opengl-driver/lib`, and sets default
  GL/EGL/XDG runtime paths. The `cluster-ctl` wrapper prefixes `PATH` with the
  helper package so installed `cluster-ctl` can resolve the dialog helper without
  relying on a dev shell or sibling binary.
- **Runtime guard hardened.** On NixOS, if a display socket exists but the
  expected Wayland/X11 client libraries are absent from `LD_LIBRARY_PATH`, the
  helper exits 12 with an actionable message before calling iced/winit. Verified
  the former exit-101 repro (`LD_LIBRARY_PATH` unset with a valid Wayland socket)
  now exits 12, while the wrapped installed helper opens and times out cleanly
  with caller `LD_LIBRARY_PATH` unset.
