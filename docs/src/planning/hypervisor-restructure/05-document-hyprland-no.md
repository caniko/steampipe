# Phase 05 — Record the hyprland-is-not-supported decision

> **Recommended Codex model: GPT 5.5 low**
>
> One markdown paragraph plus a SUMMARY entry. No code, no
> reasoning beyond restating the design's locked decision. `low` is
> the correct tier; anything higher burns tokens.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

`docs/src/configuration/visual-testing.md` (or the nearest
appropriate docs page) contains a clearly-labelled paragraph
explaining why hyprland is intentionally not supported as a VM
compositor. Prevents the question from re-litigating every six
months.

## Why this matters now

The hypervisor restructure deliberately drops hyprland from scope
(see [hypervisor-restructure-design.md "D1"](../hypervisor-restructure-design.md)).
Without a durable docs trace, a future contributor — or a future
session of this same plan — will ask the question again, and the
answer ("wayvnc is wlroots-only, 70 swaymsg callsites, hyprland is
heavier in RAM") isn't obvious without re-reading the dossier.

Independent of every other phase. Lands in Wave 0.

## Out of scope

- Removing `Hyprland`/`HyprlandGpu` from the `DisplayMode` enum.
  Those variants don't exist; nothing to remove.
- Editing `src/game/compositor.rs`. The decision is "stay on sway",
  not "rewrite compositor IPC".
- Auditing for any latent hyprland references. There are none.

## Plan

1. Find the right page. Inspect:

   ```bash
   ls docs/src/configuration/
   grep -nE 'sway|compositor' docs/src/configuration/*.md
   ```

   Expected best fit:
   `docs/src/configuration/visual-testing.md`. If that page doesn't
   discuss compositor choice yet, add a short subsection. If it
   already does, append.

2. Add a paragraph (verbatim shape):

   ```markdown
   ### Why sway, and not hyprland

   The VM compositor is sway, with weston as a fallback. Hyprland
   is intentionally not supported.

   - wayvnc is documented as a VNC server for wlroots-based
     compositors. Hyprland forked from wlroots to its own
     Aquamarine renderer (~0.40+) and is partial-compat with
     wayvnc; cursor sync, multi-output bookkeeping, and resize
     behaviour all diverge.
   - The Rust harness consumes sway's IPC (`swaymsg -t get_tree`,
     `swaymsg -t get_outputs`) directly in ~70 call sites,
     including `SwayOutput`/`SwayNode` parsers in
     `src/game/compositor.rs`. Hyprland's IPC (`hyprctl`) emits a
     different JSON shape; switching would mean re-recording every
     golden fixture and rewriting the typed parsers.
   - Hyprland's resident memory footprint is notably larger than
     sway's, fighting the per-VM memory budget the cluster targets.

   See `docs/src/planning/hypervisor-restructure-design.md`
   ("Compositor — Final Stance") and the research dossier
   adjacent for the full evaluation.
   ```

3. Verify the link to
   `docs/src/planning/hypervisor-restructure-design.md` works in
   the rendered book (relative to the configuration page, the path
   is `../planning/hypervisor-restructure-design.md`).

4. `mdbook build docs/` — clean.

5. No SUMMARY changes needed if `visual-testing.md` is already
   indexed. Confirm:

   ```bash
   grep visual-testing docs/src/SUMMARY.md
   ```

6. Commit:

   ```
   docs: explain why sway, not hyprland, for VM compositor

   Records the hypervisor restructure's locked decision that
   hyprland is out of scope, with the three reasons (wayvnc
   wlroots-only, 70 swaymsg callsites, RAM footprint).
   ```

## Acceptance criteria

- [ ] `docs/src/configuration/visual-testing.md` contains a
      "Why sway, and not hyprland" subsection (or equivalent
      heading).
- [ ] The subsection cites the three reasons (wayvnc wlroots-only,
      70 swaymsg callsites, RAM footprint).
- [ ] The subsection links to
      `docs/src/planning/hypervisor-restructure-design.md`.
- [ ] `mdbook build docs/` produces no warnings.
- [ ] `docs/src/SUMMARY.md` is unchanged (`visual-testing.md`
      already indexed).

## Files likely touched

- `docs/src/configuration/visual-testing.md` — append a subsection.

## Pitfalls

- **Don't editorialise**. The paragraph states why hyprland is out
  *of scope for this repo*, not why hyprland is generally a bad
  choice. Keep it factual; no opinions on hyprland-the-project.
- **Don't link the research dossier as the primary source**. The
  *design* is the canonical decision record; the dossier is the
  evidence trail. Link the design.
- **Don't add hyprland to any enum or config option** as part of
  "documentation". The enum surface stays unchanged.
- **Stable subsection heading**: pick a stable slug. mdbook
  generates anchors from heading text; future links may target
  `#why-sway-and-not-hyprland`.

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Compositor — Final Stance", D1 in decisions).
- Research backing:
  [../hypervisor-restructure-research.md](../hypervisor-restructure-research.md)
  ("Current Reality" → "Compositor surface").
