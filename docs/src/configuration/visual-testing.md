# Visual Testing

`steampipe.toml` can define structured golden-image checks in a top-level
`[visual]` block. Paths are relative to the project root unless absolute.

```toml
[visual]
golden_dir = "tests/visual/golden"   # repo-relative input goldens
diff_dir   = "tests/visual/diff"     # produced output, usually gitignored
default_tolerance = { ssim = 0.985, max_pixel_delta = 8 }
default_backend   = "vnc"            # grim or vnc

[[visual.scene]]
name        = "main-menu"
description = "Steam launches, game shows main menu"
window      = { app_id = "regicide" }
resolution  = { width = 1280, height = 720 }
backend     = "vnc"
tolerance   = { ssim = 0.97, max_pixel_delta = 16 }
mask        = [ { x = 0, y = 0, w = 1280, h = 32 } ]   # ignore titlebar
roi         = { x = 64, y = 64, w = 1152, h = 600 }
wait_for    = { window_visible = true, min_age_ms = 500 }
```

Goldens live at `{golden_dir}/{scene}.png`. Diff artifacts live at
`{diff_dir}/{run_label}/{scene}.{actual,diff,golden}.png`.

## Compositor / backend requirements

Visual testing only runs against **sway**. Both supported backends depend on
wlroots:

- **`grim`** drives the in-VM Wayland socket — works with `--display sway`
  only. Combining it with a `*-gpu` display is rejected (`grim` cannot reach
  the virtio-gpu DRM render path).
- **`vnc`** drives `wayvnc`, which attaches to wlroots — works with
  `--display sway` *or* `--display sway-gpu`.

`--display weston` / `weston-gpu` are experimental and not supported by the
visual harness: there is no Weston readiness probe and `wayvnc` will not
attach. `cluster-ctl` rejects these combinations at startup. See the matrix
in [steampipe.toml](./steampipe-toml.md#supported-display--screenshot-backend-matrix).

### Why sway, and not hyprland

The VM compositor is sway, with weston as a fallback. Hyprland is intentionally
not supported.

- wayvnc is documented as a VNC server for wlroots-based compositors. Hyprland
  forked from wlroots to its own Aquamarine renderer (~0.40+) and is
  partial-compat with wayvnc; cursor sync, multi-output bookkeeping, and resize
  behaviour all diverge.
- The Rust harness consumes sway's IPC (`swaymsg -t get_tree`,
  `swaymsg -t get_outputs`) directly in ~70 call sites, including
  `SwayOutput`/`SwayNode` parsers in `src/game/compositor.rs`. Hyprland's IPC
  (`hyprctl`) emits a different JSON shape; switching would mean re-recording
  every golden fixture and rewriting the typed parsers.
- Hyprland's resident memory footprint is notably larger than sway's, fighting
  the per-VM memory budget the cluster targets.

See [hypervisor-restructure-design.md](../planning/hypervisor-restructure-design.md)
("Compositor — Final Stance") and the research dossier adjacent for the full
evaluation.

Fixture golden regeneration (`scripts/regenerate.sh` →
`cluster-ctl fixture golden regenerate`) brings up the **default** fixture
flavor, which is crosvm + sway; regenerating against any other flavor is
out of scope.

Scene names are filesystem-safe identifiers. They may contain ASCII letters,
digits, `.`, `_`, and `-`; `/`, `..`, NUL, whitespace, and shell metacharacters
are rejected at parse time.

## Validator Precedence

`[profile.<name>]` may set `validator_kind = "shell" | "golden" | "none"`.
If unset, `cluster-ctl` infers the validator kind from the available config.

| Profile `validator_kind` | `visual_validator` present | `[visual]` present | Effective kind |
| --- | --- | --- | --- |
| `shell` | any | any | `shell` |
| `golden` | any | any | `golden` |
| `none` | any | any | `none` |
| unset | yes | no | `shell` |
| unset | no | yes | `golden` |
| unset | yes | yes | `golden` with a deprecation warning |
| unset | no | no | `none` |

Examples:

```toml
[profile.legacy]
visual_validator = "scripts/validate-screenshot.sh"
# Effective kind: shell
```

```toml
[visual]
golden_dir = "tests/visual/golden"

[profile.ui]
visual_validator = "scripts/validate-screenshot.sh"
# Effective kind: golden. The legacy shell string is ignored and warned about.
```

```toml
[visual]
golden_dir = "tests/visual/golden"

[profile.manual]
validator_kind = "none"
# Effective kind: none, even though [visual] exists.
```

## CLI Contract

Phase D wires the CLI surface. Phase E implements capture and comparison.

```sh
cluster-ctl visual list
cluster-ctl visual record vm-1 main-menu
cluster-ctl visual bless main-menu --from run-7
cluster-ctl visual diff main-menu --from run-7
```

`visual list` reads configured scenes and prints `Name | Window | Backend |
Tolerance`. `record`, `bless`, and `diff` currently return a Phase E stub
message.

## Migrating From `visual_validator`

Existing profiles with only `visual_validator = "..."` keep using the shell
validator path. To migrate, add `[visual]`, create one `[[visual.scene]]` per
assertion target, and set `validator_kind = "golden"` explicitly if you want the
profile to document that it has moved off the shell validator.

Keep the old shell string only while comparing old and new behavior. Once a
profile uses `[visual]`, the structured golden validator takes precedence over
`visual_validator`; remove the legacy string to silence the deprecation warning.
