## Goal And Trigger

Operator ran `cluster-ctl steam login all` on the atlas host expecting a quick,
headless Steam login across every VM. `vm-1` failed within 180 s with
`SSH timeout for vm-1` and the `vm.log` tail:

```
microvm@vm-1: -chardev socket,id=fs0,path=vm-1-virtiofs-steam-state-vm-1.sock:
Failed to connect to 'vm-1-virtiofs-steam-state-vm-1.sock':
No such file or directory
```

The lease subsystem also logged `[lease] vm-2.claim PID 287714 is dead (cluster
'default'); reclaiming.` immediately before. We want:

1. To understand why login is failing.
2. A reliable, fast, **parallel** headless login path for all VMs.

This dossier collects the evidence so a follow-on implementation plan can act.

## Current Reality

### What `cluster-ctl steam login all` does today

End-to-end, serial, one VM at a time
([src/game/steam.rs:551-622](../../../src/game/steam.rs#L551-L622)):

1. `resolve_targets("all", …)` picks the configured VM set
   ([src/game/steam.rs:560](../../../src/game/steam.rs#L560)).
2. Per-VM "fill-the-gaps" check via `accounts::classify_session_status` reads
   the host-side login state and skips VMs whose tokens are still `Ok`
   ([src/game/steam.rs:583-606](../../../src/game/steam.rs#L583-L606)).
3. For each remaining VM, calls `login_single_vm`
   ([src/game/steam.rs:767-842](../../../src/game/steam.rs#L767-L842)):
   - `backend.stop_instance(config, vm)` — kills any prior runner.
   - `admit().await` — host-memory admission gate
     ([src/core/admission.rs:51-53](../../../src/core/admission.rs#L51-L53)).
   - `backend.start_instance(config, vm, login_runners_dir)` — spawns
     `<login_runners_dir>/vm-N/bin/microvm-run` as a child process
     ([src/core/backend.rs:243-277](../../../src/core/backend.rs#L243-L277)).
   - `lease::write_claim(...)` — persists PID claim
     ([src/vm/lease.rs:60-70](../../../src/vm/lease.rs#L60-L70)).
   - `backend.wait_ready(&vm.ip, LOGIN_SSH_TIMEOUT_SECS=180)` — SSH polls every
     500 ms → 4 s, capped at 180 s
     ([src/core/backend.rs:195-211](../../../src/core/backend.rs#L195-L211),
     [src/game/steam.rs:12](../../../src/game/steam.rs#L12)).
   - On success, `automated_login` (`creds` present) runs the SteamCMD
     tmux bootstrap + GUI validation cascade
     ([src/game/steam.rs:980-1027](../../../src/game/steam.rs#L980-L1027)).
   - On any failure, cleans up the VM and exits this VM's iteration.

There is no parallelism in the login loop — VMs are processed strictly
sequentially ([src/game/steam.rs:581-616](../../../src/game/steam.rs#L581-L616)).

### What the login runner actually executes

For canix's atlas host, `/etc/steampipe/host.json` reports
`loginRunnersDir = /etc/steampipe/login-runners`, which is a symlink farm to
per-VM runner scripts produced by `nix/lib/login-runners.nix`.

The currently-deployed runner is
`/nix/store/mqf7y17bwcclgaaifz3avvdar0iwadff-microvm-vm-1-microvm-run`. Reading
its body shows a QEMU command line that includes:

```
-chardev 'socket,id=fs0,path=vm-1-virtiofs-steam-state-vm-1.sock'
-device  'vhost-user-fs-pci,chardev=fs0,tag=steam-state-vm-1'
```

(Verified: `bin/microvm-run` for the deployed login runner, plus the runner at
`/nix/store/48rfb30np1anh48daxwhzbaxb9cfl6rc-steampipe-login-vm-runners/vm-1/bin/microvm-run`.)

`microvm-run` does nothing but `exec qemu …`. QEMU is *eager* on `-chardev
socket` — it tries to connect immediately and aborts when the socket file is
absent. That is precisely the error in `vm.log`.

The runner farm also ships a *sibling* binary,
`/etc/steampipe/login-runners/vm-1/bin/virtiofsd-run`, which execs
`supervisord` with a config that launches
`virtiofsd-steam-state-vm-1`. That virtiofsd binary opens the socket and serves
`/var/lib/steampipe/logins/vm-1`:

```
exec virtiofsd \
  --socket-path=vm-1-virtiofs-steam-state-vm-1.sock \
  --socket-group=kvm \
  --shared-dir=/var/lib/steampipe/logins/vm-1 \
  …
```

Nothing in the codebase ever invokes `virtiofsd-run`.
`backend.start_instance` only knows about `microvm-run`
([src/core/backend.rs:259-274](../../../src/core/backend.rs#L259-L274)). No
NixOS service unit is installed by `nix/module.nix` to start either side; the
module only installs the link-farm under `/etc/steampipe/login-runners`
([nix/module.nix:545-548](../../../nix/module.nix#L545-L548)) and a
`systemd.tmpfiles.rules` line for the state dirs
([nix/module.nix:497-503](../../../nix/module.nix#L497-L503)).

This is the root cause: the runner expects an out-of-band virtiofsd process
that nobody starts.

### Why regular runners do not hit this

`nix/lib/host-runners.nix` builds runners with `loginStateDir` left as `null`
([nix/lib/host-runners.nix:19](../../../nix/lib/host-runners.nix#L19),
[nix/lib/microvm-runner.nix:14](../../../nix/lib/microvm-runner.nix#L14)). The
share list at
[nix/lib/microvm-runner.nix:102-109](../../../nix/lib/microvm-runner.nix#L102-L109)
is gated on `shareSteamState`, so non-login runners declare no virtiofs share
and microvm.nix generates no `-chardev`/`virtiofsd-run`. That is why
`cluster-ctl up`, `steam warm`, and `steam check` work today on these VMs even
without virtiofsd orchestration.

Only `nix/lib/login-runners.nix` flips the flag by defaulting
`loginStateDir = /var/lib/steampipe/logins`
([nix/lib/login-runners.nix:14](../../../nix/lib/login-runners.nix#L14)). It is
the *only* runner class that depends on virtiofsd, and it is broken on every
invocation.

### Why the stale-lease line is incidental

`reclaiming` is the lease subsystem doing exactly what it is supposed to do:
remove a `vm-2.claim` file whose PID is no longer alive
([src/vm/lease.rs:155-202](../../../src/vm/lease.rs#L155-L202),
[src/vm/lease.rs:176-180](../../../src/vm/lease.rs#L176-L180)). The eprintln is
audit logging, not an error. It happens during `resolve_targets`/`reserve_n`
when probing the lock dir. It does not influence the virtiofs failure at all.
The `default` cluster name comes from the absence of `--cluster` on the
command line.

### What `auto_login` already gets right

`steam::auto_login` (called from `Commands::Up` at
[src/lib.rs:840-842](../../../src/lib.rs#L840-L842)) shows the parallel
shape that `steam login` lacks. It iterates with `par_each_vm`
([src/game/steam.rs:920-945](../../../src/game/steam.rs#L920-L945),
[src/core/config.rs:100-115](../../../src/core/config.rs#L100-L115)) and
delegates per-VM SteamCMD bootstrap + GUI validation to `automated_login`
against already-running VMs. It works because `up` boots the *regular* runners
(no virtiofs share). It is exactly the right shape for "headless login across
all VMs in parallel" — the only thing missing is that the regular runners do
not persist Steam state to the host.

## Evidence Inventory

| Artifact | What it proves |
|----------|----------------|
| `/home/can/.local/state/steampipe/default/vm-1/vm.log` | QEMU exits before SSH because `vm-1-virtiofs-steam-state-vm-1.sock` is missing. Two-line file, single error. |
| `/etc/steampipe/login-runners/vm-1/bin/microvm-run` → store path `mqf7y17b…-microvm-vm-1-microvm-run` | Currently-deployed login runner is QEMU and expects the steam-state socket. |
| `/etc/steampipe/login-runners/vm-1/bin/virtiofsd-run` → store path `41m9jrjb…-microvm-vm-1-virtiofsd-run` | A separate supervisord-managed virtiofsd script exists per VM but is never invoked. |
| `/nix/store/3z3pjk87…-vm-1-virtiofsd-supervisord.conf` | Supervisord config has `[program:virtiofsd-steam-state-vm-1]` only — the right share, but no producer in the orchestrator. |
| `/nix/store/57l6p0nbf7nzvn51xr20dkxs79cf9kbk-virtiofsd-steam-state-vm-1` | The virtiofsd binary uses `--socket-group=kvm` and `--shared-dir=/var/lib/steampipe/logins/vm-1`. |
| [src/core/backend.rs:243-277](../../../src/core/backend.rs#L243-L277) | `microvm_start` only spawns `bin/microvm-run`; no virtiofsd orchestration. |
| [src/game/steam.rs:551-622](../../../src/game/steam.rs#L551-L622) | `steam::login` iterates serially over VMs. |
| [src/game/steam.rs:900-970](../../../src/game/steam.rs#L900-L970) | `steam::auto_login` already runs `par_each_vm` against already-running VMs. |
| [src/vm/lease.rs:176-180](../../../src/vm/lease.rs#L176-L180) | Stale-lease reclaim is informational and unrelated to the boot failure. |
| [nix/lib/microvm-runner.nix:102-109](../../../nix/lib/microvm-runner.nix#L102-L109) | Virtiofs share is gated on `loginStateDir != null` — only login runners declare it. |
| [nix/lib/host-runners.nix:19](../../../nix/lib/host-runners.nix#L19) | Regular runners pass `loginStateDir = null` so they have no virtiofs share. |
| [nix/lib/login-runners.nix:14](../../../nix/lib/login-runners.nix#L14) | Login runners default `loginStateDir = /var/lib/steampipe/logins` and so always declare the share. |
| [nix/module.nix:497-548](../../../nix/module.nix#L497-L548) | Module installs the link-farm and tmpfiles dirs but **does not** install any `microvm@vm-N` or `virtiofsd@vm-N` systemd unit. |
| `/var/lib/steampipe/logins/vm-1` (`drwx------ can:users`) | The user that runs `cluster-ctl` *can* read the share dir without elevation. |
| `getent group kvm` → `kvm:x:302:` (empty); `id can` → no `kvm` membership | Today the operator user cannot satisfy virtiofsd's `--socket-group=kvm` chgrp call, so a naive "just also exec virtiofsd-run" fix would fail. |
| [docs/src/configuration/steam-credentials.md:58-63](../../../docs/src/configuration/steam-credentials.md#L58-L63) | Design intent: Steam writes its real client state into the writable virtiofs share so state survives down/up. The login runner is the only class that lives up to this. |
| [nix/module.nix:528-535](../../../nix/module.nix#L528-L535) | `schemaVersion = 2` comment confirms the move from "boot-time read-only injection" to "one writable Steam share" — the current orchestration is incomplete relative to that design. |
| [docs/src/planning/stabilization-and-hardening-research.md:8-12](./stabilization-and-hardening-research.md#L8-L12) | Prior plan sets `login-runner-ssh-fix` and `steam-login-fill-gaps-findings` already shipped — they do not cover the virtiofsd lifecycle. This gap is new ground. |

Commands run during research:

```bash
ls -la /etc/steampipe/login-runners/vm-1/bin/
readlink /etc/steampipe/login-runners/vm-1/bin/microvm-run
readlink /etc/steampipe/login-runners/vm-1/bin/virtiofsd-run
cat /home/can/.local/state/steampipe/default/vm-1/vm.log
cat /etc/steampipe/module.json
ls -ld /var/lib/steampipe /var/lib/steampipe/logins /var/lib/steampipe/logins/vm-1
id can; getent group kvm
grep -rn "loginRunners\|virtiofsd" nix/module.nix
grep -n "shares\|virtiofs\|loginStateDir" nix/lib/microvm-runner.nix
```

## Existing Plan Status

The only related planning prose lives in
[docs/src/planning/stabilization-and-hardening-research.md](./stabilization-and-hardening-research.md).
The `steam-login-fill-gaps-findings` track in that document already shipped
(force flag + classify-and-skip in `accounts.rs` / `steam.rs`) and the
`login-runner-ssh-fix` track is retired. Neither covers the virtiofsd
lifecycle. There is no stale or active plan to carry forward for this bug.

## Work That Should Survive

- Fill-the-gaps semantics in `steam::login`
  ([src/game/steam.rs:581-606](../../../src/game/steam.rs#L581-L606)) are
  correct and worth preserving — any new parallel flow should keep skipping
  VMs whose tokens are still `Ok` unless `--force` is set.
- `par_each_vm` + `admit().await` is the established pattern for safe parallel
  VM operations
  ([src/core/admission.rs:51-53](../../../src/core/admission.rs#L51-L53),
  [src/core/config.rs:100-115](../../../src/core/config.rs#L100-L115)) — reuse
  rather than reinvent.
- Lease identity verification with PID + cmdline fingerprint
  ([src/vm/lease.rs:155-202](../../../src/vm/lease.rs#L155-L202)) is robust;
  the noisy reclaim line is harmless and a useful audit signal.
- The split between login-state on the host (`loginStateDir/vm-N`) and the VM
  is the intended design
  ([docs/src/configuration/steam-credentials.md:58-63](../../../docs/src/configuration/steam-credentials.md#L58-L63)).
  Do not regress that for a speed win.

## Blockers And Missing Artifacts

- **No virtiofsd lifecycle in cluster-ctl.** Producer:
  `src/core/backend.rs::microvm_start`. The orchestrator that spawns
  `microvm-run` must also start `virtiofsd-run` first (and wait for the socket
  to appear), or there must be a host-level systemd unit pair that supervises
  both. Validation:
  `cluster-ctl steam login vm-1` reaches `Waiting for SSH... ready`.
- **kvm group access.** virtiofsd's `--socket-group=kvm` requires the running
  user to be in `kvm` so the socket chgrp succeeds. `can` is not in `kvm`
  today. Producer: canix host config (add `users.users.can.extraGroups =
  [ "kvm" ];` or equivalent), or drop `--socket-group=kvm` for direct
  user-level runs by overriding the virtiofsd script. Validation:
  `id can | grep -q kvm` and a successful `virtiofsd-run` invocation that
  creates the socket without `chgrp` errors.
- **`/etc/steampipe/host.json` exists; `/etc/steampipe/module.json` is legacy.**
  Module currently writes the legacy `module.json` path
  ([nix/module.nix:512](../../../nix/module.nix#L512)). The schema allows both
  ([src/core/nixos_module.rs:18-27](../../../src/core/nixos_module.rs#L18-L27))
  so this is not blocking, but the field naming mismatch in the trace output
  may confuse follow-up debugging.

## Risks And Constraints

- **virtiofsd permissions.** Running virtiofsd as the operator user (the
  natural choice for `cluster-ctl`) requires `kvm` group membership for the
  `--socket-group=kvm` flag, or the flag has to be dropped/changed. Both QEMU
  and virtiofsd run as the operator anyway, so the group is decorative for
  this deployment; we can override it.
- **Process group / cleanup.** `microvm_start` uses `process_group(0)` so the
  child group is isolated
  ([src/core/backend.rs:269-272](../../../src/core/backend.rs#L269-L272)).
  Virtiofsd has to be in the same lifecycle: if microvm dies we should kill
  virtiofsd, and `stop_instance` must reap both. The current
  `microvm_stop` only kills `microvm@<name>` by name
  ([src/core/backend.rs:289-291](../../../src/core/backend.rs#L289-L291)) — it
  needs to also kill the supervisord process or per-share virtiofsd.
- **Socket directory.** QEMU opens the chardev relative to its CWD, which
  `microvm_start` sets to `microvm_state_dir(state_dir, vm)`
  ([src/core/backend.rs:261](../../../src/core/backend.rs#L261)). virtiofsd
  must create the socket in the same directory, i.e. be invoked with the same
  `current_dir`. The supervisord script does not chdir, so wrapper code must
  set CWD.
- **Parallel boot resource pressure.** 8 login VMs at 4 GiB each = 32 GiB of
  guest commit. `admit()` throttles via
  `memory-admission`
  ([src/core/admission.rs:51-53](../../../src/core/admission.rs#L51-L53)),
  but per-VM `current_dir` separation already keeps socket files non-colliding.
  Concurrent QEMU starts will exercise the admission gate hard for the first
  time on this code path — `auto_login` already does the same via `up` so this
  is not new ground.
- **`auto_login` does not persist host-side state.** If we try to retire the
  separate login runner and use the regular runner for `steam login`, we lose
  the writable virtiofs share. The state would live in the VM's home image,
  not in `/var/lib/steampipe/logins/vm-N`, breaking the "survives down/up"
  promise documented in
  [docs/src/configuration/steam-credentials.md:58-63](../../../docs/src/configuration/steam-credentials.md#L58-L63).
  The fix must either keep two runner classes or backfill the regular runner
  with the writable share.
- **Stale leases under parallel kills.** If a parallel login pass crashes the
  cluster-ctl process before claims are removed, the `[lease] … reclaiming`
  line will appear next run — that is correct behavior and should stay loud.

## Candidate Next Steps

These are sequencing hints for the follow-on plan, not commitments.

1. **Make login runners actually boot** (must-fix; unblocks everything else).
   Two viable shapes:
   - **(A) cluster-ctl spawns virtiofsd-run.** In
     `microvm_start`
     ([src/core/backend.rs:243-277](../../../src/core/backend.rs#L243-L277)),
     after computing `runner`, look for sibling
     `runners_dir/<vm>/bin/virtiofsd-run`. If present, spawn it in the same
     `current_dir` and `env`, wait for the expected socket(s) to appear, then
     spawn `microvm-run`. Extend `microvm_stop`
     ([src/core/backend.rs:279-295](../../../src/core/backend.rs#L279-L295))
     to kill the virtiofsd PID too. Track virtiofsd PID in
     `state::write_pid` or a sibling file. **Trade-off:** keeps cluster-ctl
     self-contained, no host-side service install. virtiofsd inherits the
     operator user, so the `kvm` socket-group has to be patched out or the
     operator added to `kvm`.
   - **(B) Per-VM systemd services.** Have `nix/module.nix` install
     `systemd.services."microvm@vm-N"` and
     `systemd.services."virtiofsd@vm-N"` (the pattern microvm.nix uses in its
     own host module). cluster-ctl then `systemctl start microvm@vm-N` instead
     of spawning the runner. **Trade-off:** matches upstream microvm.nix
     conventions and runs virtiofsd as root by default (no kvm group issue),
     but couples cluster-ctl to systemd and changes the operator UX for
     `up`/`down`/`steam login`.
2. **Parallelize `steam::login`**. Convert the loop at
   [src/game/steam.rs:581-616](../../../src/game/steam.rs#L581-L616) to
   `par_each_vm` (same shape as `auto_login`
   [src/game/steam.rs:920-945](../../../src/game/steam.rs#L920-L945)),
   gated by `admit().await` per VM. Preserve fill-the-gaps and `--force`
   semantics; aggregate per-VM results into a single summary at the end.
3. **Decide whether to keep two runner classes.** If we accept option (1A)
   and patch the virtiofsd lifecycle, the existing split (regular vs. login
   runners) is fine. Alternatively, drop the separation and add the writable
   `steam-state` share to the regular runner so a single `cluster-ctl up
   --with-login` does everything. Cleaner UX, larger blast radius. Open
   decision below.
4. **Stop spawning a fresh VM per login when one is already running.**
   `login_single_vm` always `stop_instance`s then `start_instance`s
   ([src/game/steam.rs:787-792](../../../src/game/steam.rs#L787-L792)). If the
   target VM is already up and reachable, skip the bounce and just run the
   SteamCMD bootstrap. Big speed-up when "login all" is used right after `up`.
5. **Update docs.** Once (1) is decided,
   [docs/src/configuration/steam-credentials.md](../../../docs/src/configuration/steam-credentials.md)
   should describe the new lifecycle (which user runs virtiofsd, kvm
   membership, parallel behavior). The
   [stabilization-and-hardening-research.md](./stabilization-and-hardening-research.md)
   entry for steam login should reference this dossier so the prior fill-gaps
   findings don't get re-litigated.

Sequencing: (1) blocks everything; (2) and (4) are independent and can land
together once (1) is in; (3) is a separate architectural decision; (5)
follows.

## Open Decisions For The User

- **Virtiofsd orchestration shape: in-process (1A) or systemd-managed (1B)?**
  (1A) preserves the current `cluster-ctl` UX and is the minimum change. (1B)
  matches upstream microvm.nix conventions and avoids the `kvm` group dance,
  but turns `steam login` into a privileged-ish operation that talks to
  systemd. Both are viable; the choice frames the implementation plan.
- **Collapse runner classes (one runner with writable steam-state share for
  everyone) or keep the split?** Collapsing simplifies the codebase but
  rebuilds every host runner with virtiofs, which means every `cluster-ctl up`
  now needs virtiofsd. If we go with (1A) this is essentially free once the
  lifecycle works; if (1B), it adds N more systemd units per host.
- **Should `steam login all` skip the reboot when the VM is already
  reachable?** Today it always bounces the VM. Skipping is faster and matches
  what `auto_login` already does, but it changes the meaning of `--force`
  (does `--force` imply VM reboot too, or only re-login?).
