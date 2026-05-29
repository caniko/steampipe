# cluster-guard-prompt

`cluster-guard-prompt` is a small, separate iced helper binary for collecting a
Steam Guard one-time code from a graphical host. It is intentionally isolated in
its own workspace crate and Nix package so the iced, winit, Wayland, X11, and
software-renderer runtime closure does not enter the `cluster-ctl` crate or its
default crane package.

The helper must run as a subprocess. iced/winit owns the OS main thread and can
panic while creating an event loop on headless hosts; this process converts those
conditions into stable exit codes for the caller.

## Protocol

Input is supplied only by argv:

```console
cluster-guard-prompt --vm <name> --user <steam_user> --reason <text> [--invalid-retry] [--timeout-secs N]
```

The Steam Guard code is never accepted as input. On submit, the helper writes
the entered code to stdout as a single line and exits `0`.

Exit codes:

- `0`: submitted; stdout contains exactly one line with the entered code.
- `10`: cancelled or the window was closed; stdout is empty.
- `11`: timeout elapsed; stdout is empty.
- `12`: no usable display or GUI initialization failed; stdout is empty.

`--timeout-secs` defaults to 120 seconds. `--invalid-retry` only changes the
prompt copy so a caller can indicate that a previous code was rejected.

The entered code is zeroized after it is printed. The helper does not log the
code, write it to files, or write it to stderr.

## Runtime Packaging

The packaged helper is wrapped with the GUI runtime libraries that iced/winit
loads dynamically on NixOS: Wayland, libxkbcommon, libGL/libglvnd, Vulkan
loader, fontconfig/freetype, and X11 libraries. The wrapper also appends
`/run/opengl-driver/lib` for host driver libraries.

The dev shell exports the same library path. If the helper sees a usable
Wayland/X11 display socket but those libraries are not discoverable, it exits
`12` with an actionable message instead of panicking during winit event-loop
creation. A missing display also exits `12`. A prompt timeout exits `11`.

Both iced renderers remain enabled (`wgpu` and `tiny-skia`). The observed
exit-101 failure was a missing Wayland client-library environment, not a GPU or
renderer failure, so the helper keeps iced's normal `wgpu` to `tiny-skia`
fallback behavior.
