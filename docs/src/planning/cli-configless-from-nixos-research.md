---
title: Config-less CLI from NixOS module — Research Dossier
---

# Config-less CLI from NixOS module — Research Dossier

## Goal And Trigger

User ran `cargo run -- steam login --target all` on atlas and hit two
friction points back-to-back:

1. `--target` is not a flag — `TARGET` is positional, so the parser
   rejected the form documented (informally) on the previous turn.
2. With the correct `cargo run -- steam login` form, the CLI bailed with
   `--login-runners-dir is required when running outside a project root`.

The user's framing is that the **paradigm is broken**: when the host is
provisioned via canix (`services.steampipe-cluster.*`), every fact the
CLI needs — VM count, bridge layout, login state dir, credentials path,
account list, login runners — should already be available without
project context or extra flags. They want a UI/UX redesign of the CLI
so that on a NixOS-deployed host, `cluster-ctl steam login` is the only
incantation that's ever needed.

Downstream planner default: `multi-phase-plan-mixed`. Some phases are
mechanical (CLI flag refactor, doc churn) and benefit from cheap
routing; others (Nix schema bump, login-runners packaging) need design
judgment. Mixed routing optimizes cost-per-quality. If the user
explicitly asks for a single-provider plan, fall back to
`multi-phase-plan-codex`.

## Current Reality

### CLI demands today

`cluster-ctl steam login` requires resolution of three pieces of state
that the global host config does **not** currently publish:

| Required by CLI | Source today (project) | Source today (host config) | Gap |
|---|---|---|---|
| `vm_count` | `--vm-count` flag, baked into Nix wrappers ([nix/lib/test-cluster.nix:320](../../../nix/lib/test-cluster.nix#L320)) | `module.vm_count` ([src/lib.rs:636](../../../src/lib.rs#L636)) | None — host config already wins when flag omitted |
| `login_runners_dir` | `--login-runners-dir <path>` flag, baked into `mkSteamLoginWrapper` ([nix/lib/test-cluster.nix:424,453,461](../../../nix/lib/test-cluster.nix#L424)) | **Not emitted** by [nix/module.nix](../../../nix/module.nix) | Hard gap — bails at [src/lib.rs:470](../../../src/lib.rs#L470) |
| `project_root` | git/Cargo.toml autodetect ([src/lib.rs:509](../../../src/lib.rs#L509)) | n/a — falls through to module if `steam_only_command` matches ([src/lib.rs:511-522](../../../src/lib.rs#L511)) | Works for Steam subcommands when a module is found |

Secondary fields the CLI silently inherits from the project (and that
the host config also does not emit): `ssh_key`, `vm_user`,
`backend_kind`, `cluster_name`, runtime `state_dir` / `lock_dir`
overrides. For atlas's primary use case (single cluster, default
backend, default user), these have stable defaults and aren't actually
required — but they exist as latent friction points that will trip the
next person who runs the CLI outside a project.

### What the NixOS module emits today

[nix/module.nix:212-233](../../../nix/module.nix#L212-L233) writes
`/etc/steampipe/module.json` with schemaVersion 2:

```text
schemaVersion, vmCount, bridge, subnet, prefix, hostIp, tapOwner,
loginStateDir, credentialsPath, accounts
```

Conspicuously absent: `loginRunnersDir`, `vmRunnersDir`, `sshKey`,
`vmUser`, `backend`. These exist only inside per-project flake
machinery ([nix/lib/test-cluster.nix:268-277](../../../nix/lib/test-cluster.nix#L268-L277)
builds `loginVMRunnersDir` as a `pkgs.linkFarm` of per-VM
`microvm-run` wrappers with **project-specific** `sshPublicKey` baked
in via `mkMicroVM`).

### What canix actually wires on atlas

[canix/root/hosts/atlas/features/steampipe.nix](file:///data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix)
sets `services.steampipe-cluster.{vmCount=8, tapOwner="can",
tapOwnerUid=1000, accounts={vm-1..vm-8}}` and rekeys 3 agenix secrets
for the 8 Steam passwords. The flake input `steampipe` points at
codeberg, so the module's `nixosModules.default` is in scope.

The flake **does not** build login VM runners at the host level —
that's a project-level artifact today.

### Reproduction of the failure mode

```text
$ cargo run -- steam login --target all
error: unexpected argument '--target' found
  tip: to pass '--target' as a value, use '-- --target'

$ cargo run -- steam login
warning: /home/can/.config/steampipe/host.json uses schemaVersion 1; ...
--login-runners-dir is required when running outside a project root
```

Two separate issues:

- `--target` was never the documented flag; [src/ui/cli.rs:556-565](../../../src/ui/cli.rs#L556-L565)
  declares `target: Option<String>` positionally. Docs and tooling
  output (e.g. README:181) consistently show positional form. The
  user's typed form came from the previous assistant turn's guess.
  Annoying but trivial.
- The `loginRunnersDir` gap is **the load-bearing problem.** Even with
  correct positional syntax, the CLI fails because there is no path to
  resolve this field from either project root or host config.

### Schema-version note (orthogonal)

The user's on-disk `/etc/steampipe/host.json` and
`/etc/steampipe/module.json` are still `schemaVersion: 1`. The module
emits 2 ([nix/module.nix:228](../../../nix/module.nix#L228)); the
warning indicates the user has not run `nixos-rebuild switch` since the
schema bump. The CLI accepts both per [src/core/nixos_module.rs:21](../../../src/core/nixos_module.rs#L21)
(`SUPPORTED_SCHEMA_VERSIONS = &[1, 2]`). This is unrelated to the
config-less work, but a future schemaVersion bump will need a
migration story.

## Evidence Inventory

| Claim | Evidence | Proves |
|---|---|---|
| `--target` is not a flag | [src/ui/cli.rs:556-565](../../../src/ui/cli.rs#L556) | Positional `target: Option<String>`; no `#[arg(long)]` attribute. |
| `None` target equals `"all"` | [src/core/config.rs:1653-1654](../../../src/core/config.rs#L1653) | `match target { None | Some("all") => Ok(self.vms.clone()), ... }`. So `steam login` (no target) is already the bulk form. |
| Hard requirement on `loginRunnersDir` outside project root | [src/lib.rs:461-474](../../../src/lib.rs#L461-L474) | `require_login_runners_dir` bails when microvm backend + no project root + no flag. |
| Host config schema today | [docs/src/configuration/host-config.md:19-35](../../../docs/src/configuration/host-config.md#L19-L35) and [src/core/nixos_module.rs:51-77](../../../src/core/nixos_module.rs#L51-L77) | No `loginRunnersDir` field. |
| Module emits v2 but caller writes only the listed keys | [nix/module.nix:212-233](../../../nix/module.nix#L212-L233) | No `loginRunnersDir` key in the JSON literal. |
| Login runners are project-flake artifacts, not host-level | [nix/lib/test-cluster.nix:268-277](../../../nix/lib/test-cluster.nix#L268-L277) | `loginVMRunners = lib.mapAttrs (mkMicroVM flavors.default) loginVMs;` and `pkgs.linkFarm "cluster-login-vm-runners"` — built per project flake. |
| `sshPublicKey` is read from `projectRoot + ssh_key` | [nix/lib/test-cluster.nix:60-68](../../../nix/lib/test-cluster.nix#L60-L68) | Login runners encode the project's SSH key, blocking trivial host-level reuse. |
| `vmCount` already falls through to module | [src/lib.rs:634-641](../../../src/lib.rs#L634-L641) | `cli.vm_count.or_else(|| module.as_ref().map(|m| m.vm_count))` — proves the pattern; just needs replication for other fields. |
| canix on atlas wires `services.steampipe-cluster` | [canix/root/hosts/atlas/features/steampipe.nix:12-52](file:///data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix#L12) | `enable = true; vmCount = 8; accounts = {...}`. |
| canix consumes steampipe as a flake input | canix/flake.nix `inputs.steampipe.url = "git+https://codeberg.org/caniko/steampipe.git"` (shown above in transcript) | Steampipe flake outputs (including potential login-runner package) are reachable from canix. |
| Login runners need 4 GiB RAM and Sway/VNC | [nix/lib/test-cluster.nix:268](../../../nix/lib/test-cluster.nix#L268) and [src/game/steam.rs:606](../../../src/game/steam.rs#L606) | `memory: 4096` and interactive VNC login flow. |
| Smoke test of current schema | `cat /etc/steampipe/module.json` (in transcript) | Real fixture used by atlas right now. |

## Existing Plan Status

`docs/src/planning/` contains:

- `steam-session-longevity-research.md` and `steam-session-longevity-design.md`
  — both target Steam token persistence after login, **not** CLI UX. Out of scope.
- `steam-warm-timer/` — also unrelated.

**No existing plan covers the config-less CLI work.** Nothing to
consolidate or invalidate. New dossier stands alone.

## Work That Should Survive Into The Long-Term Plan

The work splits cleanly along three axes; the planner can decide how
to map them to phases.

### A. Make the host config the single source of truth

A1. Bump host config schema to **version 3** and add fields:
- `loginRunnersDir` (`path string | null`) — host-level pre-built
  login runner linkFarm.
- `sshKey` (`path string | null`) — private SSH key path used to
  enter the VMs.
- `vmUser` (`string | null`, defaults to `"cluster"`) — in-guest
  user that owns Steam state.
- Optional: `runnersDir` (general runners) for full config-less
  `steam check`/`warm`.

A2. Reader: extend [src/core/nixos_module.rs:51](../../../src/core/nixos_module.rs#L51)
`NixosModuleConfig`, keep `Option<…>` for backwards compatibility,
keep `SUPPORTED_SCHEMA_VERSIONS = &[1, 2, 3]` with deprecation
warning on `< 3`.

A3. Resolver layer: replace `require_login_runners_dir` and friends
with a uniform `resolve_*` helper (precedence: CLI flag > project
config > host config > error pointing to NixOS option name). Apply
to `loginRunnersDir`, `runnersDir`, `sshKey`, `vmUser`,
`credentialsPath` (already partly done at [src/lib.rs:687](../../../src/lib.rs#L687)).

### B. Publish login runners from the NixOS module

B1. Add `nixosModules.steampipe.options.services.steampipe-cluster.loginRunners`
sub-options (`enable`, `sshAuthorizedKey`, `vmUser` — mirrors
`mkMicroVM` inputs).

B2. New `nix/lib/login-runners.nix` (or refactor of
`test-cluster.nix:268-277`): build a host-level `pkgs.linkFarm` named
`steampipe-login-runners` keyed by vm-N, accepting `vmCount`,
`sshAuthorizedKey`, `vmUser`, `flavors.default` shape.

B3. Module wires it: when `services.steampipe-cluster.loginRunners.enable`,
the system has `/run/current-system/sw/share/steampipe/login-runners`
(or `/etc/steampipe/login-runners` symlink), and `module.json` gains
`"loginRunnersDir": "<store path or symlink>"`.

B4. canix-side change (consumer): atlas adds
`services.steampipe-cluster.loginRunners = { enable = true;
sshAuthorizedKey = "..."; };`. Confirms the host-level flow works
end to end.

### C. CLI UX cleanup

C1. Stop documenting `--target` (never existed); ensure README and
docs/src/configuration/steam-credentials.md show positional form
everywhere. Add a `steam login --target` alias? No — `clap`
positional + optional flag duplicates options. Reject the alias;
fix docs.

C2. `steam login` with no target already means "all" — reflect that
in the help text ("default: all") instead of the more obscure
"Target VM: 'all', '3', 'vm-3'".

C3. Error message overhaul: when `loginRunnersDir` cannot be
resolved, the bail should name the **NixOS option** to set
(`services.steampipe-cluster.loginRunners.enable = true`) and the
**flag fallback** in the same sentence. Same for `vmCount`,
`runnersDir`, `sshKey`.

C4. `steam info` should print `loginRunnersDir` source (which row
provided it: flag / project / host config / unset). Today
[src/lib.rs:855](../../../src/lib.rs#L855) already plumbs a discovery
trace — extend it.

C5. Consider folding `--vm-count`, `--credentials`,
`--login-runners-dir`, `--runners-dir` into a single
`--config <path>` for non-NixOS hosts (out of scope unless the user
asks; current XDG/etc discovery covers it).

## Blockers And Missing Artifacts

Two open design questions block phase decomposition. Both can be
resolved by the user with a single sentence.

| Blocker | Producer | Resolution path |
|---|---|---|
| **Where does the in-VM SSH public key live for host-level login runners?** The current login runners encode the project's `nix/test-cluster/cluster_key.pub` ([nix/lib/test-cluster.nix:60-68](../../../nix/lib/test-cluster.nix#L60)). For a host-level runner, the SSH key must either be (a) generated and managed by the NixOS module itself, (b) passed in as an option (`services.steampipe-cluster.loginRunners.sshAuthorizedKey`), or (c) declared per-project and the host-level runner refuses to build until one is provided. | User design call | Decide (a)/(b)/(c) before phase B2 starts. (b) is most flexible. |
| **Does the canix atlas host actually need both project-level AND host-level login runners coexisting?** Today chessbender (or other projects) ship login wrappers that bake in their own `loginVMRunnersDir`. If the host-level runner is the new default, the project wrappers become a redundant code path. Either deprecate the project wrappers or document them as a fallback. | User product call | Decide deprecation policy before phase C1 doc rewrite. |

Not blockers, but call-outs:

- Atlas's on-disk JSON is still schemaVersion 1 — irrelevant to the
  design, but `nixos-rebuild switch` should be run before any
  smoke-test of the new flow.

## Risks And Constraints

- **Schema bump compat.** SchemaVersion 3 must be additive
  (`Option<PathBuf>`), and the reader must accept v1/v2/v3 with
  warnings on the older two. Otherwise downstream projects that pin
  pre-bump steampipe break.
- **Login runner closure size.** Host-level runners pin a NixOS
  closure (Sway + VNC + Steam GUI) into the system. Atlas can absorb
  it; smaller hosts may not want it. Make the module option
  opt-in (`enable = false` by default).
- **SSH key drift.** If a host-level runner encodes one SSH key but
  the project flake encodes another, `cluster-ctl deploy` from inside
  a project root will work while host-level `steam login` from `$HOME`
  uses a different key. Must be explicit which is canonical. Cleanest:
  host config publishes the key path; project flakes that want
  custom keys override.
- **`steam login` is interactive.** Even with config-less invocation,
  it still serializes per-VM, requires VNC, and waits for Steam Guard.
  Don't market this work as "fully automated bulk login" — it's
  "config-less invocation of the existing sequential wizard."
- **Backward compat for project flakes.** `nix/lib/test-cluster.nix`
  consumers (chessbender, etc.) currently bake `--login-runners-dir`
  into wrappers. Those should keep working. The host-level path is
  additive.
- **NixOS-only.** Non-NixOS hosts (Mac, vanilla Linux) keep using
  `host.json` + `--login-runners-dir`. The dossier is explicit about
  this; don't accidentally regress that path.

## Candidate Phase Boundaries

A reasonable initial decomposition (planner may refine):

1. **Schema v3 + reader compat.** Add fields to `NixosModuleConfig`,
   bump `SUPPORTED_SCHEMA_VERSIONS`, update docs/host-config.md, add
   unit tests for v1/v2/v3 parsing. No behavior change yet — pure
   capacity. Dependencies: none. Routing: cheap (Sonnet/Haiku /
   GPT-5 medium).

2. **Resolver refactor.** Replace `require_login_runners_dir` /
   `require_runners_dir` with a single `resolve_optional` helper that
   walks CLI flag → project → host config and emits actionable errors
   naming the NixOS option. Wire `vmCount`, `loginRunnersDir`,
   `runnersDir`, `sshKey` through it. Dependencies: phase 1.
   Routing: medium (touches multiple call sites).

3. **NixOS module: host-level login-runners builder.** Add module
   option, extract the runner-building logic into
   `nix/lib/login-runners.nix`, wire it into `module.nix`. Dependencies:
   user resolves SSH key question. Routing: design-heavy (Opus / GPT-5
   high) — Nix lib design.

4. **canix consumer wiring + smoke test.** Set
   `services.steampipe-cluster.loginRunners.enable = true` on atlas
   in canix, `nixos-rebuild switch`, run `cluster-ctl steam login`
   from `$HOME`, capture trace output. Update
   [docs/src/configuration/host-config.md](../../../docs/src/configuration/host-config.md)
   walkthrough. Dependencies: phase 3.

5. **Docs + UX cleanup.** README/`steam-credentials.md`/help text:
   remove `--login-runners-dir` from primary recipes, document the
   NixOS option, fix any lingering `--target` references in chat
   surface area (none in repo, but the user's transcript shows this
   is a discovery problem). Dependencies: phase 4.
   Routing: cheap.

6. **Optional: deprecation of project-level wrappers.** If the user
   wants project wrappers to delegate to the host-level runner
   instead of building their own, this is a separate sweep across
   `nix/lib/test-cluster.nix` and downstream consumers (chessbender,
   etc.). Dependencies: phase 4. Routing: medium.

Phases 1, 2, 5 can be sub-layer parallelized if `multi-phase-dispatch`
applies (they touch disjoint files: schema reader/tests vs. resolver
in lib.rs vs. docs).

## Open Decisions For The User

1. **SSH-key source** for host-level login runners — see Blockers
   table. Recommend: NixOS module option
   `services.steampipe-cluster.loginRunners.sshAuthorizedKey` accepting
   a string or path.
2. **Deprecation policy** for project-level `cluster-steam-login`
   wrappers — see Blockers table. Recommend: keep both, document
   project wrappers as fallback for non-NixOS hosts.
3. **Schema bump to v3 vs. quietly add optional fields under v2.** A
   v3 bump is cleaner but forces another `nixos-rebuild` cycle for
   every consumer. Adding `Option<PathBuf>` fields under v2 works
   with the existing reader. Recommend: stay v2, add optional fields,
   only bump to v3 when a *behavioral* change forces it.
4. **Default for `services.steampipe-cluster.loginRunners.enable`.**
   `false` (opt-in, default off) avoids surprising existing
   consumers. `true` makes the config-less story default. Recommend:
   `false` initially, flip to `true` after one release cycle.

## Planner Handoff

- **Selected downstream skill:** `multi-phase-plan-mixed` (default).
  Cross-provider routing makes sense because Phase 3 needs design
  judgment while Phases 1, 2, 5 are mechanical.
- **Dossier path:** `docs/src/planning/cli-configless-from-nixos-research.md`
- **Current-state summary:** Steampipe CLI requires
  `--login-runners-dir` (and friends) outside a project root, even on
  NixOS hosts where canix has fully provisioned the cluster. The
  asymmetry is: `vmCount` correctly falls through from
  `/etc/steampipe/module.json` to `cluster-ctl`, but
  `loginRunnersDir` doesn't exist in the schema and isn't built at the
  NixOS-module level — login runners are project-flake artifacts.
- **Work that should become phases:** see "Candidate Phase Boundaries"
  above.
- **Known blockers to keep as blockers:** SSH-key source for
  host-level runners; deprecation policy for project wrappers. The
  planner should leave Phase 3 in `blocked` state until the user
  answers these.
- **Acceptance evidence the future phase set should preserve:**
  - `cluster-ctl steam login` from `$HOME` on atlas succeeds with no
    flags, no project root, no env.
  - `cluster-ctl steam info` from `$HOME` shows a `loginRunnersDir`
    row sourced from the discovered host config.
  - Existing chessbender (project-flake) wrappers still work
    unchanged.
  - `cluster-ctl steam login --login-runners-dir <path>` still
    overrides the host config (precedence preserved).
  - Schema parsing tests for the older schemaVersion(s) still pass.
