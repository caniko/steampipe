# Phase 04 — canix wiring + atlas smoke test

> **Recommended Codex model: GPT 5.5 medium**
>
> External-repo edit on canix, followed by a real `nixos-rebuild
> switch` and an interactive smoke test against actual Steam VMs.
> Moderate complexity: the canix edit is mechanical, but the smoke
> interpretation requires reading `cluster-ctl steam info` discovery
> traces and confirming the VNC flow lands on the right login VM.
> 5.5 medium gives enough latitude to interpret the trace output
> without inflating to high for what is, fundamentally, "turn it on
> and check that it works".

## Working tree

- Primary: `/data/nvme0/can/Projects/canix` (the canix flake).
- Secondary (for smoke verification): `$HOME` on atlas — the user's
  shell, with no project root in scope.

## Goal

Atlas's `services.steampipe-cluster` configuration enables the
host-level login runners, `nixos-rebuild switch` succeeds, and from
`$HOME` (no project root, no flags) the command `cluster-ctl steam
login` boots the first VM through the Steam GUI login flow. This is
the end-to-end proof that the dossier's "config-less CLI from NixOS
module" claim holds.

## Why this matters now

Phases 01-03 build the capability; phase 04 proves it. Without this
phase, we have a feature on paper that has never been exercised on a
real host. The smoke also surfaces:

- Whether the SSH key sequencing from phase 03 step 2 is correct
  under a fresh reboot.
- Whether the schemaVersion 1 deprecation warning trips users who
  have not yet `nixos-rebuild switch`-ed (atlas's current
  `/etc/steampipe/module.json` is v1 per the dossier).
- Whether `cluster-ctl steam info` renders the new
  `loginRunnersDir` row correctly.

## Out of scope

- Documentation updates — phase 05.
- Project-wrapper migration — phase 06.
- Any change to phases 01-03 — if a smoke failure requires code
  changes, file them as a bug against the relevant phase. Do not
  edit phase 04's siblings from inside phase 04.

## Plan

1. **Bump the steampipe flake input in canix.** From inside
   `/data/nvme0/can/Projects/canix`:

   ```sh
   nix flake update steampipe
   ```

   Confirm the lock file picks up the commit(s) that land phases
   01-03. Inspect with:

   ```sh
   git diff flake.lock | grep -A 3 steampipe
   ```

   The `rev` should be the merge commit that closed phase 03.

2. **Enable the new option on atlas.** Edit
   [/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix](file:///data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix):

   ```nix
   services.steampipe-cluster = {
     enable = true;
     vmCount = 8;
     tapOwner = "can";
     tapOwnerUid = 1000;

     loginRunners.enable = true;   # <-- new line

     accounts = { ... unchanged ... };
   };
   ```

   No other canix changes. The option default for everything else
   (`hypervisor = "crosvm"`, `graphics = true`, `memory = 4096`) is
   correct for atlas.

3. **Build the new system closure dry-run.** Before activating:

   ```sh
   sudo nixos-rebuild build --flake .
   ```

   Expect the closure to grow by roughly the size of 8 microVM
   runner derivations. If the build fails on `readFile
   /var/lib/steampipe/ssh/cluster_key.pub`, phase 03 picked option
   (b) without the activation sequencing — file a phase-03 bug, do
   not work around it here.

4. **Activate.** Once the dry-run is clean:

   ```sh
   sudo nixos-rebuild switch --flake .
   ```

   Watch `journalctl -fu steampipe-loginrunners-ssh-key.service` in
   another shell. The unit should run once, generate the keypair,
   and stay `active (exited)`.

   Then **reboot atlas** to confirm the keypair survives and the
   runner symlink is recreated correctly:

   ```sh
   sudo reboot
   ```

   On boot, `ls -la /var/lib/steampipe/ssh/` should show the keypair
   intact, and `ls /etc/steampipe/login-runners/vm-1/bin/microvm-run`
   should be a working symlink.

5. **Inspect `module.json` and `cluster-ctl steam info`.** From
   `$HOME`:

   ```sh
   cat /etc/steampipe/module.json | jq '. | {schemaVersion, vmCount, loginRunnersDir, sshKey}'
   ```

   Expected:

   ```json
   {
     "schemaVersion": 2,
     "vmCount": 8,
     "loginRunnersDir": "/etc/steampipe/login-runners",
     "sshKey": "/var/lib/steampipe/ssh/cluster_key"
   }
   ```

   Then:

   ```sh
   cluster-ctl steam info
   ```

   The discovery trace should pick `/etc/steampipe/module.json`
   (legacy NixOS-module path, schemaVersion 2). The summary block
   should list `loginRunnersDir` as a separate row sourced from the
   host config.

6. **Run the smoke.** From `$HOME`:

   ```sh
   cluster-ctl steam login vm-1
   ```

   No flags. No project root. Expected:

   - The CLI does **not** bail with
     `--login-runners-dir is required when running outside a project
     root`.
   - The login VM boots via `microvm-run` from the host-level
     link-farm.
   - The Steam GUI appears under VNC; the wizard prompts for
     credentials (auto-supplied from
     `/etc/steampipe/credentials.toml` since
     `credentialsPath` is populated).
   - If Steam Guard kicks in, the wizard halts; complete with
     `cluster-ctl steam guard vm-1 --code <CODE>` from a separate
     shell (still from `$HOME`).
   - The wizard finishes and stops the VM cleanly.

   Capture stdout to a file for the acceptance evidence:

   ```sh
   cluster-ctl steam login vm-1 2>&1 | tee /tmp/steam-login-smoke.log
   ```

7. **Verify flag override still wins.** Without rebooting, run:

   ```sh
   cluster-ctl steam login vm-1 \
     --login-runners-dir /tmp/nonexistent-path
   ```

   Expected: CLI errors with a path-not-found message, **not** with
   "field not configured". This confirms the flag takes precedence
   over the module-supplied path (phase 02 precedence preserved).

8. **Confirm chessbender (or another project consumer) still
   works.** From the chessbender repo (or whichever project flake
   uses steampipe as an input):

   ```sh
   cd /data/nvme0/can/Projects/chessbender   # or wherever
   nix flake update steampipe
   nix run .#cluster-steam-login -- vm-1
   ```

   This still uses the project's bundled
   `loginVMRunnersDir`. Phase 06 will migrate it; for now we just
   prove it doesn't break.

9. **Commit + push the canix change.** Standard canix workflow (the
   user has canix-specific commit conventions; follow them).

## Acceptance criteria

- [ ] canix `flake.lock` updated; the steampipe input is at the phase-03 merge commit.
- [ ] `services.steampipe-cluster.loginRunners.enable = true` added to atlas's `features/steampipe.nix`.
- [ ] `sudo nixos-rebuild switch --flake .` succeeds on atlas without manual intervention.
- [ ] After a full reboot, `journalctl -u steampipe-loginrunners-ssh-key.service` shows the unit running once successfully (or being marked as already-completed), and `/var/lib/steampipe/ssh/cluster_key{,.pub}` exists with mode `0600`/`0644` owned by `can`.
- [ ] `/etc/steampipe/module.json` contains `schemaVersion: 2`, `loginRunnersDir`, `sshKey`, and all the previously emitted fields.
- [ ] `cluster-ctl steam info` from `$HOME` shows the host config sourced from `/etc/steampipe/module.json`, with a `loginRunnersDir` row marked as the source of that field. No schemaVersion-1 deprecation warning.
- [ ] `cluster-ctl steam login vm-1` from `$HOME` boots the login VM, exercises the Steam GUI, and exits cleanly. The log at `/tmp/steam-login-smoke.log` captures the run.
- [ ] `cluster-ctl steam login vm-1 --login-runners-dir /tmp/nonexistent-path` errors with a path-not-found message (proving flag precedence).
- [ ] `nix run .#cluster-steam-login -- vm-1` from chessbender (or any other project consumer) still works unchanged.

## Files likely touched

- `/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix` — one new line.
- `/data/nvme0/can/Projects/canix/flake.lock` — `nix flake update steampipe` output.
- `/tmp/steam-login-smoke.log` — captured smoke evidence (not committed; attach to PR description or paste into the verify run).

## Pitfalls

- **The schemaVersion-1 warning trap.** Before
  `nixos-rebuild switch` lands, atlas's `/etc/steampipe/module.json`
  is still v1. After activation, it becomes v2. If the user runs
  `cluster-ctl steam info` between `switch` and *reboot*, they may
  see stale parser state. Re-run after the reboot — or after
  `systemctl restart steampipe-gen-credentials.service` — to confirm
  v2 emission.
- **`tapOwner` / SSH key ownership.** If `tapOwner = "can"` but the
  oneshot ran before agenix mounted secrets (race), `chown` may
  fail. Phase 03's `after = ["agenix.service"]` should cover this;
  if you see permission errors, file a phase-03 bug.
- **Login state directory residue.** Atlas already has
  `/var/lib/steampipe/logins/vm-*/` populated from prior interactive
  logins. The new flow should **reuse** that state — Steam reads
  per-VM state via the existing virtiofs share. Do *not* `rm -rf`
  the login state dir to "start clean"; the user has already
  performed Steam Guard for those accounts and discarding the state
  forces a re-Guard.
- **Steam Guard challenges during smoke.** If `vm-1`'s session has
  expired, the smoke will pause for Guard input. That's a Steam
  behavior, not a phase-04 failure. Complete the Guard flow as
  documented and continue.
- **VNC port conflicts.** The login wizard spawns a VNC server. If
  another VNC instance is already bound to the same port (an
  abandoned previous run), the wizard hangs. `pkill -f vncserver`
  before starting if you see this.
- **Verifying without running the GUI.** If the smoke is being run
  over SSH without a Wayland session, the VNC viewer may not start.
  Run `cluster-ctl steam login vm-1` from a local terminal on
  atlas with a Wayland session active (`WAYLAND_DISPLAY` set), or
  use `--display headless` to bypass the viewer and complete via
  `cluster-ctl steam guard` from elsewhere.

## Reference

- Dossier: [../cli-configless-from-nixos-research.md](../cli-configless-from-nixos-research.md) — see "Planner Handoff > Acceptance evidence" (this phase's criteria mirror that list).
- canix atlas wiring: `/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix`.
- Phase 03 (the producer this phase consumes): [03-nixos-module-login-runners.md](./03-nixos-module-login-runners.md).
- Phase 02 (the CLI behavior this phase exercises): [02-cli-resolver-refactor.md](./02-cli-resolver-refactor.md).
