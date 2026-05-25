# Phase 03 — Host-level login runners + NixOS-managed SSH key

> **Recommended Codex model: GPT 5.5 high**
>
> The non-trivial design content of the plan lives here. Three coupled
> decisions need to be held in one head: where the SSH keypair lives
> on disk and which systemd unit creates it (without race conditions
> against the login wizard); how to parameterize `mkMicroVM` so the
> existing `nix/lib/test-cluster.nix` builder and a new host-level
> builder share code without duplicating definitions; and which file
> path to publish for `loginRunnersDir` so it stays valid across
> generations of `nixos-rebuild switch`. None of those is impossible
> for a smaller tier, but the failure mode is a subtly broken module
> that boots fine but races on first login. Worth high.

## Working tree

`/data/nvme0/can/Projects/steampipe` (the steampipe repository).

## Goal

`services.steampipe-cluster.loginRunners.enable = true` causes the
NixOS module to (1) generate and own an SSH keypair under
`/var/lib/steampipe/ssh/`, (2) build a host-level link-farm of
`microvm-run` wrappers for each VM index, and (3) publish both the
runners directory and the SSH private-key path into
`/etc/steampipe/module.json` as `loginRunnersDir` and `sshKey`
respectively. The option defaults to `false`. The existing project
flake's `loginVMRunnersDir` builder is unchanged in this phase (phase
06 migrates it).

## Why this matters now

Phase 02 made the CLI capable of consuming `loginRunnersDir` from the
host config. This phase populates that field. After this phase lands
and atlas has rebuilt, `cluster-ctl steam login` from `$HOME` works
end-to-end — phase 04 proves it.

The user decided on **NixOS-managed SSH keys** (dossier blocker 1):
the module generates and owns the keypair. This is the load-bearing
design call for this phase.

## Out of scope

- Removing or refactoring the project-flake `loginVMRunnersDir`
  builder ([nix/lib/test-cluster.nix:268-277](../../../../nix/lib/test-cluster.nix#L268-L277)).
  That's phase 06.
- Updating canix to enable the option on atlas. That's phase 04.
- Schema reader changes (done in phase 01) and CLI resolver changes
  (done in phase 02).
- Building host-level **regular** runners (the 2GB game runners used
  by `steam check`/`steam warm`). The dossier left this optional. If
  the agent has surplus capacity, add it as a follow-up sub-option
  (`services.steampipe-cluster.runners.enable`); otherwise defer to a
  future phase. **Default decision: skip it.** Phase 02 may have added
  the `runnersDir` schema field; this phase publishes only
  `loginRunnersDir`.

## Plan

1. **Extract the login-runner builder.** Create
   [nix/lib/login-runners.nix](../../../../nix/lib/login-runners.nix)
   (new file). The function signature:

   ```nix
   { pkgs, lib, nixpkgs, steampipe }:
   { vmCount
   , subnet ? "10.0.100"
   , prefix ? 24
   , vmUser ? "cluster"
   , sshAuthorizedKey   # string: in-VM authorized public key
   , hypervisor ? "crosvm"
   , graphics ? true
   , memory ? 4096
   , useSystemdStage1 ? false
   }:
   ...
   ```

   The body mirrors [nix/lib/test-cluster.nix:120-277](../../../../nix/lib/test-cluster.nix#L120-L277)
   for `mkMicroVM` + `loginVMRunnersDir`, but takes
   `sshAuthorizedKey` as a string argument instead of reading from a
   `projectRoot + sshKey` path. Returns a `linkFarm` named
   `steampipe-login-vm-runners` with `vm-1`..`vm-N` entries, each
   pointing at a microVM runner derivation.

   Keep `mkMicroVM` itself out of `nix/lib/test-cluster.nix` — define
   a private `mkMicroVM` inside `login-runners.nix` *or* lift the
   shared bits into a third file
   (`nix/lib/microvm-runner.nix`) that both consumers import. The
   second option is cleaner and avoids drift between project-level
   and host-level runners. **Decision: lift to
   `nix/lib/microvm-runner.nix`.** Phase 06 will be smaller as a
   result.

2. **SSH key generation via systemd.** In
   [nix/module.nix](../../../../nix/module.nix), when
   `cfg.loginRunners.enable`:

   ```nix
   systemd.services.steampipe-loginrunners-ssh-key = {
     description = "Generate steampipe login-runner SSH keypair";
     wantedBy = [ "multi-user.target" ];
     before = [ "steampipe-gen-credentials.service" ];
     serviceConfig = {
       Type = "oneshot";
       RemainAfterExit = true;
       UMask = "0077";
     };
     script = ''
       set -euo pipefail
       keydir=/var/lib/steampipe/ssh
       mkdir -p "$keydir"
       chmod 0700 "$keydir"
       if [ ! -f "$keydir/cluster_key" ]; then
         ${pkgs.openssh}/bin/ssh-keygen -t ed25519 -N "" \
           -C "steampipe-loginrunners@$(hostname)" \
           -f "$keydir/cluster_key"
       fi
       ${lib.optionalString (cfg.tapOwner != null) ''
         chown -R ${cfg.tapOwner} "$keydir"
       ''}
     '';
   };

   systemd.tmpfiles.rules = [
     "d /var/lib/steampipe 0755 root root -"
     "d /var/lib/steampipe/ssh 0700 ${loginStateOwner} users -"
   ];
   ```

   The pubkey path is `/var/lib/steampipe/ssh/cluster_key.pub`; the
   private key is `/var/lib/steampipe/ssh/cluster_key`. Both must be
   owned by `tapOwner` (the user that runs `cluster-ctl`) so the CLI
   can read the private key without `sudo`.

   The keypair must exist **before** the login-runner link-farm is
   exposed to the CLI for the first call. Because the runner
   derivation embeds the **public** key (it goes into the in-VM
   `authorized_keys`), the build itself reads the pubkey. **This is
   the design crux of the phase.** Two options:

   - **(a) Read at activation time.** The systemd service generates
     the keypair, then runs a small helper to rebuild a per-system
     activation symlink to a runner-dir derivation that was already
     built with a placeholder key. Brittle — runners would not
     authenticate.

   - **(b) Build the runners after the keypair exists.** Use a
     `nixos-rebuild`-time activation script that:
       1. Generates the keypair if missing (same `ssh-keygen` as the
          oneshot, run inline during activation).
       2. Reads the pubkey.
       3. Triggers a `nix-build` of the login-runners derivation
          parameterized by that pubkey, symlinking the result into
          `/etc/steampipe/login-runners`.

     This avoids the chicken-and-egg by **moving keypair creation
     into the activation script**, ahead of the `nix-build` call.

   - **(c) Pre-generate at module evaluation time using
     `pkgs.writeText` + a deterministic key.** *Rejected* — embeds a
     well-known key in the world-readable store. Insecure.

   - **(d) Two-phase: generate keypair via activation, store pubkey
     in `/var/lib/steampipe/ssh/cluster_key.pub`, runners derivation
     consumes the pubkey *as a runtime file* via virtiofs/cloud-init
     (not baked into authorized_keys at build time).** Cleanest but
     requires changes to the in-VM `cluster-vm-base` module to read
     `/etc/ssh/authorized_keys/cluster` from a virtiofs share.

   **Recommendation: option (d)**, falling back to **option (b)** if
   the in-VM module is harder to change than expected. Document the
   chosen path in the commit message. Both keep the keypair off the
   Nix store.

3. **Module option additions.** In [nix/module.nix](../../../../nix/module.nix)
   inside `options.services.steampipe-cluster`:

   ```nix
   loginRunners = {
     enable = lib.mkEnableOption "host-level Steam login VM runners";
     hypervisor = lib.mkOption {
       type = lib.types.str;
       default = "crosvm";
       description = "Hypervisor for the login VMs (default: crosvm).";
     };
     graphics = lib.mkOption {
       type = lib.types.bool;
       default = true;
       description = "Enable virtio-gpu in login VMs.";
     };
     memory = lib.mkOption {
       type = lib.types.ints.positive;
       default = 4096;
       description = "Memory per login VM in MiB. Steam GUI needs ~4 GiB.";
     };
   };
   ```

   No `sshAuthorizedKey` option — the module owns the keypair.
   No `enable` cross-reference required to the parent
   `services.steampipe-cluster.enable` because the option lives
   inside that module's namespace.

4. **Wire the builder in `module.nix`.** In `config = lib.mkIf cfg.enable { ... }`:

   ```nix
   environment.etc."steampipe/login-runners" = lib.mkIf cfg.loginRunners.enable {
     source = (import ./lib/login-runners.nix {
       inherit pkgs lib nixpkgs;
       steampipe = self;   # or whatever the local-module self-ref is
     }) {
       inherit (cfg) vmCount subnet prefix;
       vmUser = "cluster";   # or cfg.vmUser if we add that option
       sshAuthorizedKey = builtins.readFile /var/lib/steampipe/ssh/cluster_key.pub;
       hypervisor = cfg.loginRunners.hypervisor;
       graphics = cfg.loginRunners.graphics;
       memory = cfg.loginRunners.memory;
     };
   };
   ```

   **If using option (d) from step 2**, this becomes:

   ```nix
   environment.etc."steampipe/login-runners".source = ...;
   # The login-runners derivation does NOT bake the SSH key.
   # The in-VM cluster-vm-base reads
   # /run/steampipe/authorized_keys (virtiofs from host) instead.
   ```

   Update the `mkMicroVM` virtiofs `shares` list to include the
   keypair-share when the runner is built for host-level login.

5. **Publish `loginRunnersDir` and `sshKey` in `module.json`.** At
   [nix/module.nix:212-233](../../../../nix/module.nix#L212-L233),
   extend the JSON object:

   ```nix
   environment.etc."steampipe/module.json".text = builtins.toJSON ({
     # ... existing fields ...
   } // lib.optionalAttrs cfg.loginRunners.enable {
     loginRunnersDir = "/etc/steampipe/login-runners";
     sshKey = "/var/lib/steampipe/ssh/cluster_key";
   } // {
     vmUser = "cluster";  # always emit so the CLI doesn't fall back to a hardcoded default
   });
   ```

   Do **not** bump `schemaVersion` — the fields are additive
   (phase 01 made the reader accept them).

6. **Tests.** Add a `nix flake check` step:

   - `nix build .#nixosConfigurations.<test-host>.config.environment.etc."steampipe/module.json".source`
     produces a JSON file that, when parsed, contains `loginRunnersDir`
     and `sshKey` when the option is enabled, and lacks them when
     disabled.
   - The link-farm under `loginRunnersDir` contains
     `vm-1/bin/microvm-run` ... `vm-N/bin/microvm-run` where N matches
     `cfg.vmCount`.

   If the repo has no existing `flake check` test infrastructure for
   the module, write a small `tests/module-loginrunners.nix`
   referencing `nixosTest`; otherwise add to the existing test set.

7. **Document the option.** Append a section to
   [docs/src/configuration/host-config.md](../../../../docs/src/configuration/host-config.md)
   under "NixOS module walkthrough" describing:
   - What `services.steampipe-cluster.loginRunners.enable = true`
     does.
   - The SSH keypair location and ownership (`tapOwner`, mode 0700).
   - The 4 GiB-per-VM memory cost and how to reduce via the `memory`
     sub-option.

   (The doc cleanup phase 05 will rewrite the quick-start
   walkthroughs; phase 03 just documents the module surface.)

## Acceptance criteria

- [ ] `services.steampipe-cluster.loginRunners.enable = true` builds cleanly: `nix build .#nixosConfigurations.<test-host>.config.system.build.toplevel` succeeds.
- [ ] After activation, `/etc/steampipe/module.json` contains a non-null `loginRunnersDir` pointing at a path that exists, and `sshKey` pointing at `/var/lib/steampipe/ssh/cluster_key`.
- [ ] `ls -la /var/lib/steampipe/ssh/` shows the keypair with mode `0600` (private) and `0644` (public), owned by `tapOwner`.
- [ ] `loginRunnersDir` is a directory containing `vm-1/bin/microvm-run` through `vm-N/bin/microvm-run` for the configured `vmCount`.
- [ ] `nix flake check` passes; the new module test (if added) asserts both `enable = true` and `enable = false` shapes.
- [ ] `services.steampipe-cluster.loginRunners.enable = false` (the default) produces a `module.json` *without* `loginRunnersDir` or `sshKey` — the new fields are only emitted when the option is on.
- [ ] No regression: existing `services.steampipe-cluster.enable = true` (without the new sub-option) continues to produce schemaVersion-2 `module.json` with the same fields as before.
- [ ] `cluster-ctl steam info` (after phase 02 has landed) shows the new `loginRunnersDir` row sourced from the module on a host where the option is enabled.

## Files likely touched

- [nix/module.nix](../../../../nix/module.nix) — new module option(s), keypair systemd unit, module.json field emission.
- [nix/lib/login-runners.nix](../../../../nix/lib/login-runners.nix) — **new file**, host-level runner builder.
- [nix/lib/microvm-runner.nix](../../../../nix/lib/microvm-runner.nix) — **new file**, shared `mkMicroVM` definition lifted out of `test-cluster.nix`.
- [nix/lib/test-cluster.nix](../../../../nix/lib/test-cluster.nix) — only the import-and-delegate change for `mkMicroVM`. The project-level `loginVMRunnersDir` and wrapper scripts stay in place.
- [docs/src/configuration/host-config.md](../../../../docs/src/configuration/host-config.md) — module option docs (full walkthrough rewrite is phase 05).
- Possibly `tests/module-loginrunners.nix` or whatever the project's nixosTest convention is.

## Pitfalls

- **The chicken-and-egg of SSH key + Nix build.** This is the
  pre-mortem axis. If the agent picks option (b) from step 2 and
  doesn't sequence the activation script correctly, the first
  `nixos-rebuild switch` succeeds but the runners contain a stale
  pubkey, and the CLI fails to SSH in. Option (d) avoids this. If
  the agent picks (b), the activation must explicitly *re-run*
  `nix-build` after generating the key, and the systemd unit must
  fail loudly if the build fails. Don't paper over with `|| true`.
- **`tapOwner == null` failure mode.** The current module already
  warns when `tapOwner` is null
  ([nix/module.nix:189-190](../../../../nix/module.nix#L189-L190))
  because `loginStateDir` becomes root-owned. The same warning
  applies here — if `tapOwner` is null, the SSH key directory is
  also root-owned and `cluster-ctl` running as the user can't read
  it. Add a `loginRunners.enable && tapOwner == null` assertion that
  bails the build (don't just warn).
- **microVM closure pinning.** The login-runner derivation pins a
  full NixOS closure (Sway + Steam + virtio-gpu). On atlas this is
  absorbed by `/nix/store`; on smaller hosts this is heavy. Keep the
  option default `false` (the user already decided).
- **`steampipe = self` self-reference.** When importing
  `login-runners.nix` from inside `module.nix`, the `steampipe`
  argument needs to resolve to the same flake input the project
  uses for `nixosModules.cluster-vm-base` etc. Verify this resolves
  in the flake-parts wiring and isn't a free variable.
- **Schemaversion stickiness.** Resist the urge to bump to v3. The
  decision is documented in the README; if you find a reason to
  bump, *stop* and re-route the user.
- **Idempotency of the keypair systemd unit.** The script must not
  regenerate the key on every boot — only when missing. Otherwise
  every `nixos-rebuild switch` invalidates already-logged-in Steam
  sessions on the VMs. Use the `[ ! -f "$keydir/cluster_key" ]`
  guard exactly as shown in step 2.
- **Authority of the runner-dir symlink.** `environment.etc."steampipe/login-runners"`
  with `source = <derivation>` creates a symlink to `/nix/store/...`.
  If the agent reaches for `text = ...` or `target = ...` they're in
  the wrong nixos-option doc; only `source = ...` produces the
  link-farm pass-through.

## Risk profile

Not routed to `max`, so the formal risk/strategy sections from
`multi-phase-plan` are optional. Listed here for completeness because
the design pivot in step 2 is genuine:

- **R1: SSH key option (d) requires in-VM module changes.** If
  `cluster-vm-base` doesn't currently support a virtiofs-mounted
  `authorized_keys`, the agent may need a small parallel change to
  the in-VM module. Acceptable within phase 03; do not split into a
  sub-layer.
- **R2: First-boot race.** The keypair must exist before
  `cluster-ctl steam login` is invoked. Atlas's smoke test (phase 04)
  surfaces this — the phase-04 acceptance criteria require a fresh
  reboot before the smoke run, not just `nixos-rebuild switch`.

## Reference

- Dossier: [../cli-configless-from-nixos-research.md](../cli-configless-from-nixos-research.md) — see "Open Decisions For The User > SSH-key source" and "Work That Should Survive > C. Publish login runners from the NixOS module".
- Current project-level login-runners builder (model to mirror): [nix/lib/test-cluster.nix:120-277](../../../../nix/lib/test-cluster.nix#L120-L277).
- Current `module.json` emission site: [nix/module.nix:212-233](../../../../nix/module.nix#L212-L233).
- Phase 01 (schema fields this phase populates): [01-host-config-schema-fields.md](./01-host-config-schema-fields.md).
- Phase 04 (smoke test of this phase's output): [04-canix-wiring-and-smoke.md](./04-canix-wiring-and-smoke.md).
- Phase 06 (migrates project wrappers to delegate here): [06-migrate-project-wrappers.md](./06-migrate-project-wrappers.md).
