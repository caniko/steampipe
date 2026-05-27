# Phase 02 — Parallelize `steam login` + skip-bounce when reachable

> **Recommended Codex model: GPT 5.5 medium**
>
> The parallel shape already exists in `steam::auto_login`
> ([src/game/steam.rs:900-970](../../../src/game/steam.rs#L900-L970)) using
> `par_each_vm` + `admit()`. This phase ports that shape to `steam::login`
> and folds in a "skip the VM bounce when the VM is already reachable"
> optimization. Moderate complexity (preserving fill-the-gaps, cleanup paths,
> aggregate reporting) but no new design. `low` is too thin for the cleanup
> bookkeeping; `high` is unnecessary.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Prerequisite: Phase 01 must be
landed and validated on the atlas host** — without it the parallel loop just
multiplies the failure. Do not start this phase until
`cluster-ctl steam login vm-1 --force` succeeds end-to-end.

## Goal

`cluster-ctl steam login all` runs every selected VM's login concurrently
(subject to the host-memory admission gate) and exits in roughly the time of
the slowest single VM rather than the sum of all VMs. When a VM is already
reachable, the login flow skips the stop/start bounce and goes straight to
the SteamCMD bootstrap. The existing "fill-the-gaps" semantics
([src/game/steam.rs:581-606](../../../src/game/steam.rs#L581-L606)) are
preserved: VMs whose sessions classify as `Ok` are still skipped, and
`--force` re-logs every selected VM regardless.

## Why this matters now

Today `steam::login` iterates VMs sequentially
([src/game/steam.rs:551-622](../../../src/game/steam.rs#L551-L622)) and
unconditionally `stop_instance`s + `start_instance`s every target
([src/game/steam.rs:787-792](../../../src/game/steam.rs#L787-L792)).
With 8 VMs at ~60 s of SteamCMD bootstrap + ~30 s of boot apiece, "login all"
takes 10+ minutes even when everything works. The user's stated goal is a
"quick headless CLI way of logging into Steam on all our VMs quickly and
efficiently" — the lifecycle fix in Phase 01 is necessary but not
sufficient for that.

The serial / always-bounce shape is a relic of the interactive VNC-based
login path. The automated path has no operator-presence constraint and can
fan out safely; `auto_login` already proves it.

## Out of scope

- Do not change the virtiofsd lifecycle — that lives in Phase 01. If you
  hit virtiofsd errors here, the prerequisite is not actually met; stop and
  finish Phase 01.
- Do not change interactive VNC login (`creds.is_none()` branch in
  [src/game/steam.rs:813-819](../../../src/game/steam.rs#L813-L819)). It
  must remain serial because it expects operator presence on one VM at a
  time. Parallelisation is *automated-login-only*.
- Do not change the SteamCMD bootstrap, the Steam Guard flow, or the GUI
  validation pipeline ([src/game/steam.rs:980-1027](../../../src/game/steam.rs#L980-L1027),
  [src/game/steam.rs:1233+](../../../src/game/steam.rs#L1233)). They are
  correct and orthogonal.
- Do not touch docs here — Phase 03 owns the prose.
- Do not change the lease subsystem
  ([src/vm/lease.rs](../../../src/vm/lease.rs)). The audit `[lease] ...
  reclaiming.` line is fine.

## Plan

1. **Introduce a reachability probe before the bounce.** In
   `login_single_vm` ([src/game/steam.rs:767-842](../../../src/game/steam.rs#L767-L842))
   replace the unconditional `stop_instance` + `start_instance` pair
   ([src/game/steam.rs:787-792](../../../src/game/steam.rs#L787-L792))
   with:
   - If `backend.is_reachable(&vm.ip).await` (a single SSH probe, no
     polling), skip the stop/start, skip `admit().await`, and proceed
     straight to `automated_login`. Log `"  {vm.name}: already up, skipping
     reboot"`.
   - Otherwise keep the current behavior: stop → admit → start → write
     claim → wait_ready.
   - **Interactive branch is unchanged.** Wrap the new short-circuit in
     `if automated { ... }`. For interactive login (`creds.is_none()`), the
     operator may want the fresh boot, so keep the bounce.

2. **Split out a pure async closure suitable for `par_each_vm`.** Refactor
   `steam::login` ([src/game/steam.rs:551-622](../../../src/game/steam.rs#L551-L622))
   so the per-VM logic returns a structured result:
   ```rust
   enum LoginAttempt { Skipped(String), LoggedIn, Failed(anyhow::Error) }
   ```
   The closure for each VM:
   - performs the fill-the-gaps classification
     ([src/game/steam.rs:583-606](../../../src/game/steam.rs#L583-L606))
     and returns `Skipped(reason)` if `force` is false and status is `Ok`,
   - otherwise calls `login_single_vm` and maps to `LoggedIn` /
     `Failed(e)`.
   - Pure-async, owned-data only — clone what you need into the move
     closure so `par_each_vm`'s `'static` bound is satisfied (see
     [src/core/config.rs:100-115](../../../src/core/config.rs#L100-L115) and
     the existing `auto_login`
     [src/game/steam.rs:920-945](../../../src/game/steam.rs#L920-L945) for
     the cloning pattern).

3. **Fan out only for the automated path.** Keep the interactive branch
   serial: it requires a TTY for password prompts and Steam Guard, and
   parallel VNC sessions would clobber `stdout`. Concretely:
   ```rust
   if creds.is_some() {
       par_each_vm(&targets, |vm| { ... }).await?
   } else {
       // existing serial loop
   }
   ```
   Use the parallel results to build the same `Logged in / Skipped /
   Failed` counts the current code prints
   ([src/game/steam.rs:617](../../../src/game/steam.rs#L617)) so the
   user-facing summary is unchanged.

4. **Preserve cleanup semantics under parallelism.** Each
   `login_single_vm` already owns its own cleanup closure
   ([src/game/steam.rs:797-810](../../../src/game/steam.rs#L797-L810)). With
   parallelism, a `bail!` in one task must not abort the others —
   `par_each_vm` already isolates this via `JoinSet`
   ([src/core/config.rs:106-114](../../../src/core/config.rs#L106-L114)), so
   wrap any propagating `?` in a `Result` capture inside the closure.

5. **Order the user-facing output.** Parallel prints from each task will
   interleave. Either:
   - **Preferred:** collect per-VM `String` line buffers in the closure and
     print them after all tasks finish, sorted by `vm.index`. This keeps
     existing log shape intact (`══ Steam login for vm-N ══` block per VM)
     while making output deterministic.
   - **Alternative:** keep streaming and prefix every line with `vm.name`.
     Less pretty; only adopt if the buffered approach materially worsens
     debugging in failure cases.

6. **Add tests** in [src/game/steam.rs](../../../src/game/steam.rs) tests
   module:
   - Unit test that `login_single_vm` skips the stop/start when
     `is_reachable` returns true and creds are present.
   - Unit test that the same call still bounces when creds are present but
     the VM is unreachable.
   - Smoke test (gated behind a `#[ignore]` if it needs a real backend)
     that documents the expected order of operations. The existing
     `steam_login_*` parse tests
     ([src/ui/cli.rs:906+](../../../src/ui/cli.rs#L906)) cover CLI; this
     test covers internal control flow only.

7. **Real-host validation.** Run on the atlas host both cold and warm:
   ```bash
   # Cold: every VM down, fresh boot + login in parallel
   cluster-ctl down
   time cluster-ctl steam login all
   # Warm: every VM already up, sessions already fresh — should be near-instant
   time cluster-ctl steam login all
   # Force: every VM up, --force triggers re-login but no reboot
   time cluster-ctl steam login all --force
   ```
   Record times in the PR description; the cold run is the primary
   acceptance criterion below.

## Acceptance criteria

- [ ] `cargo test -p steampipe game::steam::tests` passes, including the two
      new reachability-skip tests.
- [ ] On atlas, cold `cluster-ctl steam login all` against 8 VMs completes
      in ≤ 4 minutes wall-clock (down from ~10 minutes serial) and exits 0.
- [ ] Warm `cluster-ctl steam login all` (every account already classified
      `Ok`) exits in ≤ 5 seconds and prints `"already logged in (...)"` for
      every VM.
- [ ] `cluster-ctl steam login all --force` on a fully-up cluster re-logs
      every VM **without** stopping/starting any of them (verify with
      `journalctl --grep 'microvm@vm-' -n 20` or by checking
      `~/.local/state/steampipe/<cluster>/vm-N.pid` mtimes do not change).
- [ ] Output is grouped per VM in `vm.index` order (no interleaved lines
      within a `══ Steam login for vm-N ══` block).
- [ ] Interactive login (`cluster-ctl steam login vm-1` with no
      credentials TOML) still runs serially against a single VM and shows
      VNC instructions — verify by manual run on atlas.
- [ ] No regression in `cluster-ctl steam guard` or `cluster-ctl steam
      check` behavior.

## Files likely touched

- `src/game/steam.rs` — `login`, `login_single_vm`, the new `LoginAttempt`
  enum, the reachability short-circuit, the parallel dispatch wrapping the
  automated branch, the new tests.
- `src/lib.rs` — no functional change expected; only touch if the
  `SteamAction::Login` branch
  ([src/lib.rs:905-935](../../../src/lib.rs#L905-L935)) needs a signature
  tweak after the refactor.

If you find yourself editing `src/core/backend.rs`, `src/vm/lease.rs`, or
`nix/`, stop — that is Phase 01 territory or out of scope entirely.

## Pitfalls

- **`par_each_vm` requires `'static` futures.** Every captured reference
  must be cloned into the closure. The existing `auto_login` shape
  ([src/game/steam.rs:920-945](../../../src/game/steam.rs#L920-L945))
  shows the pattern: clone `backend`, `vm_user`, `state_dir`, and `creds`
  into the closure. Symptom: borrow-checker errors at `tasks.spawn`.
  Recovery: clone earlier.
- **`admit().await` placement under parallelism.** Today every call to
  `admit` blocks until host memory drops below the watermark
  ([src/core/admission.rs:51-53](../../../src/core/admission.rs#L51-L53)).
  In parallel, all 8 closures will hit the gate simultaneously; that is
  fine — the gate serialises them naturally. But if you accidentally call
  `admit` outside the per-task closure (e.g. before the spawn loop), you
  rate-limit the spawn loop instead of the per-task admit. Symptom: VMs
  spawn in lockstep. Recovery: keep `admit().await` inside the closure,
  guarding only the `start_instance` call.
- **Skipping the bounce when login state is mid-write.** If a prior
  `steam login` was killed in the middle of writing
  `~/.local/share/Steam/config/loginusers.vdf`, the VM may be reachable
  but Steam is in a stale state. Symptom: SteamCMD bootstrap fails
  immediately with corrupt config. Recovery: this is a documented
  fill-the-gaps territory — `--force` exists for this case. Note in code
  comments that the reachability shortcut delegates correctness to
  fill-the-gaps + `--force`.
- **Interleaved stdout under parallel println.** Multiple tasks calling
  `println!` produce torn lines. Use the per-VM buffered-output pattern
  in step 5. Symptom: garbled output in cold-run logs. Recovery: buffer
  and emit in `vm.index` order.
- **`auto_login` already does parallel — don't double-fire.** `auto_login`
  is called from `Commands::Up`
  ([src/lib.rs:840-842](../../../src/lib.rs#L840-L842)). It uses the
  *regular* runners and is already correct. This phase only changes
  `SteamAction::Login`. Do not refactor `auto_login` to share code unless
  the duplication is obvious; resist a premature DRY pass.
- **Test isolation under parallel `tokio::task`.** If your new tests use
  shared global state (e.g. the `POLICY` `OnceCell` in `admission.rs`),
  they may race other tests. Use `serial_test::serial` like
  `src/core/nixos_module.rs` does
  ([src/core/nixos_module.rs:280-281](../../../src/core/nixos_module.rs#L280-L281))
  if you touch any global.

## Reference

- Existing parallel pattern:
  [src/game/steam.rs:900-970](../../../src/game/steam.rs#L900-L970)
  (`auto_login`).
- `par_each_vm` definition:
  [src/core/config.rs:100-115](../../../src/core/config.rs#L100-L115).
- Reachability probe:
  [src/core/backend.rs:158-215](../../../src/core/backend.rs#L158-L215)
  (`is_reachable` / `wait_ready`).
- Fill-the-gaps classification:
  [src/game/steam.rs:581-606](../../../src/game/steam.rs#L581-L606) and
  [src/game/accounts.rs](../../../src/game/accounts.rs).
- Research dossier — "What `auto_login` already gets right":
  [../headless-steam-login-research.md](../headless-steam-login-research.md).
