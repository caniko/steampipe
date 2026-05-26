# Phase 03 — Flake check: assertion fires on empty sshAuthorizedKey

> **Recommended Codex model: GPT 5.5 low**
>
> One new eval-only flake check that mirrors the pattern of the
> existing `module-runners-eval` / `host-warm-timer-missing-runners-eval`
> checks added in commit 2e765f4. Mechanical Nix work, no design
> calls, no log interpretation. `low` is the right tier; anything
> higher just burns tokens on a ~20-line addition.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase, a future regression that re-introduces a
silently-empty `loginRunners.sshAuthorizedKey` (or `runners.sshAuthorizedKey`)
default trips `nix flake check` rather than landing on `trunk`. The
check exercises the assertion path P2 added and confirms it produces
the expected message.

## Why this matters now

P2 makes the bug loud at eval time, but only if an operator hits the
empty-key state. There is no guard against someone — possibly future
us — re-adding the `pathExists` default "to make installation feel
nicer" without realising flake pure-eval defeats it again. A flake
check pins the new contract.

The existing pattern lives at the bottom of `flake.nix` under the
`checks.${system}` set, e.g. `module-runners-eval`,
`module-overlay-applied-eval`, `host-warm-timer-missing-runners-eval`.
They evaluate `nixosConfigurations` against asserted properties and
fail the build if eval succeeds when it shouldn't (or vice versa).

## Out of scope

- Re-litigating any of P2's design choices. P3 only encodes them.
- Adding integration tests that boot a VM and SSH into it. That's a
  much heavier surface and not required to guard against the specific
  regression class this phase targets.
- Adding checks for the rest of the module's option surface.

## Plan

1. Read the existing patterns to match the style:

   ```bash
   grep -nE "module-(runners|overlay)-eval|host-warm-timer" flake.nix
   ```

   Copy the structure (probably a `pkgs.runCommand` that imports
   `nixpkgs.lib.nixos` and asserts an eval-time failure with a
   substring match against the assertion message).

2. Add two checks. Suggested names:

   - `module-loginrunners-empty-key-asserts` — enables
     `loginRunners.enable = true;` without setting
     `sshAuthorizedKey`; expects eval to fail with a message
     containing the substring
     `loginRunners.enable is true but sshAuthorizedKey is empty`.
   - `module-runners-empty-key-asserts` — same shape for
     `runners.enable`.

   Each check should:

   - Import `nixpkgs.lib.nixosSystem` against a minimal module that
     enables the cluster service with the broken config.
   - Run `nix-instantiate` (or equivalent eval) in a `runCommand` that
     captures stderr.
   - `grep` the captured stderr for the assertion's signature
     substring. Exit 0 on match (the assertion fired as expected),
     non-zero otherwise.

3. Add a positive companion check: `module-loginrunners-with-key-evals`
   that sets a fake but valid-looking `sshAuthorizedKey =
   "ssh-ed25519 AAAA fake";` and confirms the configuration evaluates
   successfully. This guards against the assertion accidentally
   firing on the happy path.

4. Run the new checks locally:

   ```bash
   nix flake check --no-build
   nix build .#checks.x86_64-linux.module-loginrunners-empty-key-asserts
   nix build .#checks.x86_64-linux.module-runners-empty-key-asserts
   nix build .#checks.x86_64-linux.module-loginrunners-with-key-evals
   ```

   Each should succeed (i.e., the assertion did fire / didn't fire as
   appropriate). If any of the new checks fail, fix the check, not
   the assertion.

5. Commit:

   ```
   flake: guard loginRunners/runners empty-key assertions

   Adds eval-only checks that fail if the empty-sshAuthorizedKey
   guard from <commit-of-P2> is removed or weakened. Companion
   positive check confirms the happy path still evals.
   ```

## Acceptance criteria

- [ ] `nix flake show 2>/dev/null | grep -E "module-(login)?runners-empty-key-asserts|module-loginrunners-with-key-evals"`
      lists all three new checks.
- [ ] `nix flake check --no-build` is green after the changes.
- [ ] Reverting the P2 assertion *locally* makes
      `module-loginrunners-empty-key-asserts` fail with a clear
      message about the missing assertion. Re-apply P2 before
      committing.
- [ ] Each new check has a description comment in `flake.nix`
      naming the regression class it protects against.

## Files likely touched

- `flake.nix` — only file. Additions only; no removals.

## Pitfalls

- **Pure-eval again.** The check itself runs under flake pure-eval,
  same as the bug. If your assertion-fired-as-expected detector reads
  any state under `/var/lib/`, you're reproducing the original
  problem. Keep the check purely Nix-evaluated; rely on `nix eval`
  exit codes and stderr, not filesystem reads.
- **stderr capture in `runCommand`.** `nix-instantiate` writes
  assertion errors to stderr. Use `2>&1` redirection and `grep`-style
  matching in the runCommand script; don't rely on JSON output that
  may not exist.
- **Brittle substring matches.** If your `grep` matches the entire
  assertion message verbatim, future minor edits to the assertion
  text break the check. Pick a short, stable substring (e.g.,
  `sshAuthorizedKey is empty`) — long enough to be unambiguous,
  short enough to survive copy edits.
- **`module-runners-eval` already exists.** Don't shadow it with a
  near-identical name. The new checks should sit alongside it, not
  replace it.

## Reference

- Predecessor phase: [02-drop-pathexists-default-and-assert.md](./02-drop-pathexists-default-and-assert.md).
  This phase will not pass until P2's assertion is in place.
- Existing check pattern: search `flake.nix` for `module-runners-eval`.
- Background commit 2e765f4 added the existing pattern; mimic its
  shape.
