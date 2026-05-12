# VM Scheduling Flexibility Audit

Date: 2026-05-11

Phase A audits assumptions that VM names are exactly `vm-1, vm-2, ..., vm-N`
for the requested count, and records the allocation contract for the follow-up
implementation phases.

## Assumption Inventory

| Site | Category | Assumption | Blast radius if non-contiguous IDs are allocated |
| --- | --- | --- | --- |
| `src/core/config.rs:1262-1275` | Generator | `ClusterConfig::load` clamps `vm_count` to `max_vms`, then builds `self.vms` from `1..=vm_count` with `vm-{i}`, `subnet.i`, and `index = i`. | Any command that uses `config.vms` before leasing sees only the prefix set. A later lease can reserve `vm-7`, while target resolution, deploy/test loops, and wrapper defaults still think the cluster is `vm-1` only. |
| `src/core/config.rs:1446-1460` | Lease-derived generator | `leased_vms()` reconstructs VM definitions from claim IDs and `subnet.id`; this supports non-contiguous IDs but still assumes the VM name/IP convention is a pure function of the slot ID. | Flexible allocation works only for uniform slot definitions. Per-VM overrides keyed by `[[vm]] index = N` need to be merged here or in the post-lease derivation path, otherwise reserved slots lose their configured resources. |
| `src/core/config.rs:1469-1482` | Consumer expecting contiguous request list | `resolve_targets(None | "all")` returns `self.vms`, and `--continue-from` filters `self.vms` by `index >= target.index`. | Commands using targets before/without leases operate on `vm-1..vm-count`, not "currently claimed IDs"; after flexible allocation, `all` should mean chosen/leased VMs for lifecycle-sensitive commands. |
| `src/core/config.rs:1472-1474` | Display/error contract | Unknown target error computes max from `self.vms.len()` and prints `expected vm-1..vm-{max} or 1..{max}`. | If `self.vms = [vm-3]`, the error incorrectly says `vm-1` is valid and hides the chosen slot set from operators. |
| `src/core/config.rs:1494-1501` | Test helper generator | `ClusterConfig::for_test(vm_count)` always creates `vm-1..vm-count`. | Existing tests cannot assert gaps like `[2, 4]` without a new helper; flexible allocation regressions may be missed. |
| `src/vm/lease.rs:51-91` | Allocator | `reserve_n` scans `1..=max_vms`, skips held slots, and returns the first `count` free IDs in ascending order. | This already implements "any free VM" but the docstring should explicitly drop contiguity guarantees; callers must not assume the returned IDs are `1..=count`. |
| `src/vm/lease.rs:116-126` | Lease scanner | `list_cluster_vms` scans `1..=max_vms` and returns alive claims for the cluster. | Down/status-style commands can correctly discover non-contiguous claims, but only within the configured `max_vms`; stale hard-coded max values elsewhere can miss `vm-8`. |
| `src/vm/lifecycle.rs:47-65` | Post-lease generator/output | `up` computes request count from `config.vms.len()`, calls `reserve_n`, builds `VmDef`s from returned IDs, and prints `Reserved: vm-N, ...`. | This is the desired runtime shape, but the printed format must be treated as stable because scripts may parse it. If `config.vms` remains prefix-based, request count is correct but pre-lease target state is not. |
| `src/vm/lifecycle.rs:119-128` | Down consumer | `down` calls `config.leased_vms()` and removes claims by each leased VM index. | Down already targets "everything this cluster currently claims" when claims exist; fallback to `self.vms` is legacy behavior only for clusters without claims. |
| `src/core/backend.rs:248-250` and `src/core/backend.rs:292-293` | Runner consumer | MicroVM start looks for `runners_dir/<vm.name>/bin/microvm-run`. | If Nix builds only `vm-1` for a `--vm-count 1` wrapper and lease picks `vm-7`, boot fails with `Runner not found: .../vm-7/bin/microvm-run`. This is the concrete incident failure. |
| `src/vm/preflight.rs:35-44` | Pool scanner with hard-coded bound | Cleanup scans `1..=7` and formats `vm-{id}` directly, independent of `config.max_vms`. | Fixture `max_vms = 8` leaves `vm-8` stale PIDs, orphan processes, or sockets untouched; flexible allocation expands the chance of landing on an unclean slot. |
| `src/main.rs:207-220` | Target normalizer | Visual record target normalization maps `1` to `vm-1` and searches only `config.vms`. | If the active leased VM is `vm-7` but `config.vms` contains `vm-1`, default/explicit visual capture cannot select the actual reserved VM unless config derivation is updated. |
| `src/harness/runner.rs:1576-1588` | Target normalizer | Scene VM selection maps numeric targets to `vm-N` and searches the run's `target_vms`. | This is safe only if `target_vms` is the post-lease VM list. If it receives prefix `config.vms`, scene checks target the wrong slot or fail to find the reserved slot. |
| `nix/lib/test-cluster.nix:96-105` | Nix generator | `linuxVMs` is generated for `vm-1..vmCount`; IP, MAC, and index are derived from the name. | The pool size is currently tied to `vmCount = 7`, not project `max_vms = 8`; Nix cannot build or describe `vm-8` runners without changing this bound. |
| `nix/lib/test-cluster.nix:284-296` | Runner-dir generator | `mkRunnersDir flavor count` filters with `takeFirstVMs count`, so mode wrappers build only `vm.index <= count`. | 1v1 wrappers build only `vm-1`; any skipped lease that lands on `vm-2+` fails at backend runner lookup. This is the main Nix blocker. |
| `nix/lib/test-cluster.nix:413-416` | Wrapper generator | `mkAllFlavorScripts` passes the mode count into `mkRunnersDir` and into `cluster-ctl --vm-count`. | The same integer currently means both "requested VMs" and "runner slots available"; flexible allocation needs those meanings split. |
| `nix/lib/test-cluster.nix:447-456` | Mode setup | 1v1 wrappers use count `1`; tournament wrappers use `vmCount`. | Tournament wrappers can boot any slot up to `vm-7`, while 1v1 wrappers cannot, creating inconsistent behavior across modes. |
| `tests/fixture/flake.nix:116-128` | Workaround | `cluster1v1UifullSoloUp` intentionally reuses the tournament runner dir so a one-VM diagnostic can land on `vm-N` up to 7. | This confirms the fix shape: the diagnostic works when the runner dir contains the slot chosen by the lease, but it is an unexported workaround rather than the standard 1v1 contract. |
| `tests/fixture/diagnose/run.sh:42-67` | Workaround | Diagnostic pre-writes placeholder claims for `vm-1..vm-6` to force `reserve_n` onto `vm-7`. | This can be deleted after 1v1 runner dirs contain the full pool and `up` surfaces the selected ID; until then it is fragile and can collide with real claims. |
| `nix/module.nix:8-9` and `nix/module.nix:198-201` | Host pool generator | The NixOS module creates `vmNames = vm-1..vmCount`, TAP names, and login state directories from `cfg.vmCount`. | Host-level resources must be provisioned for the pool, not only a mode's requested count; otherwise flexible allocation may choose a slot without a TAP or login directory. |

Display-only examples and unit tests that merely use `vm-1` as fixture data were
not treated as blockers unless they create or consume runtime VM sets.

## Decision Record

1. `cluster-ctl --vm-count N` means "reserve N VMs from the configured pool, choosing any N free IDs from `1..=max_vms`."
   Rationale: the lease code already skips held slots, and preserving contiguity is the behavior that caused the 2026-05-11 fixture failure.

2. Allocation order is deterministic skip-and-take: scan `1..=max_vms` and take the first N free slots.
   Rationale: this is already implemented, is easy to reason about, keeps logs stable, and avoids adding random or preference-list policy before there is a concrete need.

3. The chosen IDs are surfaced on stdout as exactly one reserved-list line from `up`: two leading spaces, `Reserved: `, then comma-space separated VM names in allocation order, for example `  Reserved: vm-2, vm-4`.
   Rationale: the line already exists in `src/vm/lifecycle.rs:64-65`; formalizing it gives scripts a stable low-cost parse point without adding a new output mode in this phase.

4. Do not add `--vm-allocation sequential|any`; switch the default contract to flexible allocation.
   Rationale: the audit found compatibility problems in build/config assumptions, not a legitimate runtime need for contiguity; adding a flag would preserve the broken path and complicate every wrapper.

5. `mkTestCluster` runner dirs should contain runners for the full host pool, `vm-1..vm-max_vms`, while wrappers still pass `--vm-count` as the requested count.
   Rationale: backend lookup is by chosen VM name, so every allocatable slot must have a runner; building only `vm-1..vm-count` is the direct cause of `Runner not found`.

6. `cluster-ctl down` operates on every live claim owned by the cluster, not on `vm-1..vm-count`.
   Rationale: `down` already calls `config.leased_vms()`, which uses `list_cluster_vms(cluster, max_vms, lock_dir)` and only falls back to `self.vms` when no claims exist.

## Runner-Dir Disk Cost

The requested `result-default` path did not exist in the working tree, so I
built the in-tree fixture's tournament UI-full wrapper expression and extracted
its runner dir:

```bash
nix build --impure --expr '
let
  fixture = builtins.getFlake "path:/data/nvme0/can/Projects/steampipe/tests/fixture";
  steampipe = builtins.getFlake "path:/data/nvme0/can/Projects/steampipe";
  system = "x86_64-linux";
  pkgs = import fixture.inputs.nixpkgs { inherit system; overlays = [ steampipe.overlays.default ]; };
  lib = pkgs.lib;
  projectConfig = builtins.fromTOML (builtins.readFile /data/nvme0/can/Projects/steampipe/tests/fixture/steampipe.toml);
  workloads = {
    footBanner = import /data/nvme0/can/Projects/steampipe/tests/fixture/workloads/foot-banner.nix { inherit pkgs lib; };
    vkcubeFrozen = import /data/nvme0/can/Projects/steampipe/tests/fixture/workloads/vkcube-frozen.nix { inherit pkgs; };
    wgpuChecker = import /data/nvme0/can/Projects/steampipe/tests/fixture/workloads/wgpu-checker.nix { inherit pkgs lib; };
  };
  cluster = steampipe.lib.mkTestCluster {
    inherit pkgs lib steampipe projectConfig;
    nixpkgs = fixture.inputs.nixpkgs;
    clusterCtl = steampipe.packages.${system}.default;
    projectRoot = /data/nvme0/can/Projects/steampipe/tests/fixture;
    extraVmModules = [
      (import /data/nvme0/can/Projects/steampipe/tests/fixture/workloads.nix {
        inherit workloads;
        vmUser = projectConfig.vm_user;
      })
    ];
  };
in cluster.scripts."cluster-tournament-uifull-up"
' -o result-tournament-uifull-up
grep -oE -- '--runners-dir [^[:space:]]+' result-tournament-uifull-up | awk '{ print $2 }' | tail -n1
```

The extracted runner dir was:

```text
/nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull
```

The literal `du` invocation over the link farm reports only symlink directory
size, not the Nix closure:

```bash
du -sh /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull/vm-{1,2,3,4,5,6,7} \
  /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull
```

Output:

```text
4.0K  /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull/vm-1
4.0K  /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull/vm-2
4.0K  /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull/vm-3
4.0K  /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull/vm-4
4.0K  /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull/vm-5
4.0K  /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull/vm-6
4.0K  /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull/vm-7
0     /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull
```

The meaningful store closure measurement is:

```bash
nix path-info -Sh /nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull
```

Output:

```text
/nix/store/s2vkmg4k74wj7cr4ydr7gizd95dnym6f-cluster-vm-runners-uifull  18.6 GiB
```

Each individual runner target has a 7.9 GiB closure when measured alone, but
the seven-runner link farm shares dependencies and has an aggregate closure of
18.6 GiB. This exceeds the plan's 5 GiB escalation threshold, so Phase C should
not treat "build all UI-full slots" as a free change. It is still the simplest
correct contract, but the implementation should explicitly accept the closure
cost or introduce a more selective runner materialization strategy.

## Interface Sketches

### `reserve_n`

The function signature can remain unchanged. The contract needs to be made
explicit and tests should assert gaps.

```rust
/// Reserve `count` VM IDs from `1..=max_vms`.
///
/// Returns exactly `count` IDs in ascending pool order. Held slots owned by
/// other live clusters are skipped. The returned IDs are not guaranteed to be
/// contiguous and are not guaranteed to start at 1.
pub fn reserve_n(
    count: u8,
    max_vms: u8,
    lock_dir: &Path,
    cluster: &str,
) -> anyhow::Result<Vec<u8>>;

// Example: vm-1 and vm-3 are held by other clusters.
let ids = reserve_n(3, 7, lock_dir, "fixture")?;
assert_eq!(ids, vec![2, 4, 5]);
```

### `ClusterConfig.vms` Derivation

Phase B should split "requested count" from "chosen VM definitions". The config
load phase can keep enough static information to validate the bridge and know
the pool, but lifecycle commands should derive the operational VM list from the
reserved IDs before starting, deploying to, or targeting instances.

```rust
struct ClusterConfig<S> {
    requested_vm_count: u8,
    max_vms: u8,
    subnet: String,
    vm_overrides: BTreeMap<u8, VmOverride>,
    vms: Vec<VmDef>, // operational set; after leasing, this is chosen IDs
    _state: PhantomData<S>,
}

impl<S> ClusterConfig<S> {
    fn vm_def_for_id(&self, id: u8) -> anyhow::Result<VmDef> {
        let override_cfg = self.vm_overrides.get(&id);
        Ok(VmDef {
            name: VmName(format!("vm-{id}")),
            ip: override_cfg
                .and_then(|vm| vm.ip.clone())
                .unwrap_or_else(|| IpAddr(format!("{}.{id}", self.subnet))),
            index: id,
        })
    }

    fn with_allocated_vm_ids<T>(self, ids: &[u8]) -> anyhow::Result<ClusterConfig<T>> {
        let vms = ids
            .iter()
            .map(|&id| self.vm_def_for_id(id))
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(ClusterConfig {
            requested_vm_count: self.requested_vm_count,
            max_vms: self.max_vms,
            subnet: self.subnet,
            vm_overrides: self.vm_overrides,
            vms,
            _state: PhantomData,
        })
    }
}

// Lifecycle shape:
let config = ClusterConfig::<Unchecked>::load(...)?.validate_or_skip_bridge()?;
let ids = reserve_n(
    config.requested_vm_count,
    config.max_vms,
    &config.lock_dir,
    &config.cluster_name,
)?;
let config = config.with_allocated_vm_ids::<BridgeReady>(&ids)?;
up_allocated(&config, runners_dir).await?;
```

The existing `Unchecked -> BridgeReady` typestate does not need a conceptual
change: bridge validation is independent of selected VM IDs. The operational
change is that commands which act on instances must use the post-lease config or
an explicit `Vec<VmDef>` from the reserved IDs, not the pre-lease prefix list.

## Follow-Up Requirements

Phase B:

- Update docs/tests for `reserve_n` to assert non-contiguous allocation.
- Replace prefix-derived command targets with post-lease `VmDef`s where commands operate on running instances.
- Make stale cleanup use `config.max_vms`, not hard-coded `7`.
- Preserve per-VM overrides when deriving `VmDef`s from reserved IDs.

Phase C:

- Split Nix runner-dir pool size from wrapper request count.
- Build runner dirs for every allocatable slot or provide an explicit alternative that still guarantees `runners_dir/vm-N/bin/microvm-run` exists for any reserved `N`.
- Reconcile `vmCount = 7` with fixture `max_vms = 8`; the pool bound must be one source of truth.

Phase E:

- Delete the placeholder-claim workaround in `tests/fixture/diagnose/run.sh` after 1v1 UI-full wrappers can boot whichever free slot the lease returns.
