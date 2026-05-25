# Phase 05 — Docs + UX cleanup

> **Recommended Codex model for merge/orchestration: GPT 5.5 low**
>
> Three disjoint mechanical edits across separate file groups. The
> phase-level merge is trivial: each sub-layer touches a distinct
> path, and the three sub-PRs can land in any order. Routing the
> merge step to 5.5 low matches the work; the per-sub-layer routing
> is set independently in each sub-layer file.

## Sub-layers

| # | Slug | Model | Touches | Sub-layer file |
|---|------|-------|---------|----------------|
| 01 | readme-quickstart | 5.5 low | [README.md](../../../../../README.md), [docs/src/getting-started/quick-start.md](../../../../../docs/src/getting-started/quick-start.md) | [sub-01-readme-quickstart.md](./sub-01-readme-quickstart.md) |
| 02 | configuration-docs | 5.5 low | [docs/src/configuration/steam-credentials.md](../../../../../docs/src/configuration/steam-credentials.md), [docs/src/configuration/host-config.md](../../../../../docs/src/configuration/host-config.md) | [sub-02-configuration-docs.md](./sub-02-configuration-docs.md) |
| 03 | cli-help-text | 5.5 low | [src/ui/cli.rs](../../../../../src/ui/cli.rs) (help-text strings only) | [sub-03-cli-help-text.md](./sub-03-cli-help-text.md) |

## Goal (phase-level)

User-facing surfaces — README, mdBook docs, and CLI `--help` output —
reflect the post-phase-03/04 reality: on a NixOS host with
`services.steampipe-cluster.loginRunners.enable = true`, no
`--login-runners-dir` flag is needed. The flag-based examples remain
documented as a fallback for non-NixOS hosts but stop being the
primary recipe.

## Why this matters now

Phase 04 proved the config-less flow works on atlas. Users opening
the README or running `cluster-ctl steam login --help` still see
flag-required examples and will be confused about why their host
config "isn't working" (it is; the docs are stale). This phase
brings the surface area in sync with the implementation.

## Out of scope

- Changing CLI flag *behavior* — phase 02 handled that. Sub-layer 03
  only edits help-text strings, never the `#[arg(...)]` attributes
  or option types.
- Documenting the NixOS module option in mdBook for the first time —
  that was phase 03's job. Phase 05 *links* to that doc from the
  quick-start recipes.
- Documenting host-level `runners` (for `steam check`/`warm`). That
  capability was deferred in phase 03. If it lands later, a separate
  doc pass picks it up.

## Merge plan

The three sub-layers touch disjoint files. The user runs each
sub-layer in a fresh agent session (or dispatches them in parallel),
then merges all three branches into one PR. Conflicts are not
expected; if one occurs, it indicates an out-of-order edit (e.g.,
sub-01 touching `cli.rs` help-text) — back out and re-dispatch the
offending sub-layer.

## Phase-level acceptance criteria

- [ ] `grep -rn -- --login-runners-dir README.md docs/src/` shows results only in **fallback** / **non-NixOS** subsections (or in the `--help` output reference table); no longer in the primary "how to log in" recipe.
- [ ] README's "Steam login" section mentions
  `services.steampipe-cluster.loginRunners.enable = true` as the
  recommended NixOS path before any flag-based example.
- [ ] `cluster-ctl steam login --help` output (sub-layer 03's
  responsibility) says `--login-runners-dir` is *optional when a
  NixOS host config provides loginRunnersDir*, not "required for the
  microvm backend".
- [ ] mdBook builds cleanly (`mdbook build docs/`) with no broken
  links introduced by the edits.

## Reference

- Dossier: [../../cli-configless-from-nixos-research.md](../../cli-configless-from-nixos-research.md) — see "Work That Should Survive > D. CLI UX cleanup".
- Phase 04 (must land first): [../04-canix-wiring-and-smoke.md](../04-canix-wiring-and-smoke.md).
