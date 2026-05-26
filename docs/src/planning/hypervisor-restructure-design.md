# Hypervisor + Compositor Restructure — Final Design

Companion to
[hypervisor-restructure-research.md](./hypervisor-restructure-research.md).
The research dossier surfaced evidence and four open decisions; this
document locks every decision and pins concrete code shapes so a
multi-phase planner can fan out without making any further design
calls.

If you are about to execute work from this document, the next step is
to invoke `multi-phase-plan-codex` (or sister flavour) and have it
produce phase files. **This document deliberately stops short of
phasing.**

## Pivot 2026-05-26 — host-module classes use QEMU/CHV without virtio-gpu

Phase 01 of the plan (atlas QEMU virgl probe) produced a **RED**
verdict (see
[hypervisor-restructure/01-validate-qemu-virgl-on-atlas-results.md](./hypervisor-restructure/01-validate-qemu-virgl-on-atlas-results.md)).
The original "QEMU + `-device virtio-vga-gl` + virgl/Venus" shape
is empirically unviable on atlas (Mesa 26.1.1 + AMD Navi 31).
Specifically: QEMU rejects `-display none` + GL devices before
guest boot; the Venus fallback segfaults QEMU under render load.

The design pivots:

- **Host-module classes (`loginRunners`, `runners`) don't need GPU
  rendering**. The login wizard is CEF/GTK (software-renderable
  on llvmpipe). The warm flow uses `DisplayMode::Headless` at
  [src/game/steam.rs:490](../../../src/game/steam.rs#L490) (no
  compositor). sway already runs with `WLR_BACKENDS=headless`
  ([src/game/steam.rs:56-57](../../../src/game/steam.rs#L56-L57)).
  wayvnc captures via `wlr-screencopy` on the headless output.
- **`microvm.graphics.enable = false` for host-module classes**.
  No `-device virtio-vga-gl`. No `-object memory-backend-memfd`.
  No Venus. No virgl. The graphics-related sections below are
  superseded for host-module classes; they remain accurate for
  the project-flake crosvm escape hatch.
- **Project-flake graphical workloads stay on crosvm** with the
  patched overlay. The crosvm block in `nix/flake/overlay.nix`
  is **not** deleted in Phase 10 — only the
  `cloud-hypervisor-graphics` override and the
  `__steampipeOverlay` marker go. Project owners pinning
  `[cluster] hypervisor = "crosvm"` keep the working build.
  Re-evaluation horizon: 2026-Q4 nixpkgs (post QEMU 11.x / Mesa 27.x).
- **`nix/vm-graphics.nix`** gains a `softwareRendering` mode for
  host-module classes that publishes Mesa libs (for Wayland
  client dlopen) without claiming Vulkan. The
  `microvm.graphics.enable = true` path stays unchanged for the
  crosvm escape hatch.
- **A new docs page** `docs/src/configuration/graphics-and-games.md`
  records the project-flake graphics situation publicly.

The rest of this document reads with that pivot in mind. Wherever
the original text says "QEMU graphical login VMs" with virgl
shape, the actual shipped shape is "QEMU host-module VMs with no
graphics device, running sway-headless + llvmpipe". The
empirically-failed virgl/Venus shape lives on as historical
record under "`nix/lib/microvm-runner.nix` Changes" below; its
guidance is **superseded** by the pivot.

## Decisions Resolved

| # | Decision | Choice | Rationale |
|---|---|---|---|
| D1 | Drop hyprland from scope? | **Yes** | wayvnc is wlroots-only by upstream description; hyprland-on-Aquamarine is partial-compat. 70 swaymsg callsites + typed parsers would need rewriting. Hyprland's RAM footprint is larger than sway's. All three reasons fight the user's stated constraints; the conditional in "consider hyprland *if* VNC is reliable" fails. |
| D2 | Steam state persistence path | **Path A (per-VM home image, shrunk for login runners)** | Path B (real virtiofs/9p share) needs either an out-of-band virtiofsd wrapper around `microvm-run` or migration to systemd-microvm-host integration. Neither is in this scope. The home-image path is already what's live (proven by "vm-1 is already logged in" survival). Reduce login-runner image to 2 GiB; runtime stays at 8 GiB. |
| D3 | Downstream `[cluster] hypervisor = "crosvm"` pin policy | **Honour, but emit a deprecation warning** | Hard error would break consumers (chessbender, regicide, fragpipe) on a flake-input bump. Silent upgrade hides a real behaviour change. The deprecation warning includes the suggested replacement string and the cut-off release. Cut-off planned for 0.4. |
| D4 | Atlas rebuild order vs restructure | **Rebuild atlas first** | Closes the residual SSH-fix P1 (P1 status: partial). Without this, any `cluster-ctl steam login` test inside the restructure waves still fails at the SSH stage and doesn't actually exercise the new hypervisor paths. One nixos-rebuild from canix, no code change. |

## Final Hypervisor Matrix

| VM class | Hypervisor | Why |
|---|---|---|
| Login runners (`services.steampipe-cluster.loginRunners`, project-flake graphical flavours) | **QEMU** | Battle-tested virgl + Venus integration; KVM acceleration; first-class virtio-balloon + virtio-mem; `microvm.qemu.extraArgs` lets us run headless on atlas. |
| Runtime runners (`services.steampipe-cluster.runners`, project-flake headless flavours) | **cloud-hypervisor** (plain upstream, not `-graphics`) | LF-owned Rust hypervisor; lean; no virgl needed for headless; built into nixpkgs without overlay; `virtio-balloon` + `deflate_on_oom` supported. |
| Manual operator override | Any microvm.nix-supported runner | Honour `hypervisor = "crosvm"` / `"firecracker"` / etc. as a power-user escape hatch; emit deprecation warning for `"crosvm"`. |

Default selection logic, exposed in three places:

1. `services.steampipe-cluster.loginRunners.hypervisor` — `lib.mkOption` `default = "qemu"`.
2. `services.steampipe-cluster.runners.hypervisor` — `lib.mkOption` `default = "cloud-hypervisor"`.
3. `mkTestCluster` (`nix/lib/test-cluster.nix`) — per-flavour default derives from `graphics`:

   ```nix
   hypervisor = f.hypervisor or (
     if (f.graphics or vmGraphics)
     then "qemu"
     else "cloud-hypervisor"
   );
   ```

   Same fallback applies to `vmHypervisor` from the top-level `[cluster]`.

## Compositor — Final Stance

**Keep sway. Drop hyprland as a non-goal.**

- `DisplayMode` enum at
  [src/ui/cli.rs:65-78](../../../src/ui/cli.rs#L65-L78) keeps its
  current five variants: `Headless`, `Sway`, `Weston`, `WestonGpu`,
  `SwayGpu`. No `Hyprland` / `HyprlandGpu` variant.
- `SwayOutput` / `SwayNode` parsers in
  [src/game/compositor.rs:430-501](../../../src/game/compositor.rs#L430-L501)
  stay as-is. No abstraction layer over compositor IPC.
- Add a one-paragraph note in
  `docs/src/configuration/visual-testing.md` explaining why hyprland
  is intentionally not supported. Stops the question from
  re-litigating every six months.

## Final Memory Budgets

All numbers in MiB. KSM enabled host-side (`hardware.ksm.enable = true`
or equivalent in canix); these budgets assume KSM is reclaiming
identical pages across VMs.

| VM class | `microvm.mem` | `microvm.balloon` | `microvm.deflateOnOOM` | `microvm.initialBalloonMem` | Effective working set | Host reclaimable (idle) |
|---|---|---|---|---|---|---|
| Login (graphical, Steam GUI) | 4096 | true | true | 1024 | ~2.5 GiB Steam + sway + wayvnc + kernel | up to ~1.5 GiB |
| Runtime (headless, game binary) | 2048 | true | true | 512 | project-dependent | up to ~1 GiB |
| Test-cluster TOML override | honoured | inherited | inherited | inherited | per project | per project |

Settings live in [nix/lib/microvm-runner.nix](../../../nix/lib/microvm-runner.nix);
the per-class defaults are picked by which builder calls
`microvm-runner.nix` (login vs runtime), not by an option on the
runner itself. Operators that want to tighten a runner can override
via the NixOS module's `loginRunners.memory` / `runners.memory` (no
new options).

**vCPUs**: 1 per VM, unchanged. (Steam GUI is dominated by single-
thread CEF dispatch; doubling vCPUs gains ~10% wall-clock at 2× host
CPU cost.)

**No hotplug-mem (virtio-mem)**. virtio-mem is more memory-efficient
in steady state than ballooning but adds an extra failure surface (the
host can fail to hot-plug under fragmentation pressure, leaving the
guest stuck below mem). Reliability priority #1 — ballooning's worst
case is "host doesn't reclaim", which is benign.

## Final Disk Budgets

| File | Size | Per how many VMs | Total | vs current |
|---|---|---|---|---|
| `vm-N-home.img` (login runners) | 2048 MiB | up to vmCount (8) | 16 GiB | down from 64 GiB |
| `vm-N-home.img` (runtime runners) | 8192 MiB | up to vmCount (8) | 64 GiB | unchanged |
| Store EROFS (`microvm.storeOnDisk = true`, default) | ~600 MiB/closure | 1 (deduped via Nix store hashing) | shared across VMs of same closure | unchanged |

Per the dossier's Path A decision, the 2048 MiB login-runner image is
enough for Steam config, `loginusers.vdf`, libraryfolders.vdf, and
session tokens. No game content lives on login runners.

**Migration of existing 8 GiB login-runner images**: on atlas, the
existing `vm-N-home.img` files persist across rebuilds (they live in
`$XDG_STATE_HOME/steampipe/`, not the nix store). After the new
defaults land, operators must `rm` the old 8 GiB images for the new 2
GiB ones to take effect. Cluster-ctl gains a `cluster-ctl reset
vm-N --home` convenience for this. (See "Rust code changes" below.)

## NixOS Module — Concrete API Changes

In [nix/module.nix](../../../nix/module.nix):

1. **Default flips.**

   ```nix
   loginRunners.hypervisor.default = "qemu";       # was "crosvm"
   runners.hypervisor.default      = "cloud-hypervisor";  # was "crosvm"
   ```

   `description` and `example` text updated to match.

2. **New assertions.** Add to the existing `assertions = [ ... ]`
   block at [nix/module.nix:362-426](../../../nix/module.nix#L362-L426):

   ```nix
   {
     assertion = !cfg.loginRunners.enable
       || builtins.elem cfg.loginRunners.hypervisor
            [ "qemu" "crosvm" "cloud-hypervisor" ];
     message = ''
       services.steampipe-cluster.loginRunners.hypervisor must be one
       of "qemu" (default, supported), "crosvm" (deprecated; see
       restructure plan), or "cloud-hypervisor" (no virgl, not
       supported for graphical login).
     '';
   }
   {
     assertion = !cfg.runners.enable
       || builtins.elem cfg.runners.hypervisor
            [ "cloud-hypervisor" "qemu" "crosvm" ];
     message = ''
       services.steampipe-cluster.runners.hypervisor must be one of
       "cloud-hypervisor" (default, supported), "qemu", or "crosvm"
       (deprecated).
     '';
   }
   ```

3. **New deprecation warnings.** Add to the `warnings =` block at
   [nix/module.nix:430-436](../../../nix/module.nix#L430-L436):

   ```nix
   ++ lib.optional (cfg.loginRunners.enable && cfg.loginRunners.hypervisor == "crosvm")
        "services.steampipe-cluster.loginRunners.hypervisor = \"crosvm\" is deprecated and scheduled for removal in steampipe 0.4. crosvm + virgl + Vulkan crashes the guest VCPU on Steam GUI startup (see docs/src/planning/hypervisor-restructure-research.md). Switch to \"qemu\"."
   ++ lib.optional (cfg.runners.enable && cfg.runners.hypervisor == "crosvm")
        "services.steampipe-cluster.runners.hypervisor = \"crosvm\" is deprecated and scheduled for removal in steampipe 0.4. Switch to \"cloud-hypervisor\" (or \"qemu\" if you need virgl)."
   ```

4. **Remove the crosvm-overlay-shadow warning.** Once the overlay is
   gone (Wave 3), the warning at
   [nix/module.nix:435-436](../../../nix/module.nix#L435-L436)
   referencing `pkgs.crosvm.passthru.__steampipeOverlay` becomes dead
   code. Delete in the same commit as the overlay deletion.

5. **No new memory options.** The balloon / deflateOnOOM / initial-
   balloon settings live entirely inside `microvm-runner.nix`,
   keyed off the existing `flavor.memory` value and the
   login-vs-runtime selection. Operators with bespoke needs override
   via the existing `loginRunners.memory` / `runners.memory` (already
   `lib.types.ints.positive`).

No other option surface changes in `nix/module.nix`.

## Project-Flake — `nix/lib/test-cluster.nix` Changes

1. **Hypervisor default derived from `graphics`.**

   ```nix
   mkFlavor = f: let
     graphics = f.graphics or vmGraphics;
     hypervisor = f.hypervisor or (if graphics then "qemu" else "cloud-hypervisor");
   in { ... };
   vmHypervisor = clusterConfig.hypervisor
     or (if vmGraphics then "qemu" else "cloud-hypervisor");
   ```

2. **Drop `patchCrosvmGpuParams`.** The `if flavor.gpuProfile == "egl" then ... else ...`
   logic at [nix/lib/test-cluster.nix:135-153](../../../nix/lib/test-cluster.nix#L135-L153)
   exists only to rewrite crosvm's `--params context-types=...` in
   the runner script. With crosvm gone from the default path, this
   whole helper goes. The `gpuProfile` flavour field can stay as a
   no-op for one release (logged-but-ignored) to avoid breaking
   project TOMLs that mention it; remove in 0.4.

3. **Drop the crosvm-graphics branch in `mkRunnersDir`.**

   ```nix
   # before
   path =
     if flavor.hypervisor == "crosvm" && flavor.graphics
     then patchCrosvmGpuParams flavor (mkMicroVM flavor name vm)
     else mkMicroVM flavor name vm;

   # after
   path = mkMicroVM flavor name vm;
   ```

4. **Update header comment.** The hypervisor doc-block at
   [nix/lib/test-cluster.nix:1-15](../../../nix/lib/test-cluster.nix#L1-L15)
   currently says `"crosvm is the supported default"`. Rewrite to
   match the new matrix.

5. **`useSystemdStage1` simplification.** The current cascade at
   [nix/lib/test-cluster.nix:83-90](../../../nix/lib/test-cluster.nix#L83-L90)
   gates on `hypervisor == "crosvm"`. With crosvm out of the default
   path the cascade simplifies — the `useSystemdStage1` flag remains
   user-settable but the auto-true-for-crosvm leg goes away.

## `nix/lib/microvm-runner.nix` Changes

Concrete additions and removals:

1. **Memory budget block** (new, after `mem = ...`):

   ```nix
   balloon = true;
   deflateOnOOM = true;
   initialBalloonMem =
     if loginStateDir != null then 1024  # login runners
     else 512;                            # runtime runners
   ```

   Use `loginStateDir != null` (the existing `shareSteamState` switch)
   as the login-vs-runtime discriminator — no new parameter threads.

2. **QEMU graphical extra args** (new):

   ```nix
   qemu.extraArgs = lib.mkIf (flavor.hypervisor == "qemu" && flavor.graphics) [
     "-display" "none"
     "-device"  "virtio-vga-gl"
     "-object"  "memory-backend-memfd,id=mem,size=${toString microvmConfig.mem}M,share=on"
   ];
   ```

   Note: `memory-backend-memfd` with `share=on` is what enables virgl
   on `-display none`. If Step A (validation phase) finds we need a
   different shape (e.g., `virtio-gpu-gl-pci,blob=on`), the design
   parameter to change is this list — nothing else.

3. **`crosvm.extraArgs`** stays gated under `lib.mkIf (flavor.hypervisor == "crosvm")`,
   so the seccomp-policy-dir injection is dormant for the default
   path. Don't delete it yet — power users with crosvm pins still
   need it until 0.4.

4. **Steam state share protocol** (defer; do NOT change in this
   restructure):
   - The `proto = "virtiofs"` declaration stays but remains inert
     because cluster-ctl bypasses `microvm-host` systemd integration.
   - For login runners, `loginStateDir` continues to be `null` from
     `host-runners.nix`, so the share block doesn't render. Steam
     state lives on the per-VM home image (Path A from D2).
   - When `mkTestCluster` passes `loginStateDir != null` (project-
     flake `nixosIntegration` path), the share renders but only works
     under microvm.host's virtiofsd. That's pre-existing behaviour,
     not regressing here. Document explicitly in a comment so it
     doesn't look like a bug to future readers.

5. **Volume `size`** (login-vs-runtime split):

   ```nix
   volumes = [
     {
       mountPoint = "/home/${vmUser}";
       image = "${name}-home.img";
       size = if loginStateDir != null
              then 2048   # login runners — Steam config only
              else 8192;  # runtime runners — game data lands here
     }
   ];
   ```

   Again use the existing `shareSteamState`/`loginStateDir` switch as
   the discriminator.

## `nix/vm-graphics.nix` Changes

**Unchanged in spirit. Pending Step A confirmation.**

The `VK_DRIVER_FILES =
"/run/opengl-driver/share/vulkan/icd.d/virtio_icd.x86_64.json"` at
[nix/vm-graphics.nix:36-38](../../../nix/vm-graphics.nix#L36-L38)
worked under crosvm's Venus capset (`context-types=...venus...`). QEMU
`virtio-vga-gl` also exposes Venus through the same `virtio_icd.x86_64.json`
ICD on Mesa-driven hosts. Expectation: **no change**.

If Step A finds the ICD path differs in QEMU's virgl pipeline
(unlikely but possible), the targeted change is one path string in
this file. No upstream contract.

## Rust Code Changes

Three surfaces — two follow-ups from the SSH-fix dossier, plus a
host-side admission gate to defend against host-OOM during parallel
VM startup.

### Host-side admission gate (memory-admission)

The guest-side memory contract (balloon + deflateOnOOM) handles
**guest** OOM. It does not handle **host** OOM: if cluster-ctl ever
starts N VMs in parallel and the sum of their initial memory exceeds
host RAM, the host OOM-killer claims one of the crosvm/qemu/cloud-
hypervisor processes and the corresponding VM dies before sshd ever
comes up. Today the `cluster-ctl steam login all` path is sequential
so this isn't reachable, but `cluster-ctl up`, harness parallel test
fan-out, and any future parallel-login path are all latent exposures.

Add the **[memory-admission](https://crates.io/crates/memory-admission)**
crate (0.1.7, MIT/Apache-2.0) as a host-side admission gate around
parallel VM startup. The async (`memory_admission::r#async::AdmissionGate`)
variant fits cluster-ctl's tokio runtime; the `ProcMeminfoProvider`
(Linux-only, reads `/proc/meminfo`) is the right provider for atlas.

**Integration shape**:

1. New module `src/core/admission.rs`. Owns a singleton
   `AdmissionGate` initialised at cluster-ctl startup. Memory
   threshold derived from the VM class budgets:

   ```rust
   pub fn admission_gate() -> &'static AdmissionGate {
       static GATE: OnceLock<AdmissionGate> = OnceLock::new();
       GATE.get_or_init(|| {
           AdmissionGate::new(Config {
               // Refuse new admissions when free host RAM falls
               // below the largest single VM class (4 GiB login) +
               // 1 GiB host headroom.
               high_watermark_mb: 5 * 1024,
               low_watermark_mb:  6 * 1024,  // hysteresis band
               provider: Box::new(ProcMeminfoProvider::default()),
               ..Config::default()
           })
       })
   }
   ```

   Tune the watermarks during phase execution based on
   `nproc`/`mem-total` on atlas. The hysteresis band prevents
   start-stop-start-stop flapping when one VM's startup pushes RAM
   over the threshold momentarily.

2. Gate `backend.start_instance(...)` call sites where multiple VMs
   could be in-flight concurrently. Audit list:
   - [src/core/backend.rs:222-225](../../../src/core/backend.rs#L222-L225)
     (the `Backend::start_instance` dispatch — wrap at the call
     sites, not inside the dispatch itself, so single-VM paths stay
     fast).
   - `cluster_up_all` / equivalent in `src/lib.rs` and `src/cli/`.
   - `harness::runner` parallel test launches (search for
     `tokio::spawn` + `start_instance`).
   - `src/vm/lifecycle.rs` `wait_for_ssh`-related fan-out.

   Wrap each parallel start with:

   ```rust
   admission::admission_gate().wait().await;
   let pid = backend.start_instance(config, vm, runners_dir)?;
   ```

3. Sequential paths (today's `cluster-ctl steam login all` iterates
   one VM at a time) get the same `gate.wait().await` call. It's a
   no-op when no other VM startup is in flight; preserves the
   property that future refactors don't lose the protection.

4. On `gate.wait()` returning `Err`, the gate's provider failed (e.g.
   `/proc/meminfo` unreadable). Log via `tracing` and proceed
   without throttling — matches the crate's documented graceful-
   degradation behaviour. Reliability priority #1 means we never
   block startup on a provider failure.

5. Add a `cluster-ctl admission status` subcommand that prints the
   current `MemoryStats` sample plus the configured watermarks.
   Useful for operators diagnosing "why is my second VM startup
   waiting?".

**Test surface**:

- Unit test the gate config using `FixedProvider` (in-crate stub):
  set "free memory = 1 GiB", call `wait()` from two tasks, assert
  the second blocks until the stub is bumped above the low watermark.
- Integration check: on atlas, start 4 login VMs in parallel via a
  test harness and confirm none get OOM-killed during startup.

**Cargo.toml**:

```toml
memory-admission = { version = "*", default-features = false, features = ["tokio"] }
```

The crate is owned by this project's author — no external semver-pin
risk; follow the latest. `tracing` is already a workspace dep; no
new transitive surface.

**Why not at the microvm.nix level?** Because the host-OOM risk is
*per-process*, and the actors deciding when to spawn those processes
are cluster-ctl (Rust) and harness/runner (Rust). The microvm.nix /
NixOS module layer never starts more than one VM in a single
evaluation; the concurrency is entirely in cluster-ctl's control
plane.

### Diagnostic tail on empty stdout/stderr (Step F)

### Diagnostic tail on empty stdout/stderr (Step F)

In [src/game/steam.rs:1119-1129](../../../src/game/steam.rs#L1119-L1129),
the `finish_steam_login` error path currently formats:

```rust
anyhow::bail!(
    "{}: Steam GUI validation failed ({}). stderr: {}. Use VNC fallback if Steam Guard requires GUI confirmation.",
    vm.name, output, stderr
);
```

When `output.is_empty() && stderr.is_empty()`, replace with a richer
message:

```rust
if output.is_empty() && stderr.is_empty() {
    // SSH command produced no output. Either the VM crashed during the
    // call (vm.log tail will show it) or sshd dropped the connection.
    let log_tail = read_vm_log_tail(&config.state_dir, vm)
        .unwrap_or_else(|| "(no vm.log available)".to_string());
    anyhow::bail!(
        "{}: Steam GUI validation produced no output — VM likely crashed during the call. \
         vm.log tail:\n{}",
        vm.name, log_tail
    );
}
anyhow::bail!(
    "{}: Steam GUI validation failed ({}). stderr: {}. Use VNC fallback if Steam Guard requires GUI confirmation.",
    vm.name, output, stderr
);
```

The helper `read_vm_log_tail` factors out the existing
`print_vm_log_tail` body from
[src/game/steam.rs:748-770](../../../src/game/steam.rs#L748-L770).
Wire it through `config.state_dir` (already plumbed). Apply the
same pattern to any other `run_cmd` → `bail!` site that ignores
empty-output cases. Audit list (do during phase execution): grep
`bail!` in `src/game/steam.rs` and `src/harness/runner.rs`.

This change is **independent of the hypervisor migration** and
should land in Wave 0.

### `cluster-ctl reset <vm> --home`

New subcommand (or `cluster-ctl reset-home <vm>`) that:

1. Locates `$XDG_STATE_HOME/steampipe/<cluster>/<vm>/<vm>-home.img`.
2. Verifies the VM is not running (`lease.read_claim`).
3. `rm` the file.

Used to retire the existing 8 GiB login-runner home images so the
new 2 GiB default takes effect on next boot. Document it next to the
disk-budget docs.

Implementation lives in `src/cli/` (subcommand parsing) +
`src/game/lifecycle.rs` (the file removal). Size: ~40 lines + 1 test.

## Flake Check Changes

In [nix/flake/checks.nix](../../../nix/flake/checks.nix):

### Add

1. `module-qemu-loginrunners-eval` — eval a minimal NixOS host with
   `services.steampipe-cluster.loginRunners.enable = true;
   sshAuthorizedKey = "<fake>"; hypervisor = "qemu";` and assert the
   resulting `loginRunnersDir` derivation builds.
2. `module-cloud-hypervisor-runners-eval` — same shape for
   `runners.enable = true; hypervisor = "cloud-hypervisor";`.
3. `module-crosvm-deprecation-warning` — eval with the crosvm pin,
   assert the deprecation warning string is in the config warnings
   list.

Each follows the existing `module-runners-eval` /
`module-loginrunners-with-key-evals` pattern.

### Remove (Wave 3, after overlay deletion)

1. The `crosvmMarker`/`__steampipeOverlay` check at
   [nix/flake/checks.nix:373-383](../../../nix/flake/checks.nix#L373-L383)
   — deletes alongside the overlay.

## Overlay Deletions (Wave 3)

[nix/flake/overlay.nix](../../../nix/flake/overlay.nix):

- Delete the entire `crosvm = prev.crosvm.overrideAttrs ...` block
  at lines 28-196 (~169 lines).
- Delete the `cloud-hypervisor-graphics = prev.cloud-hypervisor-graphics.overrideAttrs ...`
  block at lines 20-26 — the headless path uses plain
  `cloud-hypervisor`, the graphical path is QEMU.
- The file shrinks from 198 lines to a near-empty
  `nixpkgs.lib.composeExtensions microvm.overlay (final: prev: {})`.
  Either keep it as a passthrough (in case future overlays land) or
  delete the file entirely and unwire it from `flake.nix`.

Gated on: A green (QEMU works on atlas) **and** B green
(cloud-hypervisor headless works) **and** C green (login default
flipped, verified by `cluster-ctl steam login vm-N` on atlas without
a VCPU crash).

## Documentation Changes

- [docs/src/getting-started/installation.md](../../getting-started/installation.md):
  - Hypervisor examples in module config snippets — replace any
    `hypervisor = "crosvm"` with the new defaults (or omit
    entirely, since defaults now match what we want).
  - Add a one-paragraph "What runs where" section explaining the
    QEMU-graphical / cloud-hypervisor-headless split.

- [docs/src/configuration/host-config.md](../../configuration/host-config.md):
  - Update `loginRunners.hypervisor` and `runners.hypervisor` rows
    to show the new defaults.

- New page **`docs/src/configuration/vm-resources.md`**:
  - The memory contract: balloon + deflateOnOOM rationale.
  - The KSM expectation (host-side `hardware.ksm.enable = true`).
  - The 2 GiB / 8 GiB home-image split and the rationale.
  - Reference the `cluster-ctl reset --home` recovery tool.
  - Link from `SUMMARY.md` under Configuration.

- [docs/src/configuration/visual-testing.md](../../configuration/visual-testing.md):
  - One paragraph: "Hyprland is intentionally not supported. wayvnc
    is wlroots-only; hyprland's Aquamarine compositor is partial-
    compat; the swaymsg IPC surface this repo depends on
    (`get_outputs`, `get_tree`) is not portable. See the
    hypervisor-restructure research dossier for details."

- The two existing planning artifacts
  ([login-runner-ssh-fix/](./login-runner-ssh-fix/) and
  [login-runner-ssh-unreachable-findings.md](./login-runner-ssh-unreachable-findings.md))
  stay as historical record. After Wave 3 ships, P1's atlas-rebuild
  retirement (D4) lets the whole `login-runner-ssh-fix/` directory
  retire via the verify-mode auto-retire path.

- The research dossier
  ([hypervisor-restructure-research.md](./hypervisor-restructure-research.md))
  stays. Mark this design as its companion in the dossier's footer
  during plan execution.

## Host-Side (canix) Changes

Outside the steampipe repo, but listed for completeness because the
memory budgets assume them:

1. **Rebuild atlas** to pick up the canix commit `995ff7b` and
   activate the explicit-key path. Retires SSH-fix P1 (D4).
2. **Enable KSM** in canix's atlas config:

   ```nix
   hardware.ksm.enable = true;
   ```

   (Or the canix-equivalent option name; verify against the canix
   module surface during phase execution.) The memory budget table
   assumes KSM is running.

Both are canix commits, not steampipe. They need a separate phase or
two when this plan is broken down — the steampipe phases cannot land
the canix change.

## Sequencing Constraints (Final)

```
              ┌──────── A (QEMU virgl validates on atlas) ──┐
              │                                              ├─→ C ─→ D ─┐
              │                                              │            ├─→ G
              ├──────── B (cloud-hypervisor headless)  ─────┘            │
              │                                                            │
   Wave 0 ────┼──────── E (shrink login home image)                       │
              │                                                            │
              ├──────── F (empty-stderr diagnostic)                       │
              │                                                            │
              ├──────── H (document hyprland-no)                          │
              │                                                            │
              ├──────── I (memory-admission host-side gate)               │
              │                                                            │
              └──────── Atlas-rebuild + KSM (canix)                       │
                                                                           │
              Wave 1: C  (after A)                                         │
              Wave 2: D  (after B + C)                                    │
              Wave 3: G  ─────────────────────────────────────────────────┘
```

Dependencies:

- **A → C**: must prove QEMU virgl works on atlas before making it the
  login-runner default.
- **B + C → D**: balloon + deflateOnOOM live in microvm-runner.nix and
  only matter on qemu / cloud-hypervisor; until both new paths exist,
  D has nothing to attach to.
- **A + B + C all green → G**: do not delete the crosvm overlay until
  both replacements have shipped a real `cluster-ctl steam login` +
  `cluster-ctl steam test` cycle on atlas.

Independent (any order):

- **E** (shrink login home image) — pure number change in
  microvm-runner.nix.
- **F** (empty-stderr diagnostic) — Rust-only, unblocked.
- **H** (docs) — markdown only.
- **I** (memory-admission host-side gate) — Rust-only, unblocked.
  Adds the `src/core/admission.rs` module and Cargo dependency.
- **Atlas rebuild + KSM** — canix-side, no steampipe coupling.

Recommended Wave 0 fan-out: A, B, E, F, H, I, Atlas — six steampipe
phases plus one canix phase. Reasonable to run all seven in parallel
sessions.

## Empirical Dependencies That Could Force Redesign

Listed so a planner knows when to bail. The design is **stable iff**
these all hold:

1. **A (QEMU virgl on atlas) succeeds** with `-display none -device
   virtio-vga-gl` (or a near variant). If atlas's GPU stack rejects
   virgl-without-host-display:
   - Fallback 1: try `virtio-gpu-gl-pci,blob=on`.
   - Fallback 2: software EGL (llvmpipe) — viable for Steam GUI
     validation but slow.
   - Fallback 3: GPU passthrough to the VM — much larger surface.
   This invalidates the C, D, G waves. The plan would need a
   different graphical path or stick with crosvm for graphical
   (defeating the whole point).

2. **B (cloud-hypervisor headless from plain upstream) works without
   the spectrum-os patch stack.** Confirmed in nixpkgs 26.05; if
   plain CHV fails for an unrelated reason, we'd have to keep the
   `-graphics` variant or pin a different CHV release. Doesn't
   invalidate the design but pulls the overlay deletion (G) back.

3. **virtio-balloon + deflateOnOOM actually rescues a guest** under
   memory pressure on Linux 6.18. The microvm.nix option exists; the
   guest kernel includes `CONFIG_VIRTIO_BALLOON=y`. Worst case: OOM
   killer fires before the balloon-deflate signal lands and kills a
   critical Steam process. If this happens reliably, lower the
   `initialBalloonMem` to give the guest more headroom from the
   start — a number tweak, not a redesign.

4. **`hardware.ksm.enable` is the right canix option name.** Probably
   right; verify during the canix phase. If different, just rename
   in the canix commit.

5. **The 2 GiB login-runner home image fits Steam config + login
   state.** Steam's typical `~/.local/share/Steam` is ~300-600 MiB
   without games. 2 GiB has 3-7× headroom. Should hold.


None of these are blockers for *writing* the plan; each phase records
the empirical check in its acceptance criteria.
