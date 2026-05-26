# Phase 02 — Drop the dead `pathExists` default and assert loudly

> **Recommended Codex model: GPT 5.5 medium**
>
> Mechanical Nix edits plus one design call (assertion vs warning, and
> what bootstrap workflow we promise instead). The phase touches both
> module code and user-facing docs, so the agent has to keep the two
> consistent. Moderate complexity, sub-agent role — `medium` is the
> match. `low` would risk leaving the docs and the module out of sync;
> `high` is overkill for ~50 lines of Nix and ~30 lines of markdown.

## Working tree

`/data/nvme0/can/Projects/steampipe` (this repo).

## Goal

After this phase, the steampipe NixOS module no longer pretends that
`builtins.pathExists "/var/lib/steampipe/ssh/cluster_key.pub"` can ever
return `true` under flakes. Operators who enable `loginRunners` or
`runners` without supplying `sshAuthorizedKey` get a hard eval-time
error that tells them exactly what to do, rather than a successful
rebuild that silently builds unreachable VMs. The installation docs
describe the new bootstrap dance honestly.

## Why this matters now

The findings dossier
([../login-runner-ssh-unreachable-findings.md](../login-runner-ssh-unreachable-findings.md))
proved the default reader is dead under flake pure-eval. The current
fallback behavior — empty `authorized_keys`, runner builds, sshd
rejects every key — is the worst possible failure mode: silent,
delayed, and only diagnosable by `grep`-ing the VM nixos closure.

The existing warnings at
[nix/module.nix:404-407](../../../nix/module.nix#L404-L407) suggest a
future rebuild will fix the state, which is a lie under flakes.

The dead code lives at:

- [nix/module.nix:251-282](../../../nix/module.nix#L251-L282) — the
  `loginRunners.sshAuthorizedKey` option with the `pathExists` default
  + `defaultText` that promises auto-reading from the host key.
- [nix/module.nix:303-319](../../../nix/module.nix#L303-L319) — the
  same shape for `runners.sshAuthorizedKey`.
- [nix/module.nix:399-407](../../../nix/module.nix#L399-L407) — the
  warnings that mislead operators into thinking a second rebuild will
  fix things.
- [docs/src/getting-started/installation.md:63-65](../../getting-started/installation.md#L63-L65)
  — the "run a second rebuild" guidance.

## Out of scope

- The host-side activation script that generates the keypair
  (`nix/module.nix:61-77` and the `systemd.services.steampipe-loginrunners-ssh-key`
  unit at `nix/module.nix:489-499`). It still has a job: bootstrapping
  the keypair on first activation so operators have something to paste.
- AF_VSOCK SSH (option 3 from the findings dossier). That's a much
  larger refactor and isn't blocking this fix.
- Runtime virtiofs revival (option 2 from the findings dossier).
  Commit 4ee68f0 documents why it was broken; reintroducing it as
  the fix path is explicitly off the table.
- Rotating existing keys on shipped hosts — that's the operator's
  responsibility once the explicit-string mechanism is in place.
- The flake check that codifies this new behavior — that's P3.

## Plan

1. Remove the `pathExists`/`readFile` default and `defaultText` from
   both options in `nix/module.nix`:

   - `services.steampipe-cluster.loginRunners.sshAuthorizedKey` —
     change `default = if builtins.pathExists ... then ... else "";`
     to `default = "";` and simplify the `defaultText`/description to
     say "required when `loginRunners.enable = true`".
   - Same for `services.steampipe-cluster.runners.sshAuthorizedKey`.
   - Leave the option type as `lib.types.str`.

2. Promote the empty-key warnings at `nix/module.nix:399-407` into
   hard assertions in the `assertions = [ ... ]` block earlier in the
   `config` section (`nix/module.nix:362-395`). The new assertions:

   ```nix
   {
     assertion = !cfg.loginRunners.enable || cfg.loginRunners.sshAuthorizedKey != "";
     message = ''
       services.steampipe-cluster.loginRunners.enable is true but
       sshAuthorizedKey is empty. Login VMs would build with an empty
       authorized_keys file and reject every SSH connection.

       Bootstrap workflow:
         1. Set loginRunners.enable = false; (or leave the cluster
            service unconfigured), nixos-rebuild switch. This runs the
            activation script that generates the cluster keypair at
            /var/lib/steampipe/ssh/cluster_key{,.pub}.
         2. Read /var/lib/steampipe/ssh/cluster_key.pub and paste its
            single-line contents (no trailing newline) into
            services.steampipe-cluster.loginRunners.sshAuthorizedKey.
         3. Re-enable loginRunners and rebuild. The pubkey now lands in
            the runner closure and SSH works.

       This indirection exists because flake pure-eval cannot read
       /var/lib/* at evaluation time.
     '';
   }
   ```

   Add the same shape for `runners.enable` + `runners.sshAuthorizedKey`.

3. Delete the corresponding entries from the `warnings` list at
   `nix/module.nix:399-407` (the two `lib.optional` branches that
   warn about empty `sshAuthorizedKey`). They are now superseded by
   the assertions.

4. Update
   [nix/lib/host-runners.nix:46-53](../../../nix/lib/host-runners.nix#L46-L53)
   — the comment that says "Empty key produces empty authorized_keys;
   runners build but are unreachable. This is the intentional
   first-rebuild state..." is no longer accurate. Either remove the
   comment entirely or rewrite it as "Empty key is impossible: the
   module asserts this at eval time."

5. Rewrite
   [docs/src/getting-started/installation.md:57-72](../../getting-started/installation.md#L57-L72).
   Replace the misleading "Run the first rebuild ... if loginRunners
   or runners is enabled, run a second rebuild" sequence with the
   honest workflow:

   ```markdown
   4. First rebuild — bootstrap the cluster keypair. In your initial
      config, set `loginRunners.enable = false;` and
      `runners.enable = false;`, then:

      ```bash
      sudo nixos-rebuild switch --flake .#myhost
      ```

      The activation script creates the cluster keypair at
      `/var/lib/steampipe/ssh/cluster_key{,.pub}`.

   5. Paste the pubkey into the host config. Read the generated
      pubkey:

      ```bash
      sudo cat /var/lib/steampipe/ssh/cluster_key.pub
      ```

      Add it to your config:

      ```nix
      services.steampipe-cluster.loginRunners = {
        enable = true;
        sshAuthorizedKey = "ssh-ed25519 AAAA... steampipe-loginrunners@host";
      };
      # And the same for runners.sshAuthorizedKey if you enabled runners.
      ```

   6. Rebuild with the key baked in:

      ```bash
      sudo nixos-rebuild switch --flake .#myhost
      ```

      The pubkey now lives in every login-runner's authorized_keys.
   ```

   Add a brief explanatory paragraph: "Why two rebuilds? Flake
   evaluation runs in pure-eval mode, which forbids reading
   `/var/lib/*` from disk. The first rebuild creates the key on the
   host; the second rebuild bakes the key you just read into the VM
   closure."

6. Run the flake check suite locally to confirm nothing else trips
   over the now-required option:

   ```bash
   nix flake check --no-build
   ```

   Any pre-existing test fixture that enables `loginRunners` without
   `sshAuthorizedKey` needs to either set the field to a test value
   or set `enable = false`. Likely candidates: `flake.nix` checks like
   `module-runners-eval`, `host-warm-timer-eval`, etc.

7. Commit on a single topic branch with a descriptive message:

   ```
   module: require explicit loginRunners.sshAuthorizedKey

   Flake pure-eval makes the pathExists-based default reader dead
   code; rebuilds silently bake empty authorized_keys into the
   login-runner closure and sshd rejects every key. Promote the
   empty-key warning to an assertion with a bootstrap workflow in
   the message, and rewrite installation.md to match.
   ```

## Acceptance criteria

- [ ] `grep -n "builtins.pathExists" nix/module.nix` returns zero
      lines.
- [ ] `nix eval .#nixosConfigurations.<test-host>.config.services.steampipe-cluster.loginRunners.sshAuthorizedKey`
      returns `""` by default (no surprise auto-read).
- [ ] A test eval with `loginRunners.enable = true; sshAuthorizedKey = "";`
      fails with the new assertion message — verified by reading the
      eval error, not by mocking it.
- [ ] `nix flake check --no-build` is green; any flake-check fixture
      that previously relied on the auto-read default has been updated.
- [ ] `installation.md` contains no mention of "second rebuild bakes
      the key in" via `pathExists`; the new workflow is the
      enable=false → rebuild → paste → enable=true → rebuild sequence.
- [ ] `mdbook build docs/` is clean.
- [ ] The `Warnings` section at `nix/module.nix:399-407` no longer
      contains the two empty-`sshAuthorizedKey` branches.

## Files likely touched

- `nix/module.nix` — option defaults, `defaultText`, assertions block,
  warnings block. The bulk of the change.
- `nix/lib/host-runners.nix` — the misleading comment near
  `sshAuthorizedKeys = lib.optional ...`.
- `docs/src/getting-started/installation.md` — the bootstrap walkthrough.
- `flake.nix` — likely one or two existing eval checks need their
  fixture to set the new required field. Inspect during execution.

## Pitfalls

- **Test fixtures cascading.** The `module-runners-eval` and
  `host-warm-timer-eval` checks in `flake.nix` exercise the module
  with `loginRunners.enable` or `runners.enable` flipped on. After
  the assertion lands they fail unless they also set
  `sshAuthorizedKey = "ssh-ed25519 AAAA fake-key"` (or similar). Plan
  on touching `flake.nix` even though it's not in the headline file
  list.
- **`defaultText` desync.** If you only change `default` and leave
  the old `defaultText` describing the auto-read, the nix manual
  output (and `nixos-option`) will lie. Update both.
- **Hidden indirection through canix.** This phase changes the
  external API of `services.steampipe-cluster.loginRunners`. canix
  will need a bump after a new steampipe input revision is pushed.
  P1 already covers atlas locally; downstream consumers of the
  steampipe input may break on the next bump. That's intentional —
  the alternative is silently broken VMs.
- **Warnings vs assertions sequencing.** If you only add the
  assertions but leave the warnings, the warnings code-path never
  runs (assertion failures stop eval). Cosmetic but worth cleaning.
- **Don't delete the activation script.** The keypair generator at
  `nix/module.nix:61-77` and the
  `steampipe-loginrunners-ssh-key.service` unit are still needed —
  they create the file that the operator pastes into config. Only the
  reader on the eval side is dead.

## Reference

- Findings: [../login-runner-ssh-unreachable-findings.md](../login-runner-ssh-unreachable-findings.md)
- Background commit: 4ee68f0 "Drop runtime ssh-authorized virtiofs
  share from login runners" — explains why `pathExists` exists.
- Background commit: 2e765f4 "Finish the NixOS-module install path" —
  introduced the symmetric `runners.sshAuthorizedKey` option that this
  phase also has to fix.
- Downstream: P3 codifies the new assertion behavior as a flake check
  to prevent silent regressions.
