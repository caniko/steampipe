# Hypervisor Patches Findings

## Goal And Trigger

User asked whether the patches steampipe applies to hypervisors can be
removed today, or whether they are still needed. The repo has one
overlay file ([nix/flake/overlay.nix](../../../nix/flake/overlay.nix))
that mutates `pkgs.crosvm`, and one in-tree post-build substitution
([nix/lib/test-cluster.nix:154-172](../../../nix/lib/test-cluster.nix#L154))
that rewrites the generated `microvm-run` script. The
`cloud-hypervisor-graphics` override and `__steampipeOverlay` marker
were already removed in commit
[`4ef2de8`](../../../) so this answer is scoped to what is left in
HEAD.

## Root Cause(s)

There are five distinct patches with three different upstream tracks.
Status (verified 2026-05-27):

| # | Patch | Where | Why | Upstream | Can drop now? |
|---|---|---|---|---|---|
| 1 | `version = "107.1-unstable-..."` + `__intentionallyOverridingVersion = true` | [nix/flake/overlay.nix:28-29](../../../nix/flake/overlay.nix#L28) | Makes microvm.nix's crosvm runner skip its `--seccomp-log-failures` workaround that breaks text-mode policy parsing. | [microvm-nix/microvm.nix#516](https://github.com/microvm-nix/microvm.nix/pull/516) **merged 2026-05-07** into `main`. | **Not yet — input is stale.** The locked rev (`c0a53823`, 2026-04-12) is 19 commits behind `main`; the merge is among them. `nix flake update microvm` brings it in. |
| 2 | `buildInputs += [aemu gfxstream]`, `buildFeatures += ["gfxstream"]`, `nativeBuildInputs += [python3]` | [nix/flake/overlay.nix:30-43](../../../nix/flake/overlay.nix#L30) | Mesa Venus / gfxstream support for the deprecated `hypervisor = "crosvm"` graphical escape hatch. | Not mirrored by either tracked PR. microvm.nix#518 ("Make crosvm graphics package configurable", merged) moves only the runner side. | **No.** Tied to the crosvm graphical path that the design pins for the 2026-Q4 nixpkgs window (see [graphics-and-games.md:82](../../../docs/src/configuration/graphics-and-games.md#L82)). |
| 3 | `postInstall` that installs `.policy` + pre-compiled `.bpf` files to `$out/share/policy/<arch>` | [nix/flake/overlay.nix:84-159](../../../nix/flake/overlay.nix#L84) | crosvm only reads external seccomp policies via `--seccomp-policy-dir`, and nixpkgs ships none. The microvm runner uses `--seccomp-policy-dir=${pkgs.crosvm}/share/policy/...` ([nix/lib/microvm-runner.nix:64-67](../../../nix/lib/microvm-runner.nix#L64)). | [NixOS/nixpkgs#517363](https://github.com/NixOS/nixpkgs/pull/517363) **OPEN** (last update 2026-05-11). | **No.** PR has not merged. |
| 4 | `postPatch` + post-install `sed` widening `prctl: arg0 == PR_SET_VMA` → also `PR_SET_NAME`; `madvise: arg2 == ...` → also `arg2 == 102` (MADV_GUARD_INSTALL); applied to `common_device.policy` and `gpu_common.policy` | [nix/flake/overlay.nix:72-83](../../../nix/flake/overlay.nix#L72) and [:147-152](../../../nix/flake/overlay.nix#L147) | Kernel ≥ 6.13 + glibc ≥ 2.40 emit these unconditionally during `pthread_create`. Without the widening every device proxy worker dies with SIGSYS. | Bundled inside nixpkgs#517363 — same gate as (3). | **No.** Same gate. |
| 5 | `substituteInPlace "$out/bin/microvm-run"` rewriting virtio-gpu `context-types` for the venus / egl flavours | [nix/lib/test-cluster.nix:154-172](../../../nix/lib/test-cluster.nix#L154) | Switches between virglrenderer-Venus and EGL-virgl scanout paths per `gpuProfile`. Not a nixpkgs concern. | None — this is steampipe configuration, not a workaround for a missing upstream feature. microvm.nix#518 lets us swap in a different graphics crosvm package upstream, but does not let us choose context-types per-runner. | **No** (independent of the upstream PRs). Lives or dies with the deprecated graphical crosvm escape hatch. |

## Evidence

Commands run:

- `gh api repos/astro/microvm.nix/pulls/516 --jq '{state, merged}'` →
  `{"merged":true,"state":"closed"}`.
- `gh api repos/astro/microvm.nix/compare/c0a53823dbf7eb166c2fa7dc2d1e0d6cb2be7562...main --jq '{ahead, behind}'`
  → `{"ahead":19,"behind":0}`. Among those 19 commits is "crosvm
  runner: drop `--seccomp-log-failures` version-gate".
- `gh api repos/NixOS/nixpkgs/pulls/517363 --jq '{state, merged}'`
  → `{"merged":false,"state":"open"}`.
- `grep -rn 'overrideAttrs\|substituteInPlace' nix/ flake.nix` →
  three sites: the crosvm overlay block, and the two
  `patchCrosvmGpuParams` substitutions in `test-cluster.nix`. No other
  hypervisor (qemu, cloud-hypervisor, firecracker) is patched.
- Existing stabilization dossier
  [docs/src/planning/stabilization-and-hardening-research.md](./stabilization-and-hardening-research.md)
  S15 / N13 already flag "add a `// REMOVE-AT-0.4` breadcrumb in the
  overlay so the cutoff is greppable" — that breadcrumb still does
  not exist (`grep REMOVE-AT-0.4 nix/flake/overlay.nix` → no match).

## Recommended Fix

**Today: the patches are still needed.** The deletion happens in two
stages, gated on upstream:

1. **When ready: `nix flake update microvm` + delete patch (1).** The
   merge of microvm.nix#516 makes the `version = "107.1-..."`
   override and the `__intentionallyOverridingVersion = true` line
   dead weight. After bumping the input, delete lines
   [overlay.nix:28-29](../../../nix/flake/overlay.nix#L28) and the
   long comment block above them ([:21-27](../../../nix/flake/overlay.nix#L21)).
   Validate with `nix flake check` and one `nix run
   .#cluster-1v1-up` to confirm crosvm still boots without
   `--seccomp-log-failures` injected. No coordination with the
   nixpkgs PR is required for this step.

2. **When nixpkgs#517363 merges and reaches our nixpkgs pin:** delete
   the entire `postPatch` block ([:72-83](../../../nix/flake/overlay.nix#L72))
   and the policy-install + sed-widening half of `postInstall`
   ([:91-152](../../../nix/flake/overlay.nix#L91)). What remains in
   `overrideAttrs` after that is the gfxstream/aemu/python3
   buildInputs and the buildFeature — i.e., only patch (2), which is
   the deprecated-graphical-crosvm concern.

3. **When the design's deprecation horizon arrives
   (target: 2026-Q4 nixpkgs, post QEMU 11.x / Mesa 27.x):** delete
   the `crosvm = prev.crosvm.overrideAttrs (...)` block entirely
   *and* delete `patchCrosvmGpuParams` from
   [nix/lib/test-cluster.nix](../../../nix/lib/test-cluster.nix), the
   `crosvm.extraArgs` `--seccomp-policy-dir` line from
   [nix/lib/microvm-runner.nix:64-67](../../../nix/lib/microvm-runner.nix#L64),
   and remove `"crosvm"` from the accepted hypervisor enum (this
   completes the work scoped under hypervisor-restructure design D3).

## Optional Follow-Ups

- **Add the `// REMOVE-AT-0.4` breadcrumb that N13 already asked for.**
  Single-line comment at the top of the `crosvm.overrideAttrs` block
  in `nix/flake/overlay.nix`. Picks up the existing stabilization
  recommendation without expanding scope.
- **Refresh the upstream tracking comments** in
  [overlay.nix:128-134](../../../nix/flake/overlay.nix#L128) to
  reflect microvm.nix#516's merge — the current text says "makes the
  version override unnecessary once it lands", but it has landed, so
  the next reader benefits from the corrected status. Two-word edit.

## Open Decisions

None blocking the recommended fix. The two upstream PRs (one merged,
one open) drive the schedule; no choice is required from the user
until step 1 is run.
