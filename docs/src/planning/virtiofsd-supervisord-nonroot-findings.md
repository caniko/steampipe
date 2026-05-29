## Goal And Trigger

`cluster-ctl steam login all` (after `cluster-ctl down`) fails for every VM with:

```
Error with vm-N: virtiofsd-run for vm-N never produced its sockets — if
the log shows EPERM/chgrp errors, add the operator user to the kvm group
…
==> Logged in: 0, Skipped: 0, Failed: 8
```

The hint in the error points at the wrong cause: the EPERM/kvm path documented
in the stable login-state guidance is not what is actually failing here. The
user wants the real root cause identified and fixed.

## Root Cause(s)

`microvm_start` in
[src/core/backend.rs:266-296](../../../src/core/backend.rs#L266-L296)
spawns the per-VM `bin/virtiofsd-run` as the operator user. But the
microvm.nix-generated `bin/virtiofsd-run` is a thin `supervisord`
wrapper:

```
# /nix/store/<hash>-microvm-crosvm-vm-1/bin/virtiofsd-run
exec /nix/store/…-python3.13-supervisor-4.3.0/bin/supervisord \
  --configuration /nix/store/…-vm-1-virtiofsd-supervisord.conf
```

And the corresponding `…-virtiofsd-supervisord.conf` declares:

```
[supervisord]
nodaemon=true
user=root
```

`supervisord` refuses to switch to a different effective user when it is
not started as root. The recorded `virtiofsd.log` for every failing VM
on this host (`~/.local/state/steampipe/default/vm-N/virtiofsd.log`)
contains exactly:

```
Error: Can't drop privilege as nonroot user
For help, use /nix/store/…/supervisord -h
```

So the wrapper exits in < 100 ms, no socket is ever created, and the
10-second poll in `wait_for_virtiofs_sockets`
([src/core/backend.rs:413-435](../../../src/core/backend.rs#L413-L435))
times out for every VM in the cluster.

A secondary issue is also present and will block the *next* failure
mode once supervisord is bypassed: the underlying
`virtiofsd-steampipe-ssh-authorized` script
([/nix/store/…-virtiofsd-steampipe-ssh-authorized](file:///nix/store/gi8jqpj7fj46vjahxy1b587iq1zrdqfb-virtiofsd-steampipe-ssh-authorized))
runs `virtiofsd --socket-group=kvm …`, and the host operator user
`can` is not in the `kvm` group:

```
$ id
uid=1000(can) gid=100(users) groups=100(users),1(wheel),27(dialout),
57(networkmanager),67(libvirtd),969(pink_raven),992(openrazer),
996(gamemode)
```

That is the failure mode the existing `microvm_start` error string
*does* describe, but it is downstream of the supervisord failure and
will only manifest after the supervisord blocker is removed.

## Evidence

Commands run during the investigation:

```bash
git log --oneline -20                                # commits 69424ad, 08ea98f
rg -n virtiofsd src/core/backend.rs src/core/state.rs
find ~/.local/state/steampipe/default -name virtiofsd.log
cat  ~/.local/state/steampipe/default/vm-1/virtiofsd.log
find /nix/store -name 'virtiofsd-run' | head
cat  /nix/store/lwkwxvbqw403llvrxi78qvb433drl193-microvm-crosvm-vm-1/bin/virtiofsd-run
cat  /nix/store/za7im43vrgjynxc4ahfd9ybf2zd96xyz-vm-1-virtiofsd-supervisord.conf
cat  /nix/store/gi8jqpj7fj46vjahxy1b587iq1zrdqfb-virtiofsd-steampipe-ssh-authorized
id
```

Key artifacts:

| Artifact | What it proves |
|----------|----------------|
| `~/.local/state/steampipe/default/vm-N/virtiofsd.log` (all 8 VMs identical) | `supervisord` exits immediately with `Can't drop privilege as nonroot user` — the chgrp/EPERM path documented in the code's error hint is never reached. |
| `…/microvm-crosvm-vm-1/bin/virtiofsd-run` | The wrapper does nothing except `exec supervisord --configuration <store-path>`. |
| `…/vm-1-virtiofsd-supervisord.conf` | `[supervisord] user=root` is hard-coded by microvm.nix; the file is read-only in the nix store. |
| `…/virtiofsd-steampipe-ssh-authorized` | The actual virtiofsd invocation; uses `--socket-group=kvm` and `--shared-dir=/var/lib/steampipe/ssh-authorized`. |
| [src/core/backend.rs:266-296](../../../src/core/backend.rs#L266-L296) | The orchestrator spawns the wrapper directly as the operator user with no privilege handling. |
| `id` (operator `can`) | Not in the `kvm` group — the secondary blocker that will surface after the primary fix. |

The earlier lifecycle work correctly identified the kvm-group problem, now
documented in [Steam Credentials](../configuration/steam-credentials.md#login-vm-lifecycle),
but did **not** anticipate that the upstream `bin/virtiofsd-run`
wrapper is a `supervisord` invocation hard-coded to run as root.
That is why the diagnostic hint in `microvm_start` points the
operator at the wrong remediation.

## Status

**Code fix landed** (uncommitted) in this working tree:

- [src/core/backend.rs](../../../src/core/backend.rs):
  `microvm_start` now extracts the `[program:*] command=…` paths from
  the per-VM supervisord conf referenced by `bin/virtiofsd-run` and
  spawns each program directly, bypassing supervisord. Multiple
  programs per VM are supported. On socket-timeout the error includes
  the last few lines of `virtiofsd.log`.
- [src/core/state.rs](../../../src/core/state.rs): virtiofsd PID slot
  widened from a single value to a newline-separated list
  (`read_virtiofsd_pids` / `write_virtiofsd_pids` / `remove_virtiofsd_pids`).
  The reader accepts the legacy single-line shape so an in-place
  upgrade doesn't leak processes.
- Tests added: `parse_supervisord_conf_path_*`,
  `parse_supervisord_program_commands_*`, `tail_log_*`,
  `virtiofsd_pids_roundtrip`, `virtiofsd_pids_handles_single_line_legacy_format`.
- `cargo test --lib`, `cargo clippy --all-targets -- -D warnings`,
  `cargo build --release --bin cluster-ctl` all green at HEAD.

**Host side (option A from the original recommendation)**: canix
already declares `users.users.can.extraGroups = ["kvm"];` at
[`root/hosts/atlas/features/steampipe.nix:12`](file:///data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix#L12)
and the system has applied it
(`getent group kvm` → `kvm:x:302:can`). The operator's *current*
shell session predates the change, so `id` does not list kvm and
any process spawned from it inherits the same gap. A fresh login
(or `loginctl terminate-session $XDG_SESSION_ID`) is required for
the `--socket-group=kvm` chgrp inside virtiofsd to succeed.

## Smoke test the operator should run

```bash
# In a *fresh* login session so kvm is in the process credentials:
id | grep -q kvm || { echo "still no kvm — relogin first"; exit 1; }

# Install the patched cluster-ctl via canix the usual way, then:
cluster-ctl down
cluster-ctl --vm-count 1 steam login vm-1 --force

# Expected:
ls ~/.local/state/steampipe/default/vm-1/*-virtiofs-*.sock        # socket present
tail ~/.local/state/steampipe/default/vm-1/virtiofsd.log          # no supervisord errors
cluster-ctl down
pgrep -af 'virtiofsd-steampipe-ssh-authorized'                    # nothing
```

## Original Recommended Fix (kept for reference)

Bypass the `supervisord` wrapper entirely. `cluster-ctl` already owns
the process lifecycle (PID files, group kill, cleanup); it does not
need an in-tree process supervisor on top.

Concrete change in
[src/core/backend.rs:266-296](../../../src/core/backend.rs#L266-L296)
(`microvm_start`):

1. Parse `bin/virtiofsd-run` to recover the
   `--configuration <supervisord.conf>` path.
2. Parse the supervisord conf to extract every
   `[program:*] command=<path-to-binary>` entry. (Skip
   `[eventlistener:*]` blocks — those exist only to call
   `systemd-notify --ready`, which is meaningless outside a
   systemd unit.)
3. For each program command, spawn that binary **directly** as a
   child of `cluster-ctl`, with the same `current_dir(&vm_dir)`,
   `env("XDG_RUNTIME_DIR", &runtime_dir)`, stdio redirection to
   `virtiofsd.log`, and `process_group(0)` that
   `microvm_start` already uses for the wrapper today.
4. Record **each** spawned PID so `microvm_stop` can reap them.
   Either widen the existing `<vm>.virtiofsd.pid` file to a
   newline-separated list (cheap) or add a sibling
   `<vm>.virtiofsd.pids` file. The existing `pkill -f
   'virtiofsd-steam-state-<vm>'` fallback in `microvm_stop`
   ([src/core/backend.rs:354-359](../../../src/core/backend.rs#L354-L359))
   still helps, but real cleanup must come from the recorded PIDs.
5. Keep the existing `wait_for_virtiofs_sockets` poll — it is
   producer-agnostic.
6. Update the error message that points at the kvm group to also
   surface the supervisord-wrapper failure mode; one option is to
   tail the last line of `virtiofsd.log` into the returned anyhow
   error.

After (1)–(6), the secondary `--socket-group=kvm` blocker still
applies on this host. The follow-up fix has two viable shapes that
the stable login-state guidance now documents; pick one before landing the code
change:

- **(A) Add `can` to the `kvm` group** via the canix host config
  (`users.users.can.extraGroups = [ "kvm" ];`), rebuild, re-login
  (`newgrp kvm` is not enough for spawned children of a long-lived
  shell). Lowest blast radius; matches the warning that
  [nix/module.nix:466-487](../../../nix/module.nix#L466-L487)
  already emits when `loginRunners.enable = true` and the operator
  is not in kvm.
- **(B) Implement the fallback that strips `--socket-group=kvm`**.
  Since step (3) above invokes the per-program binary directly,
  the cleanest place to drop the flag is in cluster-ctl's spawn
  call: read the program script, materialise the `virtiofsd …`
  arg vector, omit `--socket-group=kvm` if the operator is not in
  the `kvm` group, and `exec` virtiofsd directly. Avoid editing
  the read-only nix-store scripts in place.

(A) is strictly less code. (B) makes the headless flow work for
operators who cannot or will not change host group membership.
Both are compatible with the rest of the headless-login plan.

A single-VM smoke test once the fix lands:

```bash
cluster-ctl down
cluster-ctl --vm-count 1 steam login vm-1 --force
ls ~/.local/state/steampipe/default/vm-1/*-virtiofs-*.sock   # socket present
tail ~/.local/state/steampipe/default/vm-1/virtiofsd.log     # no supervisord errors
cluster-ctl down
pgrep -af 'virtiofsd-steampipe-ssh-authorized'               # nothing
```

## Optional Follow-Ups

- **Adjust the misleading error hint** in
  [src/core/backend.rs:289-295](../../../src/core/backend.rs#L289-L295)
  even before the bigger fix lands. Tail the last 1-2 lines of
  `virtiofsd.log` into the returned anyhow error so the next operator
  who hits this sees `Can't drop privilege as nonroot user` directly
  instead of an unrelated kvm-group hint. Cheap; helps anyone bisecting
  this commit later.
- **Tighten the NixOS module warning** at
  [nix/module.nix:466-487](../../../nix/module.nix#L466-L487) to also
  call out that microvm.nix's supervisord wrapper hard-codes
  `user=root`, so even a correctly-grouped operator cannot invoke
  `bin/virtiofsd-run` directly. This is a documentation-only nudge
  for the next time someone reads the module.

## Open Decisions

Resolved — operator picked (A) (kvm group via canix). canix already
applied the change; the only remaining action is a fresh login to
pick up the kvm group in the operator's shell.

## Verification on 2026-05-28

`cluster-ctl steam login all` failed again this morning with the same
"never produced its sockets" error. Re-verified the root cause:

1. `~/.local/state/steampipe/default/vm-1/virtiofsd.log` (mtime
   `2026-05-28 07:10:04`) still contains exactly
   `Error: Can't drop privilege as nonroot user` — the supervisord
   failure mode, unchanged.
2. The kvm-group blocker (the *secondary* issue) is **gone**:
   `id` for `can` now reports `… 302(kvm) …`. Fresh login picked up
   the canix-declared group, so option (A) above is fully landed on
   the host side.
3. The patched `cluster-ctl` is **still not deployed**. Forensics on
   the binary on `$PATH`:
   - `which cluster-ctl` →
     `/nix/store/fdfy7a00yjl7qbqbp23nq5s6cl6hlsgy-cluster-ctl-0.1.0/bin/cluster-ctl`.
   - Source the derivation was built from
     (`/nix/store/lqhgfj60mr24yrjss8wkiq74056wykch-source/src/core/backend.rs`)
     hashes to the contents of commit `69424ad`
     ("Start virtiofsd for login runners") — i.e. the commit that
     introduced the bug — and contains zero `[program:`
     parser markers.
   - The fix in this working tree
     ([src/core/backend.rs](../../../src/core/backend.rs) +
     [src/core/state.rs](../../../src/core/state.rs)) is still
     **uncommitted** (`git status` shows both files modified). Local
     `trunk` is `c061ae4`, identical to codeberg `refs/heads/trunk`.
   - canix `flake.lock` pins `steampipe` to `4ef2de8` (revCount 150),
     which is upstream of `69424ad` — so even a `canix deploy`
     against today's lock will *not* pick up the fix until both the
     fix is pushed and canix's lock is bumped.

So the diagnosis is unchanged; the reason the operator hit the same
wall is purely a deployment gap. No further code investigation is
warranted.

## Action To Unblock

Single short shell session, in order:

```bash
# 1. Land the fix in steampipe.
cd /data/nvme0/can/Projects/steampipe
cargo test --lib                       # confirm the new tests still pass
cargo clippy --all-targets -- -D warnings
git add src/core/backend.rs src/core/state.rs \
        docs/src/planning/virtiofsd-supervisord-nonroot-findings.md
git commit                              # use a message like:
#   "Bypass virtiofsd supervisord wrapper on non-root operator"
git push origin trunk

# 2. Bump canix to point at the fixed revision and deploy.
cd /data/nvme0/can/Projects/canix
nix flake update steampipe
# inspect: jq '.nodes.steampipe.locked.rev' flake.lock should be the new sha
canix deploy <host>                    # or the canix subcommand the operator uses

# 3. Smoke test from a fresh login shell (already in kvm group).
cluster-ctl down
cluster-ctl --vm-count 1 steam login vm-1 --force
ls ~/.local/state/steampipe/default/vm-1/*-virtiofs-*.sock   # socket present
tail ~/.local/state/steampipe/default/vm-1/virtiofsd.log     # no supervisord errors
cluster-ctl down
pgrep -af 'virtiofsd-steampipe-ssh-authorized'               # nothing
```

The `update-canix` skill matches this exact propagation pattern and
will do steps 2 mechanically if invoked after step 1 lands.
