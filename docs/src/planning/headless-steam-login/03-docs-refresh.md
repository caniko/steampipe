# Phase 03 — Docs refresh for the new login lifecycle

> **Recommended Codex model: GPT 5.5 low**
>
> Mechanical edits to three documentation files, plus a one-line nav update
> in `SUMMARY.md`. No code, no design. `medium` is wasted budget here;
> `low` is the correct tier for prose updates that codify shipped behavior.

## Working tree

`/data/nvme0/can/Projects/steampipe`. **Prerequisite: Phases 01 and 02
must be landed and validated.** This phase only writes prose about
behavior that already exists in the code; nothing here is speculative.

## Goal

The user-facing documentation accurately describes the new headless login
lifecycle: virtiofsd is started in-process by `cluster-ctl`, the operator
user must be in the `kvm` group, `steam login all` runs in parallel and
skips the VM bounce when reachable, and `--force` re-logs without rebooting.
The planning research dossier and prior stabilization notes are cross-linked
so future readers find them without spelunking through `git log`.

## Why this matters now

[docs/src/configuration/steam-credentials.md](../../configuration/steam-credentials.md)
is the canonical reference for `cluster-ctl steam login` semantics. Today
it documents the serial, always-bounce flow and says nothing about
virtiofsd. After Phases 01 and 02, every claim about timing and lifecycle
in that file is out of date. The dossier
([../headless-steam-login-research.md](../headless-steam-login-research.md))
and the prior stabilization research
([../stabilization-and-hardening-research.md](../stabilization-and-hardening-research.md))
also need short pointers so this plan can be retired cleanly later.

## Out of scope

- Do not change code. If you discover a doc claim that disagrees with the
  shipped behavior, surface it in chat and stop — do not "fix" it in code
  here.
- Do not rewrite unrelated sections of `steam-credentials.md`. Touch only
  the parts that the Phase 01/02 work invalidated or that need a new
  paragraph.
- Do not delete the planning docs. Retirement is a separate
  `verify`-mode step; this phase only updates content.
- Do not add new emojis or marketing prose.

## Plan

1. **Update [docs/src/configuration/steam-credentials.md](../../configuration/steam-credentials.md)**:
   - In the "Automated login" section
     ([docs/src/configuration/steam-credentials.md:65-83](../../configuration/steam-credentials.md#L65-L83)),
     add a paragraph: `cluster-ctl steam login` now runs every selected VM's
     login in parallel (subject to the host-memory admission gate) and
     skips the VM bounce when the VM is already reachable. `--force`
     re-logs in place without rebooting.
   - Add a new "Login VM lifecycle" subsection (right before "Checking
     Steam health") that describes: cluster-ctl spawns per-VM virtiofsd
     alongside the QEMU runner; the operator user must be in `kvm` for
     virtiofsd's socket-group to succeed; `cluster-ctl down` reaps both.
     Two-three sentences, no diagrams needed.
   - Cross-link the research dossier:
     `See [headless-steam-login-research.md](../planning/headless-steam-login-research.md)
     for the root-cause analysis behind this lifecycle.`

2. **Update [docs/src/planning/stabilization-and-hardening-research.md](../stabilization-and-hardening-research.md)**:
   - In the section that enumerates active/retired plans (around
     [docs/src/planning/stabilization-and-hardening-research.md:107-141](../stabilization-and-hardening-research.md#L107-L141)),
     add a row to the table: `headless-steam-login` — shipped. Reference
     this plan directory and the dossier.
   - One sentence is enough; do not rewrite the surrounding paragraphs.

3. **Wire the plan into [docs/src/SUMMARY.md](../../SUMMARY.md)**:
   - Under the "Planning" section (or wherever
     `stabilization-and-hardening-research.md` is referenced; check the
     file), add:
     ```markdown
     - [Headless Steam login](./planning/headless-steam-login/README.md)
       - [Phase 01 — Virtiofsd lifecycle](./planning/headless-steam-login/01-virtiofsd-lifecycle.md)
       - [Phase 02 — Parallel login](./planning/headless-steam-login/02-parallel-login-and-reuse.md)
       - [Phase 03 — Docs refresh](./planning/headless-steam-login/03-docs-refresh.md)
     - [Headless Steam login research](./planning/headless-steam-login-research.md)
     ```
   - If the planning entries are not currently in SUMMARY.md (they aren't
     today — only the `Planning` heading at most exists), add a `# Planning`
     section near the bottom matching the existing
     [docs/src/SUMMARY.md](../../SUMMARY.md) heading style.

4. **Sanity-check the build.** Run `mdbook build docs/` (or the project's
   `nix run .#docs` equivalent — check `flake.nix` packages) and confirm:
   - No broken links from the touched files.
   - The new SUMMARY entries render and link correctly.
   - Search for the string `"already up, skipping reboot"` (or whatever
     Phase 02 used) in `docs/book/` to confirm the new wording landed.

5. **Final pass on the README of this plan**:
   - Confirm the README's whole-set acceptance criteria are all checkable.
     Do not edit the README otherwise; verify mode will tick them.

## Acceptance criteria

- [ ] `docs/src/configuration/steam-credentials.md` mentions in-process
      virtiofsd, the `kvm` group requirement, parallel login, and
      reachability-skip — verified by `grep -E
      'virtiofsd|kvm group|parallel|already up' docs/src/configuration/steam-credentials.md`.
- [ ] `docs/src/planning/stabilization-and-hardening-research.md` lists
      this plan as shipped with a one-line summary and a link.
- [ ] `docs/src/SUMMARY.md` references
      `docs/src/planning/headless-steam-login/README.md` and the research
      dossier; `mdbook build docs/` exits 0 with no warnings about
      missing files.
- [ ] No production code or `nix/` files modified in this phase's commit.
- [ ] The plan's whole-set acceptance criteria in
      [README.md](./README.md) remain accurate; verify-mode can tick them
      without further changes.

## Files likely touched

- `docs/src/configuration/steam-credentials.md`
- `docs/src/planning/stabilization-and-hardening-research.md`
- `docs/src/SUMMARY.md`

If you find yourself editing anything under `src/` or `nix/`, stop — that
belongs in Phases 01 or 02.

## Pitfalls

- **mdbook strict link checking.** Relative paths from
  `docs/src/configuration/steam-credentials.md` to
  `docs/src/planning/headless-steam-login-research.md` go up two
  directories. Off-by-one on `..` paths breaks the build silently in some
  mdbook configurations and loudly in others. Symptom: `mdbook build`
  warnings about unresolved links. Recovery: test with `mdbook serve` and
  click the new link.
- **SUMMARY.md ordering.** mdbook honors the order in SUMMARY.md; existing
  planning files appear in a specific place. Insert the new entries in
  the same section, not at the top of the file. Symptom: navigation
  reorders unexpectedly. Recovery: diff against `git show HEAD:docs/src/SUMMARY.md`.
- **Stale "10 minutes serial" timing claims.** The dossier mentions
  ~10 min serial as the today-baseline. After Phase 02 ships, that number
  is historical. Don't quote it in user-facing docs; quote the new
  ≤ 4 min target instead. Symptom: docs sound contradictory. Recovery:
  keep historical numbers in the planning dir only.
- **Tense drift.** The docs describe shipped behavior; use present tense
  throughout. Future-tense leaks ("will run in parallel") betray that the
  prerequisite phases were not actually validated. Symptom: someone tries
  the feature, it doesn't behave that way. Recovery: re-validate
  Phases 01 and 02 before merging this phase.

## Reference

- Research dossier:
  [../headless-steam-login-research.md](../headless-steam-login-research.md).
- Phase 01 + 02 acceptance criteria (the source of every claim made in
  the new docs): [./01-virtiofsd-lifecycle.md](./01-virtiofsd-lifecycle.md),
  [./02-parallel-login-and-reuse.md](./02-parallel-login-and-reuse.md).
- mdbook conventions in this repo:
  [docs/src/SUMMARY.md](../../SUMMARY.md).
