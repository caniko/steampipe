# Plan: Config-less CLI from NixOS module

> **Recommended Codex model for orchestration: GPT 5.5 medium**
>
> The plan is well-scoped (six phases, dependencies resolved by the
> dossier, two open blockers answered by the user). Orchestration only
> needs to track wave gating and verify that each phase's acceptance
> criteria still hold after merges. That's a moderate-complexity
> orchestrator role — 5.5 medium covers it without burning xhigh tokens
> on routine wave-flipping.

## Scope and current state

Steampipe's CLI today demands `--login-runners-dir` (and friends) when
run outside a project root, even on a NixOS host where canix has fully
provisioned the cluster via `services.steampipe-cluster`. `vmCount`
already falls through from `/etc/steampipe/module.json` to
`cluster-ctl` ([src/lib.rs:634-641](../../../../src/lib.rs#L634-L641)),
but `loginRunnersDir` doesn't exist in the schema and isn't built at
the NixOS-module level — login runners are project-flake artifacts
today ([nix/lib/test-cluster.nix:268-277](../../../../nix/lib/test-cluster.nix#L268-L277)).

This plan closes that asymmetry. After execution:

- `cluster-ctl steam login` works from `$HOME` on a NixOS-deployed
  host (no project root, no flags).
- The NixOS module owns SSH key generation for host-level login VMs.
- Project flake wrappers delegate to host-level runners when
  `nixosIntegration` is on; the project-only build path is the
  fallback for non-NixOS hosts.
- Schema stays at `schemaVersion: 2` with additive optional fields
  (no bump).

Source dossier:
[cli-configless-from-nixos-research.md](../cli-configless-from-nixos-research.md).

## User-resolved design decisions

| Question | Decision |
|---|---|
| SSH-key source for host-level login runners | NixOS-managed (module generates + owns the keypair). |
| Project-level wrapper deprecation | Migrate — wrappers delegate to host-level runners when available. |
| Schema bump vs. additive | Stay v2; add optional fields (`loginRunnersDir`, `sshKey`, `vmUser`). |
| Default for `services.steampipe-cluster.loginRunners.enable` | `false` initially, flip to `true` after one release cycle. |

## Phase table

| Phase | File | Depends on | Touches | Can parallel with | Blocking? |
|---|---|---|---|---|---|
| 01 | [01-host-config-schema-fields.md](./01-host-config-schema-fields.md) | — | `src/core/nixos_module.rs`, `docs/src/configuration/host-config.md` | — | unblocks 02 and 03 |
| 02 | [02-cli-resolver-refactor.md](./02-cli-resolver-refactor.md) | 01 | `src/lib.rs`, `src/ui/cli.rs` | 03 | unblocks 04 |
| 03 | [03-nixos-module-login-runners.md](./03-nixos-module-login-runners.md) | 01 | `nix/module.nix`, `nix/lib/login-runners.nix` (new), `nix/lib/microvm-runner.nix` (new), `nix/lib/test-cluster.nix` (extract only) | 02 | unblocks 04 and 06 |
| 04 | [04-canix-wiring-and-smoke.md](./04-canix-wiring-and-smoke.md) | 02, 03 | `canix/root/hosts/atlas/features/steampipe.nix`, smoke evidence | — | unblocks 05 |
| 05 | [05-docs-ux-cleanup/README.md](./05-docs-ux-cleanup/README.md) | 04 | `README.md`, `docs/src/configuration/*.md`, `src/ui/cli.rs` help strings | 06 | closes user-facing surfaces |
| 06 | [06-migrate-project-wrappers.md](./06-migrate-project-wrappers.md) | 03 | `nix/lib/test-cluster.nix`, downstream consumers (chessbender) | 05 | closes deprecation |

### Per-phase routing tier

| Phase | Layout | Model |
|---|---|---|
| 01 | flat | 5.5 low |
| 02 | flat | 5.5 medium |
| 03 | flat | 5.5 high |
| 04 | flat | 5.5 medium |
| 05 | dir (3 sub-layers, each 5.5 low) | 5.5 low (merge) |
| 06 | flat | 5.5 medium |

## Parallelism layer

```
Wave 0 — foundation
  ├── 01 host-config-schema-fields   (no deps; pure capacity add)
  └── (nothing else; everything else needs 01's struct fields)

Wave 1 — capacity exploitation  [parallel-safe]
  ├── 02 cli-resolver-refactor       (lib.rs + cli.rs help strings)
  └── 03 nixos-module-login-runners  (nix/ tree)
        Disjoint files; can run concurrently after 01.

Wave 2 — integration
  └── 04 canix-wiring-and-smoke      (needs both 02 + 03)
        Single phase; this is where the end-to-end claim gets proved.

Wave 3 — completion  [parallel-safe]
  ├── 05 docs-ux-cleanup             (README + configuration/*.md + help text)
  │     Sub-layered into 3 disjoint files.
  └── 06 migrate-project-wrappers    (nix/lib/test-cluster.nix)
        Disjoint files from 05; can run concurrently after 04.

Plan exhausted.
```

### Serialization points

- 01 → everything: schema fields are the load-bearing add; do not start 02 or 03 until 01 is merged.
- 04 ← 02 + 03: the smoke test is the integration claim. Do not skip.
- 05 sub-layers serialize on `git pull` only; each sub-layer touches disjoint files.

## Whole-set acceptance criteria

- [ ] `cluster-ctl steam login` from `$HOME` on atlas succeeds with no flags, no project root, no env overrides.
- [ ] `cluster-ctl steam info` from `$HOME` shows a `loginRunnersDir` row sourced from the discovered host config (selected, not "(not set)").
- [ ] Existing chessbender (project-flake) wrappers still work unchanged: `nix run .#cluster-steam-login` from the chessbender repo continues to log into Steam end-to-end.
- [ ] `cluster-ctl steam login --login-runners-dir <path>` from `$HOME` still overrides the host config (precedence preserved).
- [ ] Schema parsing tests for schemaVersion 1 and 2 still pass; new optional fields parse correctly when present and as `None` when absent.
- [ ] No regression in non-NixOS host workflow: `host.json` hand-written by a user without `services.steampipe-cluster` continues to work for `steam info`, and falls back to `--login-runners-dir` for `steam login`.
- [ ] Project-level wrappers delegate to host-level runners when `nixosIntegration = true` is set in `mkTestCluster`; the project's `loginVMRunnersDir` builder is only invoked when `nixosIntegration = false`.

## Shared-file lockstep

Two phases touch the same file and must serialize:

- **`nix/lib/test-cluster.nix`** is touched by both phase 03
  (extracts `mkMicroVM` into `nix/lib/microvm-runner.nix` and
  imports it back) and phase 06 (rewrites `mkSteamLoginWrapper` and
  the three call sites). Phase 03 lands first; phase 06 *must*
  rebase on the post-phase-03 tree before starting. Both phases'
  Plan sections name this constraint explicitly.

- **`src/ui/cli.rs`** is touched by phase 02 (`Option<PathBuf>`
  type changes on `runners_dir` for `steam warm`) and sub-layer
  05.03 (help-text doc-comment edits). Phase 02 lands first;
  sub-layer 05.03 rebases on it. Sub-layer 05.03's Pitfalls
  section calls this out.

## External repo coordination

Phases 04, 05, and 06 reach beyond the steampipe repo:

- **Phase 04** edits canix
  (`/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix`)
  and bumps the steampipe input via `nix flake update steampipe`.
  Coordinate the canix PR with the steampipe phase-03 merge —
  canix can't switch to `loginRunners.enable = true` until
  steampipe's main carries that option.

- **Phase 06** smoke-tests against a downstream consumer
  (chessbender or equivalent project flake). Bumping the
  consumer's `flake.lock` is the consumer's responsibility; phase
  06 only verifies that the steampipe-side wrapper change is
  consumer-compatible.

- Cross-repo merge order: **steampipe phase-03 → canix phase-04 →
  steampipe phase-06 → chessbender consumer bump**. Don't fan
  these out concurrently across repos; each step needs the
  previous step's main-branch state.

## PR sequencing & cross-owner coordination

The work spans three maintainer surfaces:

| Surface | Owner | Phases | Coordination need |
|---|---|---|---|
| steampipe Rust crate | this repo | 01, 02 | Self-contained; merge in order. |
| steampipe Nix lib + module | this repo | 03, 06 | Both touch `nix/lib/`; serialize per the shared-file lockstep above. |
| canix host config | canix repo | 04 | Depends on steampipe phase-03 being on a published commit (codeberg). |
| downstream project flakes (chessbender, etc.) | each project | 06 verification | Consumer-side `nix flake update steampipe` after phase 06; not blocking for steampipe-side merge. |

Land in the order 01 → 02/03 (parallel) → 04 → 05/06 (parallel).
Surface each PR with a reference to the plan README so reviewers
across the three surfaces can see the same shared picture.

## Serial-chain recovery

The dependency chain has four levels (01 → 02/03 → 04 → 05/06).
If wave 1 stalls (e.g., phase 03's SSH key design needs revision
mid-implementation), the rest of the plan blocks. Recovery steps:

1. **If phase 02 is the blocker**: phase 03 can ship
   independently because the field already exists in the schema
   (phase 01). Phase 04's smoke will fail without phase 02's
   resolver, but phase 03 + phase 06 can land and be useful for
   project-wrapper migration. Defer phase 04 until phase 02
   completes.

2. **If phase 03 is the blocker** (e.g., the SSH-key approach
   from step 2 needs to pivot): phase 02 still ships
   independently — it can read the field with no producer. Phase
   06 must wait. Phase 04 smoke must wait. Use the time to land
   phase 05 sub-layers 01 and 02 (doc edits don't depend on the
   code path being functional yet); defer sub-layer 03 (help text)
   until phase 02's resolver behavior is final.

3. **If phase 04 smoke fails on atlas**: file a bug against the
   responsible phase, don't paper over in phase 04 itself. Phase
   04 is a verification gate, not a fixup window.

4. **Stale plan after a long stall**: if more than ~2 weeks
   pass between waves, re-read the dossier and re-route any
   phase whose complexity assumptions shifted. The plan is
   small enough that full re-decomposition is rarely worth it.

## Cleanup batch

Three phases (01, 05.01, 05.02, 05.03) are `5.5 low` tier
mechanical edits. They can be bundled into one cleanup batch if
the user wants to dispatch them with a single low-cost agent
session each, in this order:

1. Phase 01 (schema fields) — sets up everything else.
2. Sub-layer 05.01 (README + quick start) — only after phase 04 smoke is green.
3. Sub-layer 05.02 (configuration docs) — same gate as 05.01.
4. Sub-layer 05.03 (CLI help text) — same gate as 05.01.

Bundling these onto the same shift keeps the routing-tier
distribution roughly half cheap, half medium-or-higher, matching
the plan's economic profile.

## Global constraints

- **No breaking changes to schemaVersion 1/2 readers.** All new fields are `Option<…>` in `NixosModuleConfig`. Old `module.json` files must continue to parse without warnings beyond the existing schemaVersion-1 deprecation warning.
- **No new top-level CLI flags.** All new resolution paths reuse the existing flags (`--login-runners-dir`, `--ssh-key`, `--vm-count`) for explicit overrides; the host config provides the fallback.
- **Error messages must name the NixOS option.** When a required field cannot be resolved, the error must point the user to either the flag *or* the `services.steampipe-cluster.*` option that would populate it.
- **No CLI behavior change when host config is absent.** All discovery is best-effort; the project-root path remains the default fallback.

## Reference

- Research dossier: [cli-configless-from-nixos-research.md](../cli-configless-from-nixos-research.md)
- Host config docs (current): [docs/src/configuration/host-config.md](../../configuration/host-config.md)
- NixOS module: [nix/module.nix](../../../../nix/module.nix)
- Reader: [src/core/nixos_module.rs](../../../../src/core/nixos_module.rs)
- CLI entry: [src/lib.rs](../../../../src/lib.rs)
- Project Nix lib: [nix/lib/test-cluster.nix](../../../../nix/lib/test-cluster.nix)
- canix atlas wiring: `/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix`
