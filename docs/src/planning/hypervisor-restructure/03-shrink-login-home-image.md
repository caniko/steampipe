# Phase 03 — Shrink login-runner home image + add `cluster-ctl reset --home`

> **Recommended Codex model: GPT 5.5 low**
>
> Two mechanical changes: a per-class number in
> `microvm-runner.nix` and a small new subcommand that `rm`s a file
> after checking a lease. No design calls, no log interpretation,
> no cross-file architectural reasoning. `medium` would over-think
> the subcommand naming; `low` ships the right shape directly.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase:

1. Newly built login-runner home images are 2 GiB (down from 8 GiB).
2. Runtime-runner home images are unchanged at 8 GiB.
3. Operators can run `cluster-ctl reset <vm> --home` to delete an
   existing on-disk home image so the new 2 GiB default takes effect
   on next VM boot.

## Why this matters now

Login runners hold only Steam config + login state in
`/home/cluster` — typically 300-600 MiB. The 8 GiB reservation is
load-bearing only because nothing has bothered to shrink it. At
vmCount = 8, this reclaims 48 GiB of atlas's `$XDG_STATE_HOME`
footprint with no functional loss.

Independent of the hypervisor migration. Ships in Wave 0.

The cluster-ctl helper exists because existing `vm-N-home.img`
files on atlas are already 8 GiB. The new 2 GiB default only
applies to newly created images; operators must remove the old ones
to reclaim disk. Hand-running `rm` is fine but error-prone (you
can shoot a running VM in the foot); a guarded subcommand is the
right shape.

## Out of scope

- Touching runtime runners' home image size. Stays 8 GiB.
- Moving Steam state to a virtiofs/9p share. That's Path B from the
  design, explicitly deferred.
- Touching `mkTestCluster` (test-cluster.nix) login runners — those
  inherit from `microvm-runner.nix`'s parameterisation by way of
  the `loginStateDir != null` discriminator.

## Plan

1. In [nix/lib/microvm-runner.nix](../../../nix/lib/microvm-runner.nix),
   the `volumes` block (currently a hardcoded `size = 8192`):

   ```nix
   volumes = [
     {
       mountPoint = "/home/${vmUser}";
       image = "${name}-home.img";
       size = if shareSteamState then 2048 else 8192;
     }
   ];
   ```

   `shareSteamState` is already in scope at
   [nix/lib/microvm-runner.nix:14](../../../nix/lib/microvm-runner.nix#L14)
   (it's `loginStateDir != null`). Using it as the login-vs-runtime
   discriminator avoids threading a new parameter.

   This matches the design's volume sizing decision.

2. Add the `cluster-ctl reset` subcommand:

   - In the clap CLI definition (search for the existing
     subcommand enum — likely in `src/ui/cli.rs` near `DisplayMode`,
     or in a dedicated `src/cli/` module), add:

     ```rust
     /// Remove on-disk VM state (home image) so next boot starts
     /// fresh.
     Reset {
         /// VM name (e.g. "vm-1").
         vm: String,
         /// Remove the home image. Required.
         #[arg(long)]
         home: bool,
     },
     ```

   - The handler lives in `src/game/lifecycle.rs` (or a new
     `src/game/reset.rs` if `lifecycle.rs` is already crowded):

     ```rust
     pub fn reset_vm_home(
         state_dir: &Path,
         lock_dir: &Path,
         cluster_name: &str,
         vm_name: &str,
     ) -> anyhow::Result<()> {
         // Check the VM is not running. lease module returns the
         // current claim PID if any.
         if let Some(claim) = lease::read_claim(vm_index(vm_name)?, lock_dir, cluster_name)? {
             if process_alive(claim.pid) {
                 anyhow::bail!(
                     "{}: VM is running (PID {}). Run `cluster-ctl down {}` first.",
                     vm_name, claim.pid, vm_name
                 );
             }
         }
         let home = state_dir.join(vm_name).join(format!("{vm_name}-home.img"));
         if !home.exists() {
             println!("{}: no home image at {}", vm_name, home.display());
             return Ok(());
         }
         std::fs::remove_file(&home).with_context(|| format!("remove {}", home.display()))?;
         println!("{}: removed {}", vm_name, home.display());
         Ok(())
     }
     ```

   - Add `--home` as a required flag (or split out subcommands like
     `reset home`, `reset state`) so the gesture is unambiguous and
     forward-compatible. Required-flag is simpler — pick it.

3. Add a unit test in the same file:

   ```rust
   #[test]
   fn reset_home_removes_file_when_no_claim() {
       let tmp = tempdir().unwrap();
       std::fs::create_dir(tmp.path().join("vm-1")).unwrap();
       std::fs::write(tmp.path().join("vm-1/vm-1-home.img"), b"fake").unwrap();
       let lock = tempdir().unwrap();
       reset_vm_home(tmp.path(), lock.path(), "default", "vm-1").unwrap();
       assert!(!tmp.path().join("vm-1/vm-1-home.img").exists());
   }
   ```

   Skip the live-VM-detection test (mocking PIDs is hard); the
   handler's guarded path is exercised manually.

4. Document the new subcommand in
   `docs/src/configuration/vm-resources.md` (created in Phase 09)
   **OR** if vm-resources.md doesn't exist yet, add a short note to
   `docs/src/configuration/vm-lifecycle.md` for now and let Phase 09
   move it.

5. Run the verification suite:

   ```bash
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --check
   nix flake check --no-build
   ```

6. Commit:

   ```
   microvm: shrink login-runner home image to 2 GiB; add cluster-ctl reset --home

   Login runners hold Steam config + login state only (~500 MiB
   typical); 8 GiB was wasted. Runtime runners stay at 8 GiB
   because game content lands there. Adds `cluster-ctl reset
   <vm> --home` so operators can retire existing 8 GiB images
   without hand-rming a file that might belong to a running VM.
   ```

## Acceptance criteria

- [ ] `nix-build` of a login-runner derivation produces a runner
      script whose home-image initialisation references `size =
      2048` (search for `truncate -s 2048M` or `size=2048M` in
      `microvm-run`).
- [ ] `nix-build` of a runtime-runner derivation still produces
      `size = 8192`.
- [ ] `cluster-ctl reset vm-1 --home` deletes
      `$XDG_STATE_HOME/steampipe/<cluster>/vm-1/vm-1-home.img` when
      the VM is not running.
- [ ] `cluster-ctl reset vm-1 --home` refuses (non-zero exit, clear
      message) when the VM is running according to the lease.
- [ ] `cargo test --workspace` is green, including the new
      `reset_home_removes_file_when_no_claim` test.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is
      clean.

## Files likely touched

- `nix/lib/microvm-runner.nix` — one line change in `volumes[].size`.
- `src/ui/cli.rs` (or `src/cli/`) — add `Reset` subcommand variant.
- `src/game/lifecycle.rs` (or new `src/game/reset.rs`) — handler +
  test.
- `src/lib.rs` — dispatch the new subcommand from the CLI entry
  point.
- `docs/src/configuration/vm-lifecycle.md` — short usage note
  (relocates in Phase 09).

## Pitfalls

- **`lease::read_claim` signature**: verify what the existing module
  already exposes before sketching the call. `[lease] vm-N.claim
  PID X is dead` log lines suggest the module already knows PID
  liveness. Use the existing accessor; don't re-invent.
- **Cluster name resolution**: `cluster-ctl reset` runs outside any
  `--cluster` flag context by default. Pull the cluster name from
  the same place other subcommands do (probably an env var or
  default `"default"`).
- **Don't shrink to less than 2 GiB**: Steam's session updates
  occasionally swell the config dir; 2 GiB is the design's chosen
  number with ~3-7× headroom over typical 300-600 MiB usage.
  Tighter risks future Steam updates filling the disk.
- **`shareSteamState` vs explicit class flag**: the discriminator
  `loginStateDir != null` is a proxy for "this is a login runner".
  If someone later adds a runtime-runner path that also passes
  `loginStateDir != null`, the home size shrinks unexpectedly.
  Document the implicit contract with a comment.
- **Existing 8 GiB images stay on disk** until an operator runs the
  new reset command. That's fine, just confusing. Mention it in
  the commit message.

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Final Disk Budgets", "Rust Code Changes" → `cluster-ctl reset`).
- Current home-image declaration:
  [nix/lib/microvm-runner.nix:74-80](../../../nix/lib/microvm-runner.nix#L74-L80).
- `shareSteamState` definition:
  [nix/lib/microvm-runner.nix:14](../../../nix/lib/microvm-runner.nix#L14).
