# Sub-layer 05.01 — README + Quick Start

> **Recommended Codex model: GPT 5.5 low**
>
> Mechanical doc edits across two markdown files. No design content,
> no code reading beyond verifying that the recommended NixOS option
> name matches what phase 03 actually wired.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

The top-level README and the mdBook quick-start lead with the
config-less recipe on NixOS hosts, with the `--login-runners-dir`
flag-based recipe demoted to a "Non-NixOS hosts" subsection. Users
landing on either doc see the canix path first.

## Why this matters now

Today both [README.md:181-185](../../../../../README.md#L181-L185)
and [docs/src/getting-started/quick-start.md:67-71](../../../../../docs/src/getting-started/quick-start.md#L67-L71)
show:

```sh
cluster-ctl --vm-count 7 steam login --login-runners-dir ./result-login
```

After phases 01-04, the recommended invocation on a configured NixOS
host is:

```sh
cluster-ctl steam login
```

## Out of scope

- Editing `docs/src/configuration/steam-credentials.md` or
  `host-config.md` (sub-layer 02).
- Editing CLI help text (sub-layer 03).

## Plan

1. **Identify every `--login-runners-dir` mention in the two files.**

   ```sh
   grep -n -- --login-runners-dir README.md docs/src/getting-started/quick-start.md
   ```

   Expected per dossier: README.md hits at lines ~181, 185, ~393,
   411, 414, ~422, ~947; quick-start.md hits at ~67, 71.

2. **Rewrite the primary "Steam login" section in README.md.**
   Restructure to:

   ```markdown
   ## Steam login

   ### Recommended: NixOS host (canix)

   Set `services.steampipe-cluster.loginRunners.enable = true` and
   run `nixos-rebuild switch`. Then from any directory:

       cluster-ctl steam login           # all VMs (default)
       cluster-ctl steam login vm-3      # single VM
       cluster-ctl steam login vm-3 -+   # vm-3..vm-N

   No flags needed — the host config provides the runner directory,
   VM count, SSH key, and credentials path.

   ### Fallback: project flake (non-NixOS hosts)

   When `services.steampipe-cluster.loginRunners.enable` is not set
   (or the host isn't running NixOS), supply the runners directory
   explicitly:

       cluster-ctl --vm-count 7 steam login \
         --login-runners-dir ./result-login
   ```

   Apply the same restructure to the deeper README sections that
   currently lead with the flag form (the dossier identified ~393,
   411, 414, 422 as those locations).

3. **Rewrite [docs/src/getting-started/quick-start.md](../../../../../docs/src/getting-started/quick-start.md)
   lines 67-71.** Same shape — config-less first, flag second.

4. **Cross-link to the new section in [docs/src/configuration/host-config.md](../../../../../docs/src/configuration/host-config.md).**
   Add a one-line "See also: NixOS module walkthrough" pointer near
   the "Recommended: NixOS host" header.

5. **Verify with mdBook.**

   ```sh
   mdbook build docs/
   ```

   Look for broken links in the build output (mdBook reports them).

## Acceptance criteria

- [ ] README's first occurrence of "Steam login" leads with the
  config-less recipe; the flag form is in a subsection clearly
  labeled as the non-NixOS fallback.
- [ ] `grep -n -- --login-runners-dir README.md` shows the flag only
  inside subsections explicitly labeled as fallback/non-NixOS or in
  the flag-reference table.
- [ ] `grep -n -- --login-runners-dir docs/src/getting-started/quick-start.md`
  similarly relegates the flag to a fallback subsection.
- [ ] `mdbook build docs/` exits 0 with no warnings about broken
  links.
- [ ] The NixOS option name used in the docs
  (`services.steampipe-cluster.loginRunners.enable`) matches exactly
  what phase 03 wired. If phase 03 used a different name, update
  here.

## Files likely touched

- [README.md](../../../../../README.md) — sections at lines ~181,
  ~393, ~947 (or wherever the dossier-identified line numbers have
  drifted to).
- [docs/src/getting-started/quick-start.md](../../../../../docs/src/getting-started/quick-start.md)
  — lines 67-71 area.

## Pitfalls

- **Line numbers drift.** Don't anchor edits to the dossier's line
  numbers literally; re-grep first and operate on current state.
- **Don't delete the flag form.** Non-NixOS hosts still need it; the
  fallback subsection is mandatory. Just demote, don't remove.
- **Don't introduce new `--target` references.** That flag does not
  exist (positional only). If the agent finds any `--target`
  mentions in the README from prior chat artifacts, delete them
  silently.

## Reference

- Phase 05 README: [./README.md](./README.md).
- Phase 04 (provides the canix wiring this doc recommends): [../04-canix-wiring-and-smoke.md](../04-canix-wiring-and-smoke.md).
- Phase 03 (defines the option name): [../03-nixos-module-login-runners.md](../03-nixos-module-login-runners.md).
