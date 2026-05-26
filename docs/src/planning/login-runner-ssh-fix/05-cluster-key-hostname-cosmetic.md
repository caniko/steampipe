# Phase 05 — Cosmetic: non-empty hostname in cluster_key comment

> **Recommended Codex model: GPT 5.5 low**
>
> One shell-substitution fix in an activation script. The agent has
> to know that `$(hostname)` can return empty in early activation
> contexts and pick a robust source for the hostname. Trivial leaf
> work; `low` covers it without overthinking.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase, freshly generated cluster keypairs carry a
non-empty hostname suffix in the OpenSSH comment field, so operators
can tell at a glance which host the keypair was generated on.

## Why this matters now

The atlas host's currently-generated key reads:

```
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIC49vWFN00fazQJUeuYP3rBLKDAO1qeIOLf5tYOTKNgb steampipe-loginrunners@
```

That trailing `@` with nothing after it suggests the `$(hostname)`
substitution in the activation script ran in a context where
`hostname` printed nothing. The comment is purely informational, but
on a host with multiple steampipe-managed environments it makes the
keypair impossible to identify by sight.

Relevant code: [nix/module.nix:61-77](../../../nix/module.nix#L61-L77)

```nix
loginRunnerKeyScript = ''
  ...
  if [ ! -f "$key" ]; then
    ${pkgs.openssh}/bin/ssh-keygen -t ed25519 -N "" \
      -C "steampipe-loginrunners@$(${pkgs.coreutils}/bin/hostname)" \
      -f "$key"
  fi
  ...
'';
```

The script is used in two contexts:

- `system.activationScripts.steampipe-loginrunners-ssh-key` —
  [nix/module.nix:440-442](../../../nix/module.nix#L440-L442). Runs
  during NixOS activation; `hostname` may be unset depending on
  systemd readiness at activation time.
- `systemd.services.steampipe-loginrunners-ssh-key` —
  [nix/module.nix:489-499](../../../nix/module.nix#L489-L499). Runs
  via systemd at `multi-user.target`; `hostname` works here.

Whichever ran first on atlas left the trailing-`@` artifact.

## Out of scope

- Rotating the existing key on atlas. The cosmetic fix only applies
  to *new* generations.
- Changing the keypair location, type, or comment prefix.
- Anything outside the `loginRunnerKeyScript` body.

## Plan

1. Replace `$(${pkgs.coreutils}/bin/hostname)` with a more robust
   source. The cleanest fix is to use the NixOS `config.networking.hostName`
   at eval time (it's a string known to the module) and bake it in:

   ```nix
   loginRunnerKeyScript = ''
     set -euo pipefail
     keydir=${lib.escapeShellArg loginRunnerSshDir}
     key="$keydir/cluster_key"

     ${pkgs.coreutils}/bin/install -d -m 0700 "$keydir"
     if [ ! -f "$key" ]; then
       ${pkgs.openssh}/bin/ssh-keygen -t ed25519 -N "" \
         -C ${lib.escapeShellArg "steampipe-loginrunners@${config.networking.hostName}"} \
         -f "$key"
     fi
     ...
   '';
   ```

   This guarantees the comment is correct regardless of when the
   script runs.

2. Reproduce on a scratch VM (or `nix-shell` eval) that the new
   `loginRunnerKeyScript` substitutes `steampipe-loginrunners@atlas`
   correctly. A quick check:

   ```bash
   nix eval --raw .#nixosConfigurations.<host>.config.systemd.services.steampipe-loginrunners-ssh-key.script \
       | grep -F 'steampipe-loginrunners@'
   ```

   The output should contain `steampipe-loginrunners@<hostname>`, not
   `$(hostname)`.

3. `nix flake check --no-build`.

4. Commit:

   ```
   module: bake host name into login-runner key comment

   $(hostname) in the activation/systemd script can run before
   /etc/hostname is set, leaving a trailing "@" on the keypair
   comment. Use config.networking.hostName so the comment is
   correct regardless of when the script first runs.
   ```

## Acceptance criteria

- [ ] `grep -n '\$(.*hostname.*)' nix/module.nix` returns no matches
      inside `loginRunnerKeyScript`.
- [ ] `nix eval --raw .#nixosConfigurations.<host>.config.systemd.services.steampipe-loginrunners-ssh-key.script`
      contains `steampipe-loginrunners@<expected-hostname>` literally.
- [ ] On a host where `cluster_key` is regenerated *after* this
      change lands (e.g., by deleting the file and re-running the
      systemd unit), `cluster_key.pub` ends with
      `steampipe-loginrunners@<hostname>`, not `@`.
- [ ] `nix flake check --no-build` is green.

## Files likely touched

- `nix/module.nix` — only the `loginRunnerKeyScript` body. The two
  consumers (activation script + systemd unit) inherit the fix.

## Pitfalls

- **Shell-quoting the interpolation**: if `config.networking.hostName`
  ever contained a shell metacharacter, naive interpolation breaks
  the script. Use `lib.escapeShellArg` (as shown) — overkill for the
  realistic input space, cheap insurance.
- **Stale keys aren't rewritten**: the script only regenerates the
  key when the file is absent (`if [ ! -f "$key" ]`). atlas's existing
  bad-comment key will remain on disk after this phase ships. That's
  fine — the comment is informational. Document this in the commit
  message if asked.
- **Don't conflate with P2**: this phase is cosmetic only. Do not
  reach into the `sshAuthorizedKey` option or the bootstrap workflow
  while editing `loginRunnerKeyScript` — those are P2's surface.

## Reference

- Findings note's "Optional follow-ups" section in
  [../login-runner-ssh-unreachable-findings.md](../login-runner-ssh-unreachable-findings.md).
- Script body:
  [nix/module.nix:61-77](../../../nix/module.nix#L61-L77).
- Activation hook:
  [nix/module.nix:440-442](../../../nix/module.nix#L440-L442).
- Systemd unit:
  [nix/module.nix:489-499](../../../nix/module.nix#L489-L499).
