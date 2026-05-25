---
title: Nix input install gaps — Research Dossier
---

# Nix input install gaps — Research Dossier

## Goal And Trigger

The user explicitly flagged that consuming `steampipe` as a flake input
leaves the host in an incomplete state — the example given was that
the `cluster-ctl` CLI itself never lands on the host even when
`services.steampipe-cluster.enable = true`. They want a survey of
every analogous gap in the "use steampipe as a NixOS module" install
method so the install story can be finished off without playing
whack-a-mole.

Scope: the gap survey covers the flake outputs at
[flake.nix:282-321](../../../flake.nix#L282-L321) (the `nixosModules`,
`lib`, and `overlays` outputs), the host-side NixOS module at
[nix/module.nix](../../../nix/module.nix), the project-flake helper at
[nix/lib/test-cluster.nix](../../../nix/lib/test-cluster.nix), and the
documentation at
[docs/src/getting-started/installation.md](../../../docs/src/getting-started/installation.md)
and [docs/src/configuration/host-config.md](../../../docs/src/configuration/host-config.md).

Out of scope: anything the existing
[cli-configless-from-nixos-research.md](./cli-configless-from-nixos-research.md)
dossier (and its
[phase set](./cli-configless-from-nixos/README.md)) already owns —
i.e. `loginRunnersDir` schema fields, the CLI resolver refactor, the
`loginRunners.enable` module wiring, and the migration of project
wrappers. Those gaps were already audited; this dossier picks up the
remaining "make the flake-input install actually work end to end" gaps.

## Current Reality

### The install paths today

A consumer can pick one of three install paths and each one has a
different completeness story:

| Path | What you get | What's missing |
|---|---|---|
| `nix run github:caniko/steampipe -- ...` | The binary, ad-hoc invocation. | No host module, no networking, no login runners, no completions, no warm timer. Fine for trying it out. |
| `inputs.steampipe.url + steampipe.packages.${system}.default` in `environment.systemPackages` | Binary on PATH. | Operator must wire `services.steampipe-cluster.*` separately. Most operators won't realise the module exists. |
| `inputs.steampipe.url + steampipe.nixosModules.default` | Bridge, TAPs, NAT, credentials TOML, login-state dirs, login-runner keypair, optional login runners. | **The `cluster-ctl` binary itself.** Plus several other gaps enumerated below. |

The asymmetry is the issue: the module wires every piece of the
runtime *infrastructure* but never installs the *tool that uses it*.
Operators who go all-in on the module discover this only when they
type `cluster-ctl` and get "command not found".

### Confirmed gap list

#### G1 — `cluster-ctl` not installed by the module

[nix/module.nix:275-401](../../../nix/module.nix#L275-L401) writes
`/etc/steampipe/module.json`, creates tmpfiles, sets up
NetworkManager profiles, the SSH keypair activation script, the
credentials generator, the login-runners linkfarm, and the firewall
rule. It never touches `environment.systemPackages`.

Evidence: `grep -n "environment.systemPackages" nix/module.nix` →
no matches. The `cluster-ctl` derivation is reachable in scope only
because [flake.nix:34-40](../../../flake.nix#L34-L40) re-imports the
module with `_module.args.steampipe = self`, but the module never
adds `steampipe.packages.${pkgs.system}.default` to systemPackages.

Consumer evidence: atlas's
[canix/root/hosts/atlas/default.nix:31-34](file:///data/nvme0/can/Projects/canix/root/hosts/atlas/default.nix#L31)
has `environment.systemPackages = [ copr-cli ];` — no `cluster-ctl`.
The user has been running `cargo run --` from a working tree as the
existing
[cli-configless-from-nixos-research.md](./cli-configless-from-nixos-research.md)
transcript records, because the host doesn't expose the binary.

#### G2 — `services.steampipe-cluster.runners.enable` doesn't exist

The CLI bails with errors that name this option, but the module does
not declare it.

- CLI references:
  [src/lib.rs:490](../../../src/lib.rs#L490),
  [src/lib.rs:852](../../../src/lib.rs#L852),
  [src/lib.rs:872](../../../src/lib.rs#L872),
  [src/lib.rs:1488](../../../src/lib.rs#L1488).
  Each one renders a bail string of the form `Either pass
  --runners-dir ... or set services.steampipe-cluster.runners.enable
  in the NixOS module`.
- CLI test fixture asserts the JSON includes `runnersDir`:
  [src/core/nixos_module.rs:328](../../../src/core/nixos_module.rs#L328).
- Docs claim it works:
  [docs/src/configuration/host-config.md:52-56](../../../docs/src/configuration/host-config.md#L52-L56)
  describes the field as "Populated by the NixOS module when
  `services.steampipe-cluster.runners.enable = true`."
- Module reality: `grep -n "runners\.enable\|runnersDir" nix/module.nix`
  → no matches. Only `loginRunners.enable` exists; the regular
  (non-login) runners are still project-flake artifacts built via
  `mkRunnersDir` in
  [nix/lib/test-cluster.nix:162-171](../../../nix/lib/test-cluster.nix#L162-L171).

So `cluster-ctl steam check`, `cluster-ctl steam warm`, and
`cluster-ctl up` from outside a project root all bail telling the
operator to set an option that does not exist. Phase 06 of the
config-less plan
([planning/cli-configless-from-nixos/06-migrate-project-wrappers.md:48-51](./cli-configless-from-nixos/06-migrate-project-wrappers.md#L48))
calls this out as "a future phase" but no phase has been written for
it.

#### G3 — `warmTimerModule` is only reachable through `mkTestCluster`

The warm-timer module is emitted as a return value of `mkTestCluster`
at
[nix/lib/test-cluster.nix:359-364](../../../nix/lib/test-cluster.nix#L359-L364)
— and `mkTestCluster` requires `projectRoot`, `clusterCtl`,
`projectConfig`, and a `steampipe.toml`. A host that wires only
`steampipe.nixosModules.default` (no project flake) cannot enable the
warm timer:

- The flake's host-level `nixosModules` output
  ([flake.nix:282-296](../../../flake.nix#L282-L296)) does not expose
  `warmTimer` as a top-level NixOS module.
- The host-level `module.nix` does not even import
  `nix/lib/warm-timer.nix`.
- Docs at
  [docs/src/configuration/host-config.md:128-147](../../../docs/src/configuration/host-config.md#L128-L147)
  walk through `steampipeCluster.warmTimerModule`, which only exists
  as `(mkTestCluster {...}).warmTimerModule` — so the doc snippet is
  impossible to follow without a project flake.

`warm-timer.nix` itself
([nix/lib/warm-timer.nix:1-7](../../../nix/lib/warm-timer.nix#L1-L7))
takes `clusterCtl`, `vmRunnersDir`, `vmCount`, `defaultUser`,
`defaultSshKey` as injected arguments. Two of those — `vmRunnersDir`
and `defaultSshKey` — are currently project-flake artifacts. The
warm timer therefore cannot run host-only until G2 (`runners.enable`)
also lands and publishes a host-level runners dir + the SSH key path
from `module.json`.

#### G4 — `overlays.default` is not auto-applied by the module

[flake.nix:315-481](../../../flake.nix#L315-L481) composes
`microvm.overlay` with two consequential local overrides:

- `cloud-hypervisor-graphics` cargoDeps hash pin (otherwise the
  microvm.nix consumer hits `Cargo.lock is not the same in
  /build/cargo-deps-vendor` when nixos-unstable drifts ahead of
  spectrum's pin).
- `crosvm` postPatch + postInstall to widen seccomp policy and
  install `.bpf` files (otherwise virtio-gpu / Steam GUI dies with
  `SIGSYS` on Linux ≥ 6.13 + glibc ≥ 2.40).

`loginRunners.enable = true` builds login VMs with `hypervisor =
"crosvm"; graphics = true;` by default
([nix/module.nix:255-271](../../../nix/module.nix#L255-L271)), so the
crosvm patch is load-bearing. But the module itself never sets
`nixpkgs.overlays`. The overlay is only applied indirectly through
[nix/lib/microvm-runner.nix:24-25](../../../nix/lib/microvm-runner.nix#L24-L25)
— inside the per-VM NixOS instantiation. That covers the *guest*
side. The *host* still uses whichever `pkgs.crosvm` /
`pkgs.cloud-hypervisor-graphics` the consumer's outer nixpkgs gives,
which on most consumers is the unpatched upstream.

Concretely: a NixOS host that has `services.steampipe-cluster.enable
= true` but does not also wire
`nixpkgs.overlays = [ inputs.steampipe.overlays.default ];` will see
the login VMs build (their inner nixpkgs is patched) but any
host-side `crosvm` invocation — including future `cluster-ctl`
commands that run `crosvm` from `PATH` — will hit the unpatched
binary.

No assertion warns about this; the failure is at runtime when Steam
GUI inside the login VM dies with `SIGSYS`.

#### G5 — microvm host module not imported by `nixosModules.default`

Login runners are pre-built `microvm-run` wrappers. They expect the
host to have `microvm.host.enable = true` from microvm.nix's host
module. The flake re-exports `microvm.nixosModules` at
[flake.nix:282](../../../flake.nix#L282), so a consumer can do
`imports = [ inputs.steampipe.nixosModules.host ];`, but
`nixosModules.default` does not import it. There is also no
assertion at
[nix/module.nix:276-289](../../../nix/module.nix#L276-L289)
that warns when `loginRunners.enable = true` is set without the
microvm host module — the boot just silently fails to find
`microvm@vm-N.service` units.

Concretely on atlas: the atlas host config sets `loginRunners.enable
= true` but [no `microvm.host.enable = true`
import](file:///data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix)
is visible in the steampipe feature file. Either it's imported
elsewhere in canix, or login runners would never actually start. The
ambiguity is the gap — the consumer has no way to know from the
module's option surface that they need microvm.host wired separately.

#### G6 — `installation.md` doesn't mention the NixOS module path

[docs/src/getting-started/installation.md](../../../docs/src/getting-started/installation.md)
covers only `nix run`, `nix profile`-style `packages.${system}.default`,
and `cargo install`. It does not mention `nixosModules.default`,
`services.steampipe-cluster.*`, the overlay, or
`lib.mkTestCluster`. A consumer following the install page literally
will get a binary but no infrastructure. The host-config page at
[docs/src/configuration/host-config.md:191-253](../../../docs/src/configuration/host-config.md#L191-L253)
*does* show the module, but only as an example for getting login
runners — it doesn't bill itself as the install path.

Net effect: there is no doc that says "on NixOS, this is the
end-to-end install: flake input + module import + systemPackages +
overlay + microvm host."

#### G7 — Shell completions not installed by the module

`cluster-ctl completions {bash,zsh,fish}` is documented in the README
([README.md:843-854](../../../README.md#L843-L854)) but operators
have to invoke it by hand. The NixOS module has perfect knowledge of
the binary path; it could install completions to
`/run/current-system/sw/share/{bash-completion,zsh/site-functions,fish/vendor_completions.d}`
automatically. None of that happens today.

This is the same "binary present but harness not wired" pattern as
G1 — fixing G1 by adding `cluster-ctl` to systemPackages opens the
door to fixing G7 by enabling
`programs.{bash,zsh,fish}.enableCompletion`-aware completion install.

#### G8 — Module surface mentions runtime concepts the consumer can't shape

[nix/module.nix:120-273](../../../nix/module.nix#L120-L273) declares
options for `vmCount`, `bridge`, `subnet`, `prefix`, `hostIp`,
`natInterface` (deprecated), `tapOwner`, `tapOwnerUid`, `accounts`,
`loginStateDir`, and `loginRunners.{enable,sshAuthorizedKey,hypervisor,graphics,memory}`.

What's missing as configurable knobs on the module side:

- `vmUser` — hardcoded to `"cluster"` at
  [nix/module.nix:12](../../../nix/module.nix#L12). Project flakes
  can set their own `vmUser` in `steampipe.toml`
  ([nix/lib/test-cluster.nix:53](../../../nix/lib/test-cluster.nix#L53)),
  but host-level login runners always use `"cluster"`. A consumer
  whose project uses a different in-VM user (e.g. `chessbender`) has
  a mismatch — the login runner boots `cluster`, the project test
  cluster boots `chessbender`, and the SSH key is keyed to whichever
  ran most recently.
- `package` option — to swap the `cluster-ctl` derivation (e.g.
  pin to a fork or a debug build) without rebuilding the module
  graph.
- `runners.enable` (see G2) — the symmetric option for non-login
  runners.
- `package.completions.enable` (see G7).

These are not currently broken, but they are *future* gaps: once G1
and G2 land, `vmUser` will be the next thing operators try to set
and discover doesn't exist.

#### G9 — `pname = "cluster-ctl"`, repo name `steampipe`, module name `services.steampipe-cluster`

Three different names for one thing: `nix run github:caniko/steampipe`
produces `bin/cluster-ctl`; the module is `services.steampipe-cluster`;
the flake input is conventionally `inputs.steampipe`. The mismatch is
not technically a gap, but it means `which cluster-ctl` doesn't tell
the operator which derivation produced it, and `nixos-rebuild
list-generations | grep steampipe` doesn't surface the binary's
generation. Documenting the mapping (or aligning the names — likely
out of scope) would reduce confusion.

#### G10 — No `lib.mkTestCluster` default for `clusterCtl`

[nix/lib/test-cluster.nix:19-22](../../../nix/lib/test-cluster.nix#L19-L22)
requires `clusterCtl` as an explicit argument. Every consumer has to
pass `steampipe.packages.${system}.default` themselves. Defaulting
this to `steampipe.packages.${pkgs.system}.default` would close one
small footgun: forgetting the argument fails with a Nix evaluation
error that doesn't name the missing input clearly.

## Evidence Inventory

| Claim | Evidence | Proves |
|---|---|---|
| Module never installs `cluster-ctl` | `grep -n "environment.systemPackages" nix/module.nix` returns no matches | G1 |
| Atlas does not install `cluster-ctl` manually either | [canix/root/hosts/atlas/default.nix:31-34](file:///data/nvme0/can/Projects/canix/root/hosts/atlas/default.nix#L31) | G1 — consumer has to do extra work that they haven't done |
| CLI bails referring to nonexistent option | [src/lib.rs:472-474,490](../../../src/lib.rs#L472-L490) | G2 — error message names `services.steampipe-cluster.runners.enable` |
| Module declares only `loginRunners`, not `runners` | [nix/module.nix:120-273](../../../nix/module.nix#L120-L273) | G2 |
| Docs claim `runners.enable` exists | [docs/src/configuration/host-config.md:52-56](../../../docs/src/configuration/host-config.md#L52-L56) | G2 — doc/module mismatch |
| Phase plan flags the gap but defers | [planning/cli-configless-from-nixos/06-migrate-project-wrappers.md:48-51](./cli-configless-from-nixos/06-migrate-project-wrappers.md#L48) | G2 is known, not assigned |
| `warmTimerModule` only emitted by `mkTestCluster` | [nix/lib/test-cluster.nix:359-364](../../../nix/lib/test-cluster.nix#L359-L364) and [flake.nix:282-296](../../../flake.nix#L282-L296) | G3 |
| `warm-timer.nix` takes `vmRunnersDir`, `defaultSshKey` as injected args | [nix/lib/warm-timer.nix:1-7](../../../nix/lib/warm-timer.nix#L1-L7) | G3 — depends on G2 to even be reachable from a bare host |
| Overlay patches `crosvm` for kernel 6.13+ seccomp | [flake.nix:336-481](../../../flake.nix#L336-L481) | G4 — patch is load-bearing |
| Overlay applied only inside per-VM nixpkgs, not host | [nix/lib/microvm-runner.nix:24-25](../../../nix/lib/microvm-runner.nix#L24-L25) vs. no match in `nix/module.nix` | G4 |
| `microvm.nixosModules` re-exported but `default` doesn't import host module | [flake.nix:282-296](../../../flake.nix#L282-L296) and `grep` of `nix/module.nix` | G5 |
| No assertion about microvm.host | [nix/module.nix:276-289](../../../nix/module.nix#L276-L289) | G5 |
| Installation page has no NixOS-module section | [docs/src/getting-started/installation.md:1-39](../../../docs/src/getting-started/installation.md) | G6 |
| `vmUser` hardcoded in module | [nix/module.nix:12](../../../nix/module.nix#L12) | G8 — config knob missing |
| `mkTestCluster` requires explicit `clusterCtl` | [nix/lib/test-cluster.nix:19-22](../../../nix/lib/test-cluster.nix#L19-L22) | G10 |

## Existing Plan Status

The active plan set is
[planning/cli-configless-from-nixos/](./cli-configless-from-nixos/README.md)
(6 phases, last updated for the login-runners landing). Status of
each phase versus the gaps in this dossier:

| Phase | Status | Relevance to nix-input-install gaps |
|---|---|---|
| 01 Host config schema fields | shipped (schema 2 emits `loginRunnersDir`, `sshKey`, `vmUser`; module emits per [nix/module.nix:331-359](../../../nix/module.nix#L331-L359)) | Adds the *fields* but not the `runners.enable` option (G2) |
| 02 CLI resolver refactor | shipped (resolver in [src/lib.rs:478-492](../../../src/lib.rs#L478-L492)) | Resolver references `runners.enable` which still doesn't exist — the dangling reference is G2 |
| 03 NixOS module login runners | shipped ([nix/module.nix:219-272](../../../nix/module.nix#L219-L272) declares `loginRunners.*`; module emits `loginRunnersDir` when enabled) | Closes the login-runner half. Does not close G2 (regular runners) |
| 04 canix wiring + smoke | shipped (atlas sets `loginRunners.enable = true` per `canix/root/hosts/atlas/features/steampipe.nix:18`) | Confirms G1 in practice — atlas works for `steam login` but the operator still has no `cluster-ctl` on PATH |
| 05 Docs + UX cleanup | shipped (host-config.md walkthrough at [docs/src/configuration/host-config.md:191-253](../../../docs/src/configuration/host-config.md#L191-L253) is current) | Does not cover G6 (installation.md still has no module section) |
| 06 Migrate project wrappers | done in wrapper logic ([nix/lib/test-cluster.nix:278-306](../../../nix/lib/test-cluster.nix#L278-L306)) | Explicitly defers G2 (`runners.enable`) as "a future phase" |

Net: the existing plan closed the login-runners install path. The
"regular runners + binary install + completions + overlay +
host-imports + warm timer at host level" install paths are all still
open. Nothing in the existing plan set covers G1, G2, G3, G4, G5,
G6, G7, G8, G9, or G10 directly.

## Work That Should Survive Into The Long-Term Plan

Two natural groupings emerge.

### Track A — Finish the install paradigm

A1. **Install `cluster-ctl` from the module.** Add `package =
mkOption { type = types.package; default = steampipe.packages.${pkgs.system}.default; }`
to `nix/module.nix`, then `environment.systemPackages = [ cfg.package
];` inside the `config = mkIf cfg.enable { ... }` block. Fixes G1.

A2. **Apply `overlays.default` from the module.** Inside the same
`mkIf cfg.enable` block, set `nixpkgs.overlays = [
steampipe.overlays.default ];`. Makes host-side crosvm / cloud-hypervisor-graphics
match the patched versions used inside login VMs. Fixes G4.

A3. **Import microvm host module conditionally.** When
`loginRunners.enable = true`, `imports = [ microvm.nixosModules.host
];` or assert that the consumer has imported it. Module already has
`microvm` in scope via `_module.args` (passed by
[flake.nix:34-40](../../../flake.nix#L34-L40))? **Actually no** —
the module only receives `steampipe` and `nixpkgs`, not `microvm`.
Bumping the `_module.args` payload to include `microvm` is the
prereq. Closes G5.

A4. **Install shell completions.** Use `pkgs.installShellFiles` or
the dedicated `programs.{bash,zsh,fish}.enableCompletion` paths to
make `cluster-ctl completions <shell>` output land in the system
shell completion dirs. Fixes G7.

A5. **Add the `package`, `vmUser`, and `completions.enable`
options.** Once A1 + A4 land, expose the knobs operators will want.
Closes G8 partially (the `runners.package` knob waits on Track B).

### Track B — Symmetric `runners.enable` option

B1. **Extract `nix/lib/microvm-runner.nix`-driven runner build into
host scope.** Today [nix/lib/login-runners.nix](../../../nix/lib/login-runners.nix)
is the host-level counterpart to the project-flake
`mkRunnersDir` in
[nix/lib/test-cluster.nix:162-171](../../../nix/lib/test-cluster.nix#L162-L171).
Mint a parallel `nix/lib/host-runners.nix` (or rename
`login-runners.nix` and parameterise it) that builds *non-login* VM
runners — same vmCount, same SSH key handling, but with default RAM
(2048 MiB) and no GUI overhead.

B2. **Declare `services.steampipe-cluster.runners` sub-options.**
Mirror the `loginRunners` block in
[nix/module.nix:219-272](../../../nix/module.nix#L219-L272):
`enable`, `hypervisor`, `graphics`, `memory`, `extraVmOverlays`,
`extraVmModules`. When enabled, write `runnersDir` into
`module.json` and link-farm them at
`/etc/steampipe/runners`.

B3. **Update the CLI bail messages, the CLI resolver test fixture
([src/core/nixos_module.rs:328](../../../src/core/nixos_module.rs#L328)),
and the docs at
[docs/src/configuration/host-config.md:52-56](../../../docs/src/configuration/host-config.md#L52-L56)
to reflect the now-real option.** Closes G2.

B4. **Surface `warmTimerModule` as a flake-level
`nixosModules.warmTimer` that consumes Track B's host-published
artifacts.** Refactor `nix/lib/warm-timer.nix` so it reads from
`config.services.steampipe-cluster` (vmCount, runnersDir, sshKey,
vmUser) instead of injected arguments. Add `nixosModules.warmTimer =
import ./nix/modules/warm-timer.nix;` to
[flake.nix:282-296](../../../flake.nix#L282-L296). Closes G3.

B5. **Default `clusterCtl` in `lib.mkTestCluster`.** Set `clusterCtl
? steampipe.packages.${pkgs.system}.default` so the argument becomes
optional. Closes G10.

### Track C — Documentation

C1. **Rewrite `installation.md` to cover the NixOS module path.** Add
a "Recommended: NixOS module install" section that walks through
flake input + `imports = [ inputs.steampipe.nixosModules.default ];`
+ `services.steampipe-cluster.enable = true;` + the optional
`loginRunners.enable` / `runners.enable` / warm timer toggles. Mention
the overlay applies automatically once A2 lands. Closes G6.

C2. **Cross-link `host-config.md` to the new installation section.**
Remove the now-impossible `steampipeCluster.warmTimerModule` snippet
([docs/src/configuration/host-config.md:128-147](../../../docs/src/configuration/host-config.md#L128-L147))
and replace it with the new `services.steampipe-warm-timer.enable =
true;` flow that Track B4 enables. Touches G3 docs surface.

C3. **Pname/repo/module mapping note.** Single paragraph in
installation.md clarifying that the flake is called `steampipe`, the
binary is `cluster-ctl`, the module is `services.steampipe-cluster`.
Closes G9 (documentation-only fix; renaming is out of scope).

## Blockers And Missing Artifacts

Three open design decisions block phase assignment. Each can be
resolved with a one-sentence user call.

| Blocker | Producer | Resolution path |
|---|---|---|
| **G2/B2 ownership of `runners.enable` SSH key.** The login-runner path uses a host-generated keypair under `/var/lib/steampipe/ssh/cluster_key`. Regular runners need the same trust model — but most consumers' *test* clusters use the *project's* SSH key (per [nix/lib/test-cluster.nix:60-68](../../../nix/lib/test-cluster.nix#L60-L68)) so deploys can land. Decide whether `runners.enable` uses the host keypair (consistent with login runners; breaks chessbender's deploy path until chessbender adopts the host key) or accepts an `sshAuthorizedKey` override (consistent with login runners' option shape but lets project keys leak into the host module). | User design call | Recommend the override path. |
| **G3/B4 default for host-level warm timer when no host-level runners exist.** If the operator enables `services.steampipe-warm-timer.enable = true` but never `runners.enable = true`, the timer has nothing to run against. Should B4 assert, or silently disable, or fall back to a flag? | User product call | Recommend assert — silent disable masks the misconfiguration. |
| **G5 microvm host-module import strategy.** Two options: (a) `nixosModules.default` always imports `microvm.nixosModules.host`, pulling the dependency in transitively. (b) `nixosModules.default` only asserts and the consumer must import it themselves. (a) is easier for operators but couples our module to microvm.nix's host-side module surface; (b) is more explicit but creates the silent-failure mode that's the gap today. | User design call | Recommend (a) under a `loginRunners.enable` or `runners.enable` `mkIf`, so non-VM consumers don't get the dependency. |

Not blockers but call-outs:

- The flake `pname` rename (G9 → make `pname = "steampipe"`?) is
  potentially user-visible and breaks any consumer that does
  `steampipe.packages.${system}.cluster-ctl`. Documented-only fix is
  safer.
- Atlas needs to add `environment.systemPackages = [
  inputs.steampipe.packages.${system}.default ];` *today* as a
  one-line manual install, independently of A1, so the user stops
  having to `cargo run --`. That is one-line scope and could be done
  in a single commit while the larger track A work is sequenced.

## Risks And Constraints

- **Backward compat for downstream consumers of `mkTestCluster`.**
  Phases that rename `loginVMRunnersDir`, change the warm-timer
  argument shape, or default `clusterCtl` must keep the existing
  positional argument set working. chessbender and any other
  in-the-wild flakes will break otherwise.
- **`nixpkgs.overlays` ordering.** If A2 sets `nixpkgs.overlays = [
  steampipe.overlays.default ]` unconditionally, it composes ahead
  of any consumer-provided overlay that *also* touches `crosvm` or
  `cloud-hypervisor-graphics`. Use `mkBefore` or `mkAfter` carefully,
  and document which side wins.
- **Closure size of host-level runners.** B1 publishing
  `/etc/steampipe/runners` adds 7×~1 GiB of microvm closure to every
  host that enables it. Make `runners.enable` opt-in (`default =
  false`) and document the size cost.
- **microvm.host import as transitive dependency.** Pulling in
  `microvm.nixosModules.host` from `nixosModules.default` means
  consumers can no longer choose a different microvm host
  implementation. Acceptable if we make it conditional on
  `loginRunners.enable || runners.enable`.
- **The `vmUser` mismatch with project flakes.** When B1 publishes
  host-level runners with `vmUser = "cluster"` and a project's
  `steampipe.toml` says `vm_user = "chessbender"`, deploys to those
  runners will hit `Permission denied` on the home directory.
  Document that project-level runners override host-level when
  available; the resolver layer at
  [src/lib.rs:478-492](../../../src/lib.rs#L478-L492) is the
  enforcement point.
- **CLI's "service" option strings appear in user-visible errors.**
  Renaming `services.steampipe-cluster.runners.enable` later (e.g.
  to `services.steampipe-cluster.runners`) will silently desync from
  the bail messages until they're updated. Cover with a unit test
  that asserts the option string exists in `nix/module.nix`'s option
  surface — closes the dangling-reference bug class.
- **Shell completion install hooks vary by distro.** A4 is
  trivial on NixOS, but the README claims `cluster-ctl` "should work
  on any distro with the dependencies below"
  ([README.md:46-47](../../../README.md#L46-L47)). The module's
  completion install only helps NixOS hosts; non-NixOS consumers
  still need the manual snippet.

## Candidate Phase Boundaries

A reasonable initial decomposition. Tracks A and B can interleave;
track C blocks on at least A1+A2.

1. **Quick win: install `cluster-ctl` + completions from the
   module.** A1 + A4 + A5 (`package`/`completions.enable` options).
   No design questions, no schema changes. Dependencies: none.
   Routing: cheap (Sonnet / GPT-5 medium).

2. **Overlay application + microvm host wiring.** A2 + A3. Touches
   `_module.args` plumbing in
   [flake.nix:34-40](../../../flake.nix#L34-L40) to add `microvm`,
   then sets `nixpkgs.overlays` and conditional imports. Some
   design judgment around `mkBefore`/`mkAfter` and the `loginRunners
   || runners` conditional import. Dependencies: Blocker G5
   resolved. Routing: design-touch (Opus / GPT-5 high).

3. **Symmetric `runners.enable` option (host-level regular runners).**
   B1 + B2 + B3. Extract a parameterisable runner builder, declare
   the sub-options, emit `runnersDir`, update CLI bail strings and
   the test fixture, refresh
   [docs/src/configuration/host-config.md](../../../docs/src/configuration/host-config.md).
   Dependencies: Blocker G2 SSH-key decision. Routing: design-heavy
   (Opus / GPT-5 high).

4. **Host-level warm timer.** B4. Refactor `warm-timer.nix` to read
   from `config.services.steampipe-cluster`, add
   `nixosModules.warmTimer` flake output, update the host-config
   walkthrough. Dependencies: phase 3 (publishes the runner dir +
   SSH key the timer needs). Routing: medium.

5. **`lib.mkTestCluster` default for `clusterCtl`.** B5. Single
   `clusterCtl ? steampipe.packages.${pkgs.system}.default` change
   plus a smoke against a project flake. Dependencies: none
   (independent of the module work). Routing: cheap.

6. **Installation docs rewrite + pname/repo/module mapping.** C1 +
   C2 + C3. Adds the missing NixOS-module section, removes the
   impossible warm-timer snippet, documents the name mapping.
   Dependencies: phases 1, 2, 4 (so docs reference real options).
   Routing: cheap.

7. **Optional: atlas-side one-liner install of `cluster-ctl`.** A
   single-commit canix change adding `environment.systemPackages = [
   inputs.steampipe.packages.${system}.default ];` to
   [canix/root/hosts/atlas/features/steampipe.nix](file:///data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix).
   Independent of all phases above — unblocks the user immediately.
   Routing: trivial.

Phases 1, 5, and 7 can land in parallel — they touch disjoint files.
Phase 2 should land before phase 3 so the host-side crosvm patch is
in place when host-level runners start being exercised.

## Open Decisions For The User

1. **`runners.enable` SSH key model** — see Blockers G2/B2.
   Recommend: `sshAuthorizedKey` option, defaulting to the
   host-generated `cluster_key.pub` (same model as `loginRunners`).
2. **microvm host module import strategy** — see Blockers G5.
   Recommend: (a) auto-import under `mkIf (loginRunners.enable ||
   runners.enable)`.
3. **Warm timer fallback when `runners.enable = false`** — see
   Blockers G3/B4. Recommend: assert at eval time.
4. **`pname` rename for the binary derivation** — recommend keep
   `cluster-ctl` (rename breaks every flake that names the
   derivation), document the mapping instead.
5. **Atlas hot-fix vs. wait for phase 1.** The user could land
   phase 7 (single line in canix) right now to get `cluster-ctl` on
   their PATH while the larger track A work is sequenced. Confirm
   whether they want the one-liner or prefer to wait.

## Planner Handoff

- **Selected downstream skill:** `multi-phase-plan-mixed` (default
  for this repo per the existing dossier convention). Phase 1, 5, 6,
  7 are mechanical; phases 2, 3, 4 need design judgment.
- **Dossier path:**
  `docs/src/planning/nix-input-install-gaps-research.md`.
- **Current-state summary:** The `nixosModules.default` install path
  wires runtime infrastructure (bridge, TAPs, NAT, login runners,
  credentials, SSH keypair) but never installs the `cluster-ctl`
  binary, never applies the host overlay, never imports the microvm
  host module, never installs shell completions, never exposes the
  warm-timer module at the host level, and references a
  `services.steampipe-cluster.runners.enable` option that does not
  exist. Documentation for the NixOS-module install path is absent
  from `installation.md`.
- **Work that should become phases:** see "Candidate Phase Boundaries"
  above.
- **Known blockers to keep as blockers:** SSH-key model for
  `runners.enable`; microvm host import strategy; warm-timer
  fallback policy. The planner should leave phases 2, 3, 4 in
  `blocked` state until the user answers.
- **Acceptance evidence the future phase set should preserve:**
  - On atlas (post-`nixos-rebuild switch`), `which cluster-ctl`
    returns a `/run/current-system/sw/bin/cluster-ctl` path.
  - `cluster-ctl --help` works without any flake inputs in scope.
  - `cluster-ctl completions bash | head` works and bash completion
    triggers in interactive shells.
  - `cluster-ctl steam check` from `$HOME` on atlas no longer bails
    with "set `services.steampipe-cluster.runners.enable`" — the
    option exists and the runners directory is published.
  - `services.steampipe-warm-timer.enable = true;` on atlas does
    not require importing `mkTestCluster`; the option is reachable
    from `steampipe.nixosModules.warmTimer` (or `.default` if
    folded in).
  - Host-side `crosvm --help` runs from the patched derivation, not
    upstream; `nix-store -q --references $(readlink
    /run/current-system/sw/bin/crosvm)` should show the steampipe
    overlay's hash.
  - `installation.md` has a NixOS-module section that walks from
    flake input to running cluster in under 10 steps.
  - Existing chessbender consumer still builds and `nix run
    .#cluster-steam-login` succeeds (backwards compat preserved).
