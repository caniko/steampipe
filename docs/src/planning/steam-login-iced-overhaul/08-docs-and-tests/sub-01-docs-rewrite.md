# Phase 08 · Sub-layer 01 — Docs rewrite

> **Recommended Codex model: GPT 5.5 medium**
>
> Prose rewrite across several user-facing surfaces that must accurately encode a
> non-trivial new contract (single-invocation login, host dialog, headless
> fail-fast, fallbacks). Leaf-ish role, but accuracy matters — wrong docs are
> worse than none — so `medium`, not `low`.

## Working tree

`/data/nvme0/can/Projects/steampipe`. Independent of sub-02 (disjoint files).
Assumes Phases 01–07 have landed the new behavior. This sub-layer is the **only**
one that edits `docs/src/SUMMARY.md`.

## Goal

Every documentation surface describes the new Steam login flow:
auto-steamcmd on missing/expired session, a host-side iced dialog
(`cluster-guard-prompt`) for the one-time code, completion in a single invocation,
the `--code`/`STEAMPIPE_GUARD_CODE` non-interactive path, the headless fail-fast
contract, and the `steam guard --code` (and VNC, if retained) fallbacks. No
surface still presents the VNC two-phase model as the default.

## Out of scope

- Test files, `src/ui/cli.rs` help strings — sub-02.
- Code behavior — Phases 01–07.

## Plan

1. **Rewrite `docs/src/configuration/steam-credentials.md`** (current VNC
   procedure at `:23-47`, first-trust VNC recommendation `:79-82`, serial
   rationale `:84-89`): describe the default flow, the host dialog, the
   `--code`/env path, the headless fail-fast behavior, and the kvm-group
   requirement (durable — keep). Demote VNC + `steam guard` to a "Fallbacks"
   section. Note that `vnc`/wayvnc remain for screenshots only.
2. **Update `README.md`** steam sections (`:177-207`, `:431-447`, `:472-474`,
   `:784-791`, `:992`, `:58-60`): replace "VNC in and complete Steam Guard" and
   "Automated login may not work with Steam Guard accounts" with the new flow;
   update the command reference for `steam login`/`guard`.
3. **Update getting-started** (`docs/src/getting-started/quick-start.md:86`,
   `installation.md`, `host-config.md:54-61`) for the new login UX and the
   `cluster-guard-prompt` dependency on graphical operator hosts.
4. **Wire `docs/src/SUMMARY.md`** if any new doc pages are added; ensure the new
   flow is discoverable.
5. **Preserve shipped lifecycle behavior in stable docs** per the dossier's
   "Existing Plan Status": keep virtiofsd lifecycle, parallel login, and
   fill-the-gaps guidance in contributor/user docs.
6. `mdbook build docs` to confirm it renders.

## Acceptance criteria

- [ ] `steam-credentials.md` leads with the auto-steamcmd + host-dialog flow,
      documents `--code`/`STEAMPIPE_GUARD_CODE` and headless fail-fast, and has a
      clearly-labelled Fallbacks section; `rg "VNC into|complete the Steam login"
      docs/` returns only fallback context.
- [ ] README no longer presents VNC as the default login; command
      reference matches the new CLI.
- [ ] `mdbook build docs` succeeds; new/changed pages are in `SUMMARY.md`.
- [ ] The shipped lifecycle behavior remains documented in stable docs.

## Files likely touched

- `docs/src/configuration/steam-credentials.md`, `docs/src/getting-started/*`,
  `docs/src/configuration/host-config.md`, `docs/src/SUMMARY.md`
- `README.md`

## Pitfalls

- **Documenting behavior that didn't ship.** If Phase 07 kept VNC behind
  `--vnc-fallback` (first-trust unproven), document *that*, not full removal.
  *Recovery:* check the Phase 07 outcome before writing the Fallbacks section.
- **Dropping the kvm-group requirement.** It is durable (host constraint); keep
  it. *Recovery:* preserve `steam-credentials.md:102-109` content.

## Reference

- Dossier: incompatibility #14; "Existing Plan Status".
- Sibling: sub-02 (tests + CLI help). Phase README: [README.md](./README.md).
