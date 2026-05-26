# Phase 01 — Unblock atlas with an explicit pubkey in canix

> **Recommended Codex model: GPT 5.5 low**
>
> One config edit, one rebuild, one verification command. No design
> choices, no log interpretation beyond "did sshd accept the key".
> Trivial leaf work that any tier above `low` would just burn tokens on.

## Working tree

`/data/nvme0/can/Projects/canix` (external repo, not steampipe).

## Goal

After this phase, `cluster-ctl steam login vm-1` on atlas reaches the
Steam login UX (or at least passes `Waiting for SSH... ready`) without
any change to the steampipe repository. The fix is host-config only.

## Why this matters now

Atlas is the user's daily-driver host. The current symptom from the
chat transcript:

```
══════════════════════════════════════════════════
  Steam login for vm-1 (10.0.100.1)
  Mode: automated (credentials from TOML)
══════════════════════════════════════════════════
  Booting VM...
  Waiting for SSH... timeout after 180s
  VM log unavailable: /home/can/.local/state/steampipe/default/vm-1/vm.log
Error with vm-1: SSH timeout for vm-1
```

Root cause is documented in
[../login-runner-ssh-unreachable-findings.md](../login-runner-ssh-unreachable-findings.md):
flake pure-eval makes the module's `pathExists`-based default reader
silently return `""`, so every login-runner is built with an empty
`authorized_keys`. This phase shortcuts the eval-time discovery by
hardcoding the key into canix's host config.

The other phases will repair the module so future operators don't fall
into the same trap, but those changes ship through codeberg releases
and a steampipe-input bump in canix. P1 unblocks today.

## Out of scope

- Any change to the steampipe repository.
- Rotating the cluster keypair (use the existing key on disk; rotation
  is a separate workflow).
- Touching any other `services.steampipe-cluster` field.
- Editing `installation.md` — that's P2's job.

## Plan

1. Read the current host-managed pubkey:

   ```bash
   cat /var/lib/steampipe/ssh/cluster_key.pub
   ```

   It should be a single line of the form
   `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... steampipe-loginrunners@`.

2. Open
   `/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix`
   and add the explicit `sshAuthorizedKey` under
   `services.steampipe-cluster.loginRunners`:

   ```nix
   services.steampipe-cluster = {
     enable = true;
     vmCount = 8;
     tapOwner = "can";
     tapOwnerUid = 1000;

     loginRunners = {
       enable = true;
       sshAuthorizedKey =
         "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... steampipe-loginrunners@";
     };

     accounts = { ... };  # unchanged
   };
   ```

   Paste the actual contents of `cluster_key.pub` (single line, no
   trailing newline).

3. Commit on canix `trunk`:

   ```bash
   cd /data/nvme0/can/Projects/canix
   git add root/hosts/atlas/features/steampipe.nix
   git commit -m "atlas: pin steampipe login-runner ssh key explicitly"
   ```

4. Rebuild atlas:

   ```bash
   sudo nixos-rebuild switch --flake /data/nvme0/can/Projects/canix#atlas
   ```

5. Verify the bake landed by grepping the new vm-1 system closure for
   the pubkey:

   ```bash
   sys=$(readlink /etc/steampipe/login-runners/vm-1 | sed 's|/bin/.*||')
   # Actually simpler: find the nixos-system-vm-1 store path and grep:
   grep -rln "$(awk '{print $2}' /var/lib/steampipe/ssh/cluster_key.pub)" \
       /etc/steampipe/login-runners/vm-1 \
       $(nix-store -q --references /etc/steampipe/login-runners/vm-1/bin/microvm-run)
   ```

   At least one match in a `nixos-system-vm-1-...` store path is the
   evidence that the key is now in the closure.

6. Run the actual login flow:

   ```bash
   cluster-ctl steam login vm-1
   ```

   Expect `Waiting for SSH... ready` rather than `timeout after 180s`.

## Acceptance criteria

- [ ] `cat /var/lib/steampipe/ssh/cluster_key.pub` and the value in
      `features/steampipe.nix` match byte-for-byte (no trailing
      newline, no extra whitespace).
- [ ] `sudo nixos-rebuild switch --flake .#atlas` completes with no
      warnings about
      `services.steampipe-cluster.loginRunners ... sshAuthorizedKey`.
- [ ] `grep` for the key's base64 body inside the live
      `nixos-system-vm-1` store path returns at least one match.
- [ ] `cluster-ctl steam login vm-1` prints `Waiting for SSH... ready`
      within 60 s (not the 180 s timeout).
- [ ] No regression in any other host built from the same canix flake
      (`nix flake check` on canix still passes).

## Files likely touched

- canix: `root/hosts/atlas/features/steampipe.nix` — add the
  `loginRunners.sshAuthorizedKey` field.

## Pitfalls

- **Trailing newline on paste.** `cat` outputs a trailing newline by
  default. If you paste `"...\n"` into the nix string, sshd compares
  byte-for-byte and may reject it. Use `lib.removeSuffix "\n" (...)` if
  you read with `builtins.readFile`, or paste the single line manually
  without the trailing newline.
- **Two pubkeys.** If the activation script ever ran twice (e.g.,
  someone deleted `cluster_key` and let it regenerate), the file you
  paste may not match what is also baked elsewhere. Verify with the
  grep in step 5 before celebrating.
- **Stale flake input in canix.** This phase doesn't need the latest
  steampipe input; the field already exists in the pinned `4ee68f0`
  commit. Don't bump the input as part of this phase — that's a
  separate change with its own surface.
- **Wrong VM.** atlas's `vmCount = 8` but the chat transcript shows
  the user running `cluster-ctl steam login all`. Verify on `vm-1`
  first; if it works, `all` will work for every VM that has accounts
  declared.

## Reference

- Findings: [../login-runner-ssh-unreachable-findings.md](../login-runner-ssh-unreachable-findings.md)
- canix host file:
  `/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix`
- steampipe option:
  [nix/module.nix:251-282](../../../nix/module.nix#L251-L282) —
  the `loginRunners.sshAuthorizedKey` option this phase sets explicitly.
