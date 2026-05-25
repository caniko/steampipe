# Phase 06 — Migrate project wrappers to host-level runners

> **Recommended Codex model: GPT 5.5 medium**
>
> Refactors `nix/lib/test-cluster.nix` so the
> `cluster-*-steam-login` wrappers delegate to host-level runners
> when `nixosIntegration = true`. Touches one Nix file in steampipe
> plus a verification pass against the chessbender consumer. Moderate
> complexity: the wrappers' shell logic is small but the conditional
> "use host-level or build locally" branching needs care so that
> existing non-NixOS users don't lose the project-level fallback.

## Working tree

- Primary: `/data/nvme0/can/Projects/steampipe` (the steampipe
  repository).
- Secondary (for verification): `/data/nvme0/can/Projects/chessbender`
  or whichever project flake consumes steampipe.

## Goal

`mkSteamLoginWrapper` in
[nix/lib/test-cluster.nix](../../../../nix/lib/test-cluster.nix)
detects whether the host has `services.steampipe-cluster.loginRunners.enable
= true` (via the existing `nixosIntegration = true` switch + a
runtime check for `/etc/steampipe/login-runners`) and delegates to
the host-level runners when present, falling back to the project-built
`loginVMRunnersDir` otherwise. Downstream consumers (chessbender,
etc.) don't need to update their flake invocations.

## Why this matters now

Phase 04 proved the host-level flow works for direct `cluster-ctl`
invocations. But the `nix run .#cluster-steam-login` wrappers still
unconditionally pass `--login-runners-dir ${loginVMRunnersDir}`
([nix/lib/test-cluster.nix:424, 453, 461](../../../../nix/lib/test-cluster.nix#L424)),
forcing every project to build its own 4 GiB-per-VM closure. That's
wasted build time and disk on hosts where the NixOS module already
publishes the runners. The user explicitly asked for migration
(dossier blocker 2 resolved as "migrate").

## Out of scope

- Removing the project-level `loginVMRunnersDir` build path entirely.
  Non-NixOS consumers still need it; the migration is delegation, not
  deletion.
- Touching `cluster-steam-test` or `cluster-steam-check` wrappers.
  Those use the *regular* `vmRunnersDir`, not login runners. A
  future phase can apply the same delegation pattern when the NixOS
  module starts emitting `runnersDir`.
- Changing the in-VM `cluster-vm-base` module (phase 03's territory).

## Plan

1. **Confirm phases 03 and 04 have landed.** The wrapper's
   delegation logic assumes `/etc/steampipe/login-runners` exists on
   hosts with the option enabled. Phase 04's smoke run is the proof.

2. **Rewrite `mkSteamLoginWrapper` in
   [nix/lib/test-cluster.nix](../../../../nix/lib/test-cluster.nix)
   (lines 347-357).** Current shape:

   ```nix
   mkSteamLoginWrapper = name: count: args:
     pkgs.writeShellScript "cluster-${name}" ''
       ${hostClusterEnv}
       credentials_args=()
       ${lib.optionalString (credentials == null) ''
         if [ -f ${systemCredentialsPath} ]; then
           credentials_args=(--credentials ${systemCredentialsPath})
         fi
       ''}
       exec ${ctl} --vm-count ${toString count} ${credsFlag} \
         "''${credentials_args[@]}" ${args} "$@"
     '';
   ```

   New shape — split into two helpers, one for fallback (current
   behavior) and one for delegation:

   ```nix
   mkSteamLoginWrapper = name: count: subcommand:
     pkgs.writeShellScript "cluster-${name}" ''
       ${hostClusterEnv}
       credentials_args=()
       ${lib.optionalString (credentials == null) ''
         if [ -f ${systemCredentialsPath} ]; then
           credentials_args=(--credentials ${systemCredentialsPath})
         fi
       ''}

       # Host-level login runners (when the NixOS module published them)
       # supersede the project-built runners. cluster-ctl resolves the
       # path from /etc/steampipe/module.json automatically; pass no
       # --login-runners-dir flag in that case.
       login_runners_args=()
       if [ -d /etc/steampipe/login-runners ] && [ "${if nixosIntegration then "1" else "0"}" = "1" ]; then
         : # cluster-ctl reads loginRunnersDir from module.json
       else
         login_runners_args=(--login-runners-dir ${loginVMRunnersDir})
       fi

       exec ${ctl} --vm-count ${toString count} ${credsFlag} \
         "''${credentials_args[@]}" \
         ${subcommand} "''${login_runners_args[@]}" "$@"
     '';
   ```

   Note: `subcommand` now excludes the `--login-runners-dir` flag —
   the call sites need adjustment in step 3.

3. **Update wrapper call sites** at
   [nix/lib/test-cluster.nix:424, 453, 461](../../../../nix/lib/test-cluster.nix#L424).

   Before:

   ```nix
   "cluster-1v1-steam-login" = mkSteamLoginWrapper "1v1-steam-login" 1
     "--cluster 1v1 steam login --login-runners-dir ${loginVMRunnersDir}";
   ```

   After:

   ```nix
   "cluster-1v1-steam-login" = mkSteamLoginWrapper "1v1-steam-login" 1
     "--cluster 1v1 steam login";
   ```

   Apply the same edit to `cluster-steam-login` (line 453) and the
   `nixosIntegration` block's `cluster-steam-login-persist` (line
   461). The `--login-runners-dir ${loginVMRunnersDir}` substring
   disappears from every call site — the helper now decides whether
   to inject it.

   For the `cluster-steam-guard` wrapper (lines 425, 454): no
   change. `steam guard` doesn't take `--login-runners-dir`.

4. **Audit `loginVMRunnersDir` lazy-build.** The current code builds
   `loginVMRunnersDir` unconditionally
   ([nix/lib/test-cluster.nix:271-277](../../../../nix/lib/test-cluster.nix#L271-L277)).
   Even when `nixosIntegration = true`, this still builds (just
   unused). On atlas this is wasted closure space.

   Make the build conditional:

   ```nix
   loginVMRunnersDir =
     if nixosIntegration
     then null   # host-level runners supplant project-level
     else pkgs.linkFarm "cluster-login-vm-runners" (...);
   ```

   Adjust the wrapper to handle the `null` case:

   ```nix
   else
     login_runners_args=(--login-runners-dir ${
       if loginVMRunnersDir == null
       then throw "loginVMRunnersDir is unavailable when nixosIntegration=true; rely on /etc/steampipe/login-runners instead"
       else loginVMRunnersDir
     })
   ```

   At Nix evaluation time, this `throw` only fires if the code path
   is reached, which the runtime `if` should prevent on a properly
   wired host. Belt-and-braces: if `/etc/steampipe/login-runners`
   doesn't exist on a `nixosIntegration = true` host, the wrapper
   fails loudly instead of falling back to a non-existent
   project-built dir.

5. **Update the wrapper's assertion at
   [nix/lib/test-cluster.nix:49-50](../../../../nix/lib/test-cluster.nix#L49-L50).**

   Today:

   ```nix
   assert lib.assertMsg (!(nixosIntegration && credentials != null))
   "credentials cannot be set when nixosIntegration is enabled - the NixOS module provides credentials";
   ```

   Add a second assertion for runtime self-consistency: the wrapper
   should not be evaluated with `nixosIntegration = true` on a host
   that doesn't actually run `services.steampipe-cluster.loginRunners
   = true`. That check is *runtime*, not eval-time, and lives in the
   wrapper's shell guard — but document it in a comment near the
   assertion.

6. **Smoke against chessbender (or equivalent).** From a project
   flake that uses `steampipe.lib.mkTestCluster { nixosIntegration =
   true; ... }`:

   ```sh
   cd /data/nvme0/can/Projects/chessbender   # or wherever
   nix flake update steampipe
   nix build .#cluster-tournament-up         # closure shrinks
   nix run .#cluster-steam-login -- vm-1
   ```

   Expected: the new closure no longer includes the 4 GiB-per-VM
   login runners (since `nixosIntegration = true` skips the build).
   The wrapper invokes `cluster-ctl steam login` and the host's
   `/etc/steampipe/login-runners` provides the runners.

   For a project with `nixosIntegration = false` (or unset), the
   wrapper still includes `--login-runners-dir`, the closure still
   contains the project-built login runners, and `nix run
   .#cluster-steam-login` works on a non-NixOS host.

7. **Document the delegation in `test-cluster.nix`.** Add a
   doc-comment near the top of `mkSteamLoginWrapper` explaining the
   precedence:

   ```nix
   # mkSteamLoginWrapper:
   # - When nixosIntegration is true AND /etc/steampipe/login-runners
   #   exists at runtime, the wrapper does not pass --login-runners-dir.
   #   cluster-ctl reads loginRunnersDir from /etc/steampipe/module.json.
   # - Otherwise, the wrapper passes --login-runners-dir pointing at the
   #   project-built linkFarm (loginVMRunnersDir).
   ```

## Acceptance criteria

- [ ] `mkSteamLoginWrapper` in `nix/lib/test-cluster.nix` no longer hardcodes `--login-runners-dir`; the flag is appended conditionally based on `nixosIntegration` and the runtime existence of `/etc/steampipe/login-runners`.
- [ ] `cluster-steam-login`, `cluster-1v1-steam-login`, and `cluster-steam-login-persist` wrapper call sites pass only `steam login` (no `--login-runners-dir ...` substring) at the eval level.
- [ ] When `nixosIntegration = true`, `loginVMRunnersDir` is not built (evaluates to `null`). Build closure size decreases by ~one microVM-runner-set worth.
- [ ] When `nixosIntegration = false` (or unset), the project still builds `loginVMRunnersDir` and the wrappers pass `--login-runners-dir` to `cluster-ctl`. This is the non-NixOS fallback.
- [ ] From the chessbender repo on atlas: `nix run .#cluster-steam-login -- vm-1` succeeds with the host-level runners (the chessbender flake input for steampipe is at the merge of phases 01-03).
- [ ] From a project flake with `nixosIntegration = false` (or running on a non-NixOS host): `nix run .#cluster-steam-login -- vm-1` still succeeds with the project-built runners.
- [ ] `nix flake check` from steampipe still passes; no new test failures or eval errors.

## Files likely touched

- [nix/lib/test-cluster.nix](../../../../nix/lib/test-cluster.nix) — `mkSteamLoginWrapper`, the three call sites at lines 424/453/461, and `loginVMRunnersDir` lazy-build.
- `/data/nvme0/can/Projects/chessbender/flake.lock` (or equivalent) — `nix flake update steampipe` to pick up the new wrapper.

## Pitfalls

- **Runtime vs. eval-time confusion.** The condition "host has
  `services.steampipe-cluster.loginRunners` enabled" can't be
  detected at Nix eval time from a project flake — the project
  doesn't know the host's NixOS config. Use the runtime `[ -d
  /etc/steampipe/login-runners ]` check inside the shell wrapper.
  The `nixosIntegration` flag is the eval-time hint that the
  consumer *expects* host-level runners.
- **`throw` vs. silent fallback.** If `nixosIntegration = true` but
  `/etc/steampipe/login-runners` is missing at runtime, the safest
  behavior is to fail loudly, not silently fall back to the project
  builder (which we made `null` in step 4). Phase 06 deliberately
  errors out — the user has explicitly told us they want migration,
  so missing host wiring is a bug, not a fallback case.
- **Closure shrinkage may not register on first build.** Old
  `loginVMRunnersDir` derivations may still be in the project's
  flake.lock-cached output paths. `nix store gc` afterwards to
  measure the real reduction; otherwise just rely on `nix path-info
  -S .#cluster-steam-login` for the live closure size.
- **Wrappers that build under `nix flake check`.** Some `nix flake
  check` paths instantiate every wrapper. Make sure
  `mkSteamLoginWrapper` evaluates cleanly under both
  `nixosIntegration = true` and `nixosIntegration = false`. The
  conditional `loginVMRunnersDir = null` must not blow up
  `nix-instantiate`.
- **chessbender SSH key drift.** If chessbender's
  `nix/test-cluster/cluster_key.pub` differs from atlas's
  NixOS-managed `/var/lib/steampipe/ssh/cluster_key.pub`, the
  host-level runners (which embed atlas's key) won't authenticate
  for chessbender's project SSH key when chessbender's `steam test`
  workflow tries to ssh in *afterwards*. The login flow itself
  works because Steam doesn't need the project SSH key, but
  chessbender's downstream `steam check` would fail until
  chessbender adopts the host key. Document this; phase 06 doesn't
  fix it (chessbender's own SSH-key handling is the project's
  call).
- **Don't touch `cluster-steam-test`/`cluster-steam-check`
  wrappers.** They use `vmRunnersDir`, which is the regular game
  runner set — not the login runners. Out of scope.

## Reference

- Dossier: [../cli-configless-from-nixos-research.md](../cli-configless-from-nixos-research.md) — see "Open Decisions For The User > Deprecation policy" (resolved as "migrate").
- Current wrapper call sites: [nix/lib/test-cluster.nix:424, 453, 461](../../../../nix/lib/test-cluster.nix#L424).
- `mkSteamLoginWrapper` definition: [nix/lib/test-cluster.nix:347-357](../../../../nix/lib/test-cluster.nix#L347-L357).
- Phase 03 (provides `/etc/steampipe/login-runners`): [03-nixos-module-login-runners.md](./03-nixos-module-login-runners.md).
- Phase 04 (smoke evidence the host-level runners work): [04-canix-wiring-and-smoke.md](./04-canix-wiring-and-smoke.md).
