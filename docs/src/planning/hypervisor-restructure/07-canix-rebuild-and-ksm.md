# Phase 07 — Rebuild atlas + enable KSM (canix-side)

> **Recommended Codex model: GPT 5.5 low**
>
> Two canix commits: enable `hardware.ksm.enable = true;` and run
> `nixos-rebuild switch`. The agent has to verify the canix option
> name (NixOS has had several KSM option locations over the years)
> and confirm the rebuild retires the SSH-fix P1 partial. Mechanical
> work in a small repo; `low` is the right tier.

## Working tree

`/data/nvme0/can/Projects/canix` (external repo).

## Goal

After this phase:

1. atlas's live system has `hardware.ksm.enable = true;` (or the
   canix-correct option name) and KSM is actively running.
2. atlas's `nixos-system-vm-1` closure contains the cluster SSH
   pubkey from
   `/var/lib/steampipe/ssh/cluster_key.pub`. This retires the
   SSH-fix plan's [Phase 01](../login-runner-ssh-fix/01-canix-explicit-key-unblock.md)
   `partial` status.
3. `cluster-ctl steam login vm-1` (or any vm-N) reaches at least
   the Steam login stage without an SSH timeout.

## Why this matters now

The memory budget table in
[hypervisor-restructure-design.md "Final Memory Budgets"](../hypervisor-restructure-design.md)
explicitly assumes host-side KSM is reclaiming identical pages
across VMs. Without KSM, the "host reclaimable (idle)" column in
that table is overstated and the memory plan is less safe.

Separately, the SSH-fix plan's P1 has been `partial` since
2026-05-25 — canix committed the explicit key but atlas hasn't been
rebuilt against that worktree yet. Folding the rebuild into this
restructure clears the deck so later phases (08, 09) can verify
their work via `cluster-ctl steam login` on a healthy baseline.

Independent of every steampipe phase. Lands in Wave 0.

## Out of scope

- Editing the steampipe repo.
- Touching canix modules other than `root/hosts/atlas/`.
- Changing canix's flake input pin for steampipe.
  (Future phases that need a new steampipe-input revision will bump
  it then.)
- Any KSM tuning beyond the on/off switch (page scan rate, sleep
  millis). Defaults are fine.

## Plan

1. Verify the current canix option name for KSM:

   ```bash
   cd /data/nvme0/can/Projects/canix
   grep -rn "ksm" --include="*.nix" . 2>/dev/null
   nix eval --no-write-lock-file --raw '.#nixosConfigurations.atlas.options.hardware.ksm.enable.description' 2>&1 | head -3
   ```

   Common option names: `hardware.ksm.enable`, `services.ksm.enable`,
   `boot.kernel.sysctl."kernel.ksm.run"`. NixOS has had several;
   confirm which one canix's nixpkgs pin exposes.

2. Enable KSM. In
   `/data/nvme0/can/Projects/canix/root/hosts/atlas/default.nix` or
   `features/steampipe.nix` (whichever holds the existing
   atlas-specific knobs):

   ```nix
   hardware.ksm.enable = true;
   ```

   Or the canix-correct equivalent name from step 1.

3. (Verify SSH-key bake) Re-confirm the explicit key in
   `root/hosts/atlas/features/steampipe.nix:26` matches the host's
   current `/var/lib/steampipe/ssh/cluster_key.pub`:

   ```bash
   diff \
       <(awk -F'"' '/sshAuthorizedKey/ {print $2}' /data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix) \
       <(cat /var/lib/steampipe/ssh/cluster_key.pub | tr -d '\n')
   ```

   If they diverge, paste the current key in — but only if the host
   keypair has rotated; usually they match (the SSH-fix P1 commit
   already pinned this).

4. Commit (canix):

   ```bash
   cd /data/nvme0/can/Projects/canix
   git add root/hosts/atlas/  # whichever files changed
   git commit -m "atlas: enable host-side KSM for steampipe VMs

   The steampipe hypervisor restructure's memory budget assumes
   KSM is deduplicating identical pages across VMs (Mesa, libc,
   kernel images). Enable it explicitly on atlas."
   ```

5. Rebuild atlas:

   ```bash
   sudo nixos-rebuild switch --flake /data/nvme0/can/Projects/canix#atlas
   ```

   Watch for warnings; specifically the deprecation warnings the
   steampipe module emits for crosvm pins should NOT fire (atlas
   doesn't pin crosvm by name; it inherits defaults).

6. Verify the new system activated and KSM is on:

   ```bash
   readlink /run/current-system  # should point at a new store path
   cat /sys/kernel/mm/ksm/run    # 1 means KSM is running
   cat /sys/kernel/mm/ksm/pages_shared  # increases over time
   ```

7. Verify the SSH-fix P1 partial is now closed:

   ```bash
   sys=$(readlink /run/current-system)
   nix-store -qR "$sys" | grep nixos-system-vm-1
   # Take the resulting /nix/store/...nixos-system-vm-1-... path:
   grep -rln "AAAAC3NzaC1lZDI1NTE5AAAAIC49vWFN00fazQJUeuYP3rBLKDAO1qeIOLf5tYOTKNgb" \
       /nix/store/...-nixos-system-vm-1-...
   ```

   At least one match (typically in users-groups.json or
   authorized_keys.d) is the evidence the key is baked in.

8. Smoke-test `cluster-ctl`:

   ```bash
   cluster-ctl steam login vm-1
   ```

   Should print `Waiting for SSH... ready`. The Steam GUI step may
   still crash on crosvm-virgl (the whole reason for this
   restructure) — that's fine; this phase only verifies SSH
   reaches the VM.

9. Update the SSH-fix plan's status if you wish (optional;
   Phase 10 may auto-retire that whole directory):

   ```bash
   cd /data/nvme0/can/Projects/steampipe
   # No code change; the plan's status is tracked by `verify` mode.
   # Nothing to commit here.
   ```

## Acceptance criteria

- [ ] `/sys/kernel/mm/ksm/run` on atlas is `1`.
- [ ] `cat /sys/kernel/mm/ksm/pages_shared` returns a positive
      value after at least one VM has been running for ≥ 60 s.
- [ ] `readlink /run/current-system` points at a store path
      different from the system that was active before this phase.
- [ ] `grep` for the cluster pubkey body returns at least one
      match inside the live `nixos-system-vm-1` store path
      (retires SSH-fix P1).
- [ ] `cluster-ctl steam login vm-1` prints `Waiting for SSH... ready`
      within 60 s (Steam GUI step is allowed to fail per pre-Phase-08
      hypervisor; SSH reaching the VM is what's tested).
- [ ] The canix commit's message names KSM as the change and links
      to the steampipe restructure plan (or references it informally).
- [ ] No code change in the steampipe repo for this phase.

## Files likely touched

- `/data/nvme0/can/Projects/canix/root/hosts/atlas/default.nix` or
  `features/steampipe.nix` — `hardware.ksm.enable = true;` (or
  equivalent).

## Pitfalls

- **Wrong option name**: `hardware.ksm.enable` vs `services.ksm.enable`
  vs sysctl-only. Verify against canix's nixpkgs pin (step 1).
  Wrong name silently fails — the build succeeds but KSM stays off.
- **Don't tune KSM defaults**: changing `pages_to_scan`, `sleep_millisecs`,
  or `merge_across_nodes` is out of scope. Defaults are
  conservative enough; tuning is a follow-up if `pages_shared` is
  disappointing after weeks of runtime.
- **Don't conflate with `services.zswap`** or `services.zram`:
  those are separate compressed-swap mechanisms; KSM is page-dedup.
  Enabling all three is fine but each is its own decision; this
  phase enables KSM only.
- **`cluster-ctl steam login` may still crash**: at Steam GUI
  startup, on the unchanged crosvm-virgl path. That's expected;
  this phase only verifies SSH reaches the VM. The Steam GUI fix
  is Phase 08.
- **Don't bump canix's steampipe input pin** as part of this phase.
  That's a separate commit when Phase 08 (or later) needs new
  steampipe behaviour on atlas.

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Host-Side (canix) Changes").
- SSH-fix P1 (status: `partial`, retires here):
  [../login-runner-ssh-fix/01-canix-explicit-key-unblock.md](../login-runner-ssh-fix/01-canix-explicit-key-unblock.md).
- canix atlas host config:
  `/data/nvme0/can/Projects/canix/root/hosts/atlas/features/steampipe.nix`.
