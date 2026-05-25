# Sub-layer 05.02 — Configuration mdBook pages

> **Recommended Codex model: GPT 5.5 low**
>
> Two mdBook pages, both dealing with credentials/host-config docs.
> Mechanical reframing of recipes; the bulk of `host-config.md`'s
> schema reference was updated in phase 01 already. This sub-layer
> only edits the worked examples and walkthrough text.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

`docs/src/configuration/steam-credentials.md` and the walkthroughs in
`docs/src/configuration/host-config.md` document the config-less
NixOS flow as the primary path. The flag-based recipes survive as a
"Fallback for non-NixOS hosts" subsection in each file.

## Why this matters now

[docs/src/configuration/steam-credentials.md:26](../../../../../docs/src/configuration/steam-credentials.md#L26),
:48, :51, :66 currently lead with:

```sh
cluster-ctl --vm-count 7 steam login --login-runners-dir ./result-login
```

[docs/src/configuration/host-config.md:113-150](../../../../../docs/src/configuration/host-config.md#L113-L150)
has the "NixOS module walkthrough" section but the example only
shows `accounts`. After phase 03, that section also needs to
demonstrate `loginRunners.enable = true`.

## Out of scope

- The schema reference table in `host-config.md` (already updated in
  phase 01 and extended in phase 03).
- README and quick-start (sub-layer 01).
- CLI help text (sub-layer 03).

## Plan

1. **Rewrite the worked examples in
   [docs/src/configuration/steam-credentials.md](../../../../../docs/src/configuration/steam-credentials.md).**

   Replace each `cluster-ctl --vm-count N steam login
   --login-runners-dir ...` example with the config-less form
   (`cluster-ctl steam login`, `cluster-ctl steam login vm-3`,
   `cluster-ctl steam login vm-3 -+`).

   Add a closing "Non-NixOS fallback" subsection at the bottom of
   the page that shows the flag-based form once, for hosts that
   can't use the NixOS module.

2. **Extend the NixOS module walkthrough in
   [docs/src/configuration/host-config.md](../../../../../docs/src/configuration/host-config.md)
   (lines 113-137 area).**

   Add to the existing example:

   ```nix
   services.steampipe-cluster = {
     enable = true;
     accounts = { ... };

     # Enable host-level login VM runners so cluster-ctl can
     # run `steam login` without --login-runners-dir.
     loginRunners.enable = true;
   };
   ```

   Under the existing `cluster-ctl steam info` trace block, add a
   second example showing that `loginRunnersDir` appears in the
   trace as sourced from `/etc/steampipe/module.json`.

3. **Update the schema-version note at lines 80-88** to reflect that
   `loginRunnersDir`, `sshKey`, `vmUser` are optional additive
   fields and emitted only when
   `services.steampipe-cluster.loginRunners.enable = true`.

4. **Verify the page renders.**

   ```sh
   mdbook build docs/
   ```

## Acceptance criteria

- [ ] `grep -n -- --login-runners-dir docs/src/configuration/steam-credentials.md`
  shows the flag only in a section explicitly titled "Non-NixOS
  fallback" or equivalent.
- [ ] `docs/src/configuration/host-config.md`'s NixOS module
  walkthrough demonstrates `loginRunners.enable = true` and shows
  the resulting `loginRunnersDir` row in the discovery trace.
- [ ] The schema-version note at the top of `host-config.md`
  acknowledges the new optional fields without claiming a v3 bump
  occurred.
- [ ] `mdbook build docs/` exits 0; no broken cross-links between
  this sub-layer's files and sub-layer 01's edits to the README /
  quick-start.

## Files likely touched

- [docs/src/configuration/steam-credentials.md](../../../../../docs/src/configuration/steam-credentials.md)
- [docs/src/configuration/host-config.md](../../../../../docs/src/configuration/host-config.md)
  (walkthrough section only; the schema reference was already
  updated in phase 01).

## Pitfalls

- **Drift from sub-layer 01.** The README and the mdBook
  quick-start both narrate the same recipe. If sub-layer 01 chose
  one phrasing for the option name or the recipe shape, this
  sub-layer must match. Read sub-layer 01's diff before starting
  (or read the merged branch state if it landed first).
- **Don't duplicate the schema reference.** The full field table
  lives once in `host-config.md`. The walkthrough should *link* to
  it, not re-paste it.
- **`schemaVersion: 1` upgrade messaging.** The current doc says
  "`cluster-ctl` accepts `1` and `2` for one release cycle". Don't
  change that statement in this sub-layer — the schema version
  policy is unchanged. Only the field set is additive.

## Reference

- Phase 05 README: [./README.md](./README.md).
- Phase 01 (schema-reference text this sub-layer must not duplicate): [../01-host-config-schema-fields.md](../01-host-config-schema-fields.md).
- Phase 03 (defines the module option this sub-layer documents): [../03-nixos-module-login-runners.md](../03-nixos-module-login-runners.md).
