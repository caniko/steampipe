# Phase 06 — Wire `memory-admission` admission gate around VM startup

> **Recommended Codex model: GPT 5.5 medium**
>
> New Rust module + Cargo dep + audit of every `start_instance`
> call site for the right async wrapping. The agent has to read
> the `memory-admission` crate's API surface, pick reasonable
> watermarks for atlas, gracefully degrade on provider failure, and
> add a small `cluster-ctl admission status` subcommand. Moderate
> complexity, sub-agent role. `low` would skip the call-site audit
> (the whole point of the gate); `high` is overkill for ~80 lines.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase:

- `cluster-ctl` has a `memory-admission`-backed admission gate
  initialised at startup, watching `/proc/meminfo`.
- Every `backend.start_instance(...)` call site is wrapped with
  `gate.wait().await` (no-op on sequential paths; load-bearing for
  parallel paths now and in the future).
- `cluster-ctl admission status` prints the current `MemoryStats`
  + configured watermarks.
- Provider failures (e.g. `/proc/meminfo` unreadable) log via
  `tracing` and fall through to "no throttling" — never block on
  diagnostic infrastructure.

## Why this matters now

Guest-side OOM is handled by virtio-balloon + deflateOnOOM (Phase
09). Host-side OOM during *parallel* VM startup is a separate
failure mode: if N VMs allocate their initial memory simultaneously
and the sum overshoots host RAM, the host OOM-killer claims one of
the crosvm/qemu/CHV processes and the corresponding VM dies before
sshd ever comes up. That manifests as another "Steam GUI validation
failed ()" diagnostic — empty stdout, empty stderr — because the
SSH command was running against a process that vanished.

Today the `cluster-ctl steam login all` path is sequential, so the
host-OOM risk isn't reachable. But:

- `cluster-ctl up` fans out across runtime VMs.
- `harness::runner` parallel test launches start multiple VMs.
- A future parallel-login path is a plausible refactor away.

The admission gate is **defense in depth**, scoped to the layer
that actually decides concurrency (cluster-ctl in Rust). The
microvm.nix / NixOS module layer never starts more than one VM in
a single evaluation, so this can't be expressed there.

The crate is owned by this project's author; no semver-pin
constraint.

## Out of scope

- Adding the admission gate inside the VM (guest-side OOM is Phase
  09's territory).
- Replacing virtio-balloon. The gate complements ballooning; it
  doesn't substitute.
- Building host-wide memory accounting infrastructure. The gate is
  a per-process decision; it doesn't coordinate across multiple
  `cluster-ctl` invocations. (That's accepted; the realistic
  failure mode is one `cluster-ctl` fanning out, not two competing.)
- Sampling cgroups instead of `/proc/meminfo`. `ProcMeminfoProvider`
  is enough for atlas; a future cgroup-aware provider can be added
  via the `MemoryProvider` trait.

## Plan

1. **Add the Cargo dependency.** In the workspace `Cargo.toml`:

   ```toml
   memory-admission = { version = "*", default-features = false, features = ["tokio"] }
   ```

   No version pin (owned by author, see design). Confirm `tracing`
   is already a workspace dep (it is; many existing modules import
   it).

   Run `cargo build -p steampipe` once to populate `Cargo.lock`.

2. **Create `src/core/admission.rs`**:

   ```rust
   //! Host-side memory admission gate around parallel VM startup.
   //!
   //! Defends against host OOM-killer claiming a hypervisor process
   //! when multiple VMs allocate initial memory simultaneously.
   //! Guest-side OOM is handled by virtio-balloon + deflateOnOOM
   //! (see `nix/lib/microvm-runner.nix`).

   use std::sync::OnceLock;
   use memory_admission::r#async::AdmissionGate;
   use memory_admission::providers::ProcMeminfoProvider;
   use memory_admission::Config;

   static GATE: OnceLock<AdmissionGate> = OnceLock::new();

   pub fn gate() -> &'static AdmissionGate {
       GATE.get_or_init(|| {
           AdmissionGate::new(Config {
               // Refuse new admissions when free host RAM falls
               // below the largest single VM class (4 GiB login)
               // + 1 GiB host headroom.
               high_watermark_mb: 5 * 1024,
               low_watermark_mb:  6 * 1024,
               provider: Box::new(ProcMeminfoProvider::default()),
               ..Config::default()
           })
       })
   }

   /// Block until host memory is below the high watermark (or the
   /// provider fails, in which case fall through without blocking).
   /// Reliability priority: never let diagnostic infrastructure
   /// block startup.
   pub async fn admit() {
       match gate().wait().await {
           Ok(()) => {}
           Err(e) => {
               tracing::warn!(error = ?e, "admission gate provider failed; proceeding without throttling");
           }
       }
   }

   pub fn current_stats() -> Option<memory_admission::MemoryStats> {
       gate().stats().ok()
   }
   ```

   Confirm the crate's API against `cargo doc --open -p memory-admission`
   or `docs.rs/memory-admission`. The exact method names
   (`gate().wait()`, `gate().stats()`) may need adjustment.

3. **Wire it into `src/core/mod.rs`** (or wherever `src/core/`
   modules are surfaced):

   ```rust
   pub mod admission;
   ```

4. **Audit every `start_instance` call site.** Run:

   ```bash
   grep -nE 'backend\.start_instance|start_instance\(' src/ -r --include='*.rs'
   ```

   Expected sites (verify; the list may have shifted):
   - `src/game/steam.rs:696` — sequential, but wrap defensively.
   - `src/vm/lifecycle.rs` — startup in `wait_for_ssh` flow.
   - `src/harness/runner.rs` — parallel test launches.
   - `src/lib.rs` — `cluster_up_all`.

   At each call site, before `start_instance`, add:

   ```rust
   crate::core::admission::admit().await;
   let pid = backend.start_instance(config, vm, runners_dir)?;
   ```

   Sequential paths gain a no-op; parallel paths gain the gate.

5. **Add `cluster-ctl admission status` subcommand.** In the CLI
   definition (likely `src/ui/cli.rs`):

   ```rust
   /// Show the host memory admission gate's current state.
   Admission {
       #[command(subcommand)]
       action: AdmissionAction,
   },

   #[derive(Subcommand, Debug)]
   pub enum AdmissionAction {
       /// Print MemoryStats + configured watermarks.
       Status,
   }
   ```

   Handler:

   ```rust
   pub fn print_admission_status() {
       match crate::core::admission::current_stats() {
           Some(stats) => {
               println!("memory-admission status:");
               println!("  free_mb     = {}", stats.free_mb);
               println!("  available_mb = {}", stats.available_mb);
               println!("  high watermark = 5120 MB");
               println!("  low  watermark = 6144 MB");
           }
           None => {
               println!("memory-admission status: provider unavailable (gate is disengaged; startup is not throttled)");
           }
       }
   }
   ```

6. **Unit test the gate config** using `FixedProvider` (the
   crate's test stub):

   ```rust
   #[tokio::test]
   async fn gate_blocks_when_free_below_high_watermark() {
       use memory_admission::providers::FixedProvider;
       use memory_admission::r#async::AdmissionGate;
       let gate = AdmissionGate::new(Config {
           high_watermark_mb: 5 * 1024,
           low_watermark_mb:  6 * 1024,
           provider: Box::new(FixedProvider::new(1024)), // 1 GiB free
           ..Config::default()
       });
       let t1 = tokio::spawn({
           let g = gate.clone();
           async move { g.wait().await }
       });
       tokio::time::sleep(std::time::Duration::from_millis(50)).await;
       assert!(!t1.is_finished());
       // Bump provider above low watermark to release.
       gate.set_provider(Box::new(FixedProvider::new(7 * 1024)));
       t1.await.unwrap().unwrap();
   }
   ```

   The exact API for swapping providers may differ — adjust based
   on what the crate exposes.

7. **Verify** locally:

   ```bash
   cargo build --workspace
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --check
   ./target/debug/cluster-ctl admission status
   ```

   The `admission status` command should print a numeric sample
   when run on a Linux host with `/proc/meminfo` readable.

8. **Document** in `docs/src/configuration/vm-resources.md` (if it
   exists from Phase 09) or note in
   `docs/src/configuration/vm-lifecycle.md` (until Phase 09 moves
   it) — operators should know:
   - The gate is silent under normal load.
   - `cluster-ctl admission status` is the operator-facing inspect
     command.
   - On `/proc/meminfo` failure, the gate disengages by design —
     it's defense in depth, not a hard ceiling.

9. Commit:

   ```
   cluster-ctl: host-side memory admission gate

   Defends against host OOM-killer claiming a hypervisor process
   during parallel VM startup. Wraps backend.start_instance call
   sites with memory_admission::async::AdmissionGate (proc/meminfo
   provider, 5/6 GiB watermarks). Sequential paths see a no-op;
   parallel paths (cluster-ctl up, harness fan-out) now block on
   memory pressure. Provider failure logs and falls through —
   never blocks startup on diagnostic infrastructure.
   ```

## Acceptance criteria

- [ ] `memory-admission` is in `Cargo.toml` and `Cargo.lock`.
- [ ] `src/core/admission.rs` exists with `gate()`, `admit()`, and
      `current_stats()` exposed.
- [ ] Every `backend.start_instance(...)` call site in
      `src/game/`, `src/harness/`, `src/vm/`, `src/lib.rs` is
      preceded by `crate::core::admission::admit().await;`.
      Commit message names the count (e.g. "4 call sites wrapped").
- [ ] `cluster-ctl admission status` on a Linux host prints
      non-empty MemoryStats + watermark lines.
- [ ] `cluster-ctl admission status` on a host with no
      `/proc/meminfo` (or with an injected provider failure) prints
      the disengaged-by-design message and exits 0.
- [ ] The unit test `gate_blocks_when_free_below_high_watermark`
      passes.
- [ ] `cargo test --workspace` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is
      clean.
- [ ] Provider-failure path logs via `tracing::warn!`, not
      `eprintln!`.

## Files likely touched

- `Cargo.toml` — new dependency.
- `Cargo.lock` — generated.
- `src/core/admission.rs` — new module.
- `src/core/mod.rs` (or equivalent) — `pub mod admission;`.
- `src/core/backend.rs` and/or call-site files in `src/game/`,
  `src/harness/`, `src/vm/`, `src/lib.rs` — `admit().await` wraps.
- `src/ui/cli.rs` (or `src/cli/`) — new `Admission` subcommand.
- `docs/src/configuration/vm-lifecycle.md` (interim) or
  `docs/src/configuration/vm-resources.md` (after Phase 09).

## Pitfalls

- **API drift**: the crate is at 0.1.x. Method names
  (`gate().wait()`, `gate().stats()`, `set_provider`) may differ
  from this phase's sketch. Read `cargo doc --open -p memory-admission`
  before wiring; adjust names.
- **Don't block on provider failure**: the design pinned this.
  `Err(_)` from `gate().wait()` must log + fall through, never
  `.unwrap()` or `?`.
- **Watermarks too tight**: 5/6 GiB are sized for atlas's expected
  load. On a host with less RAM, they'd block the first VM
  startup entirely. Pull from `/proc/meminfo` to validate atlas
  has at least ~16 GiB free at idle before committing those
  numbers, or make the watermarks configurable via env var as a
  follow-up. Don't ship a static number you haven't verified.
- **OnceLock vs lazy_static vs Lazy**: stick with `std::sync::OnceLock`
  (stable, no dep). Some other modules in this codebase may use
  `once_cell` — match local convention.
- **Don't gate `wait_ready`**: only gate `start_instance`. Once a
  VM has started, we want sshd to come up regardless of subsequent
  memory pressure; the gate is for *admission*, not for
  *throughput*.
- **`tokio` feature gate**: `memory-admission`'s `tokio` feature
  is what gates the `async` module. If you pull the crate without
  the feature flag, only the sync flavour is available. Confirm
  the feature is in the Cargo.toml dep line.
- **Cargo.lock churn**: adding `memory-admission` may pull in
  transitive deps. Keep the commit's `Cargo.lock` diff inspectable;
  don't add unrelated workspace dep bumps in the same commit.

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Rust Code Changes" → "Host-side admission gate").
- Crate: <https://crates.io/crates/memory-admission>.
- Crate docs: <https://docs.rs/memory-admission/>.
- Failure mode the gate defends against: host OOM-killer during
  parallel start (the same diagnostic-eat the SSH-fix dossier
  flagged as "empty stdout AND stderr" in
  [`finish_steam_login`](../../../src/game/steam.rs#L1119)).
