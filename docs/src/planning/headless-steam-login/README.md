# Headless Steam Login — Plan Set

> **Recommended Codex model for plan-set orchestration: GPT 5.5 high**
>
> This plan sequences three phases that span the Rust orchestrator, the NixOS
> module, and user-facing docs. The orchestration call (which phases unlock
> what, where the file conflicts live) deserves high effort once at the
> coordination level so each phase agent can stay focused.

## Scope

Make `cluster-ctl steam login all` actually log every VM in cleanly, headless,
and in parallel. The root cause and design choices come from the research
dossier:

- Dossier: [headless-steam-login-research.md](../headless-steam-login-research.md)
- Originating failure: `vm-1 ... vm.log` reports
  `microvm@vm-1: -chardev socket,id=fs0,path=vm-1-virtiofs-steam-state-vm-1.sock:
  Failed to connect to 'vm-1-virtiofs-steam-state-vm-1.sock': No such file or
  directory` — the login runner depends on a virtiofsd socket that nothing
  starts.

## Decisions baked into this plan

The dossier's "Open Decisions" are resolved here so the phase agents do not
re-litigate them:

- **Virtiofsd orchestration shape:** in-process (cluster-ctl spawns
  `virtiofsd-run` alongside `microvm-run`). Rationale: minimum behavioural
  change for the operator UX; keeps `cluster-ctl` self-contained. Phase 01
  implements this.
- **Runner classes:** keep the existing split (regular vs. login runners).
  Rationale: collapsing rebuilds every host runner and changes the boot path
  for every command; the dossier's writable-state design only needs the share
  on the login class.
- **Skip VM reboot when reachable:** yes. Rationale: `auto_login` already does
  this; for "login all" right after `up` it is a multi-minute win. Phase 02
  implements this and treats `--force` as "re-login even if cached" — not as
  "force a VM bounce".

If new evidence forces a redo, update this README and the affected phase
docs together rather than diverging in chat.

## Phase table

| Phase | File | Model | Depends on | Touches | Blocking? |
|-------|------|-------|------------|---------|-----------|
| 01 — Virtiofsd lifecycle in cluster-ctl | [01-virtiofsd-lifecycle.md](./01-virtiofsd-lifecycle.md) | 5.5 high | — | `src/core/backend.rs`, `src/core/state.rs`, `nix/module.nix` | yes — every other phase needs login to actually boot |
| 02 — Parallelize `steam login` + skip-bounce | [02-parallel-login-and-reuse.md](./02-parallel-login-and-reuse.md) | 5.5 medium | 01 | `src/game/steam.rs`, `src/lib.rs` | no |
| 03 — Docs refresh for the new login lifecycle | [03-docs-refresh.md](./03-docs-refresh.md) | 5.5 low | 02 | `docs/src/configuration/steam-credentials.md`, `docs/src/SUMMARY.md`, `docs/src/planning/stabilization-and-hardening-research.md` | no |

## Parallelism layer

| Wave | Runs | Why this wave is safe in parallel | Unlocks |
|------|------|------------------------------------|---------|
| 0 | Phase 01 alone | Phase 01 is the only thing that can ship before login boots end-to-end. Without it, Phases 02 and 03 cannot be validated against real behavior. | 02, 03 |
| 1 | Phase 02 alone | Phase 02 rewrites the serial loop in `steam::login`; 03 depends on documenting the resulting behavior, so they serialize even though their file sets are disjoint. | 03 |
| 2 | Phase 03 alone | Final wave. Docs codify the lifecycle established by 01 and the UX established by 02. | plan exhausted |

There is no parallel sub-layer split inside any phase. Each phase is one
coherent stream of work owned by one agent session.

## External repo coordination

Every phase's working tree is `/data/nvme0/can/Projects/steampipe`; no
phase requires a commit in another repo. The closest cross-repo touch is
that real-host validation in Phases 01 and 02 exercises files installed
from canix at `/etc/steampipe/login-runners` and `/var/lib/steampipe/`,
but those are read-only consequences of the canix NixOS rebuild, not
edits this plan asks the agent to make. If a kvm-group change is needed
on the atlas host (Phase 01 step 4), edit
`/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix`
(adding `users.users.can.extraGroups = [ "kvm" ];` or equivalent) and
deploy via `canix` — record that change in the Phase 01 PR description,
but it does not belong inside the steampipe plan commits.

## PR sequencing & cross-owner coordination

Within steampipe, the plan crosses three subsystems owned by the same
maintainer (caniko): the Rust orchestrator (`src/core/`, `src/game/`),
the NixOS module (`nix/module.nix`), and the docs tree (`docs/`).
Sequencing matters because:

- Phase 01 lands changes that span `src/core/` and `nix/module.nix` in
  one PR — keep them together so a reviewer can verify the
  Rust-vs-module contract (which file paths cluster-ctl probes, what
  the assertion warns about) in a single diff.
- Phase 02 is `src/game/` only; rebase it onto the merged Phase 01
  before opening a PR so the parallel loop actually runs against the
  fixed virtiofsd lifecycle.
- Phase 03 is `docs/` only; rebase onto merged Phase 02 so the prose
  describes shipped behavior. No code review burden, but waits on the
  prior two so claims are accurate.

There are no other owners to coordinate with for this plan.

## Whole-set acceptance criteria

The plan is shipped only when **all** of these hold:

- [ ] `cluster-ctl steam login all` exits 0 against an 8-VM cluster from a cold
      state (every VM was `cluster-ctl down` first) within ≤ 4 minutes
      wall-clock on the atlas host.
- [ ] `cluster-ctl steam login all` exits 0 with no VMs re-booted when the
      cluster is already up and every account is already logged in (
      "fill-the-gaps" still works; `--force` re-logs them in place).
- [ ] No `Failed to connect to 'vm-N-virtiofs-steam-state-vm-N.sock'` errors in
      any per-VM `vm.log` in `$XDG_STATE_HOME/steampipe/<cluster>/<vm>/vm.log`.
- [ ] `cluster-ctl down` cleanly stops both `microvm@<vm>` *and* the
      corresponding virtiofsd process for every login-class VM.
- [ ] `cargo test -p steampipe` passes (no test regressions; new tests for the
      virtiofsd lifecycle and the parallel login loop land green).
- [ ] [docs/src/configuration/steam-credentials.md](../../configuration/steam-credentials.md)
      describes the new lifecycle (operator must be in `kvm` group, login runs
      in parallel, reboot is skipped when reachable).

## Global constraints

- **Working tree:** `/data/nvme0/can/Projects/steampipe` for every phase.
- **Build:** `nix build .#cluster-ctl` (or `cargo build --release -p steampipe`
  inside the dev shell) before each phase's acceptance run.
- **Real-host validation:** the atlas host (`/var/lib/steampipe/logins`,
  `/etc/steampipe/login-runners`) is the only honest validator. Local Docker
  or Local backends do not exercise the virtiofsd path.
- **No new emoji, no rewriting tests just to make them pass, no
  `--no-verify`:** standard repo hygiene applies to every phase.
- **Don't regress fill-the-gaps semantics** in
  [src/game/steam.rs:581-606](../../../src/game/steam.rs#L581-L606) — Phase 02
  must keep them intact when going parallel.

## Reference

- Research dossier (root-cause analysis, evidence inventory):
  [../headless-steam-login-research.md](../headless-steam-login-research.md).
- Prior stabilization research:
  [../stabilization-and-hardening-research.md](../stabilization-and-hardening-research.md)
  — confirms no overlapping in-flight tracks; the
  `steam-login-fill-gaps-findings` work has already shipped and is the
  baseline this plan extends.
- Upstream `microvm.nix` virtiofsd-share convention:
  `nix/lib/microvm-runner.nix` and the generated `bin/virtiofsd-run` /
  `bin/microvm-run` siblings under `/etc/steampipe/login-runners/<vm>/`.
