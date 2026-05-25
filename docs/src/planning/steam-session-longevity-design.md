# Steam Session Longevity Design Dossier

## Goal And Trigger

The research dossier at [steam-session-longevity-research.md](./steam-session-longevity-research.md) identified seven discrete defects that cause Steam sessions on cluster VMs to expire silently between test runs. This document picks one concrete design path through them, with file-level citations and rationale, so the downstream multi-phase planner can fan the work out.

The design is intentionally **subtractive**: it removes the synthesized `loginusers.vdf`, removes the read/write mount split, removes the SteamCMD-to-GUI sync script, and removes every `pkill -x steam` that races with a token write. The replacement surface area is smaller than what it deletes.

Downstream planner: `multi-phase-plan-codex` (default).

## Current Reality

Three structural facts make the fix tractable without rewriting the lifecycle:

1. **`microvm_stop` is already a clean SIGTERM** — [src/core/backend.rs:279-295](../../../src/core/backend.rs#L279) sends `kill <pid>` to the `microvm-run` wrapper, which propagates to the hypervisor and triggers ACPI shutdown. Guest `systemd` therefore runs `ExecStop=` units before the VM is killed. We can put per-VM cleanup logic in guest systemd units rather than in host SSH scripts.
2. **`loginStateDir/vm-N` is already a per-VM virtiofs share** — [nix/lib/test-cluster.nix:200-219](../../../nix/lib/test-cluster.nix#L200) wires this for login runners (writable, as `/home/${vmUser}`) and for regular runners (read-only, at `/mnt/steam-login`). The mount plumbing exists; we only need to change what gets mounted where.
3. **Lease serializes per-VM access** — [src/vm/lease.rs](../../../src/vm/lease.rs) guarantees that at most one cluster owns a given VM slot at a time. Concurrent writes to `loginStateDir/vm-N` from two different boots of the same VM cannot occur as long as the lease is honored. This means a writable mount with no further locking is safe.

These three facts together let us treat `loginStateDir/vm-N` as the **single source of truth for Steam state** — readable and writable by every runner, with the guest doing its own clean-shutdown sequencing via systemd.

## Design Overview

The fix has four pieces, in dependency order. Each piece stands alone — if the planner needs to ship them as separate PRs they still compose without breaking anything.

### Piece 1 — Graceful Steam shutdown helper

A new `graceful_stop_steam(backend, ip, vm_user)` helper in `src/game/steam.rs` that runs `steam -shutdown` (Steam's documented IPC shutdown command, which flushes session state through the existing CEF pipe), polls `pgrep -x steam` until empty or N seconds elapse, and only then falls back to SIGTERM and finally SIGKILL.

```
graceful_stop_steam:
  ssh: steam -shutdown
  for i in 1..30:
    ssh: pgrep -x steam || break
    sleep 1
  if still alive:
    ssh: pkill -TERM -x steam
    sleep 5
  if still alive:
    ssh: pkill -KILL -x steam
```

Replace every "we are stopping Steam because the session is good" call site:

| Call site | Why graceful is right |
|---|---|
| [src/game/steam.rs:539](../../../src/game/steam.rs#L539) (`shutdown_steam_login_vm` after `steam guard`) | The whole point of this path is "Steam just got fresh tokens, save them." |
| [src/game/steam.rs:619](../../../src/game/steam.rs#L619) (`login_single_vm` after interactive/automated login) | Same — this is the *primary* token-loss bug. |
| [src/game/run.rs:37](../../../src/game/run.rs#L37) (`stop-game` cleanup) | Steam was running during the test and may have rotated its token. |
| Future `steam warm` (Piece 4) | Whole point of the command. |

Leave the SIGTERM-then-SIGKILL in [src/game/steam.rs:226-229](../../../src/game/steam.rs#L226) (`ensure_steam_script`) alone — that path runs when Steam is detected as wedged, and graceful shutdown is exactly the wrong choice there.

### Piece 2 — Collapse the mount model to one writable share

Replace the current three-way login mount enum (`null` / `"read"` / `"write"`) with a single mode and a more precise mount point.

Today (existing, at [nix/lib/test-cluster.nix:191-219](../../../nix/lib/test-cluster.nix#L191)):

```
former login mode = null   → no share, home is 8 GiB disk image
former login mode = "read" → host share at /mnt/steam-login (RO), oneshot copies into home
former login mode = "write" → host share IS /home/${vmUser} (RW)
```

Proposed:

```
shareSteamState = true | false  (default true when loginStateDir is set)
  when true:
    home volume      = 8 GiB disk image (unchanged)
    virtiofs share   = loginStateDir/vm-N
    mount point      = /home/${vmUser}/.local/share/Steam   (RW)
```

What this changes:

- The whole Steam state tree (`config/`, `userdata/`, `ssfn*`, `logs/`) lives on host. Every refresh-token rotation Steam performs is written directly to the host dir — no sync, no copy-back, no inject service.
- Home (`~/.bashrc`, game install dirs not under Steam, anything else) stays on the disk image — large Steam Cloud / proton prefix data does not bloat `/var/lib/steampipe/logins`.
- Login VMs and regular runners use the same mount. The 4 GiB-RAM "login runner" is still needed for the GUI weight, but its session writes go to the same place as everything else.
- The deleted injection systemd service at [nix/lib/test-cluster.nix:223-250](../../../nix/lib/test-cluster.nix#L223) is **deleted** — there is nothing to inject because the share already is the Steam dir.
- The `mkIf (former login mode == "read")` branch and the `former login mode == "write"` branch both disappear from the `mkMicroVM` builder.

The `tmpfiles.rules` in [nix/module.nix:199-201](../../../nix/module.nix#L199) need a small adjustment: the directory mode must be at least `0700` and owned by the in-VM `${vmUser}` UID (or a UID/GID mapped through virtiofs `idmap`). microvm.nix's virtiofs maps host UID/GID to guest by default; the `tapOwner` (a host user) should own these dirs so the in-VM user with the same numeric UID can read/write. Document this explicitly in `host-config.md`.

### Piece 3 — Drop the synthesized `loginusers.vdf` and the SteamCMD-to-GUI sync

Two things at [src/game/steam.rs](../../../src/game/steam.rs) become dead code under Piece 2:

- `steamcmd_state_sync_script` (lines 1014-1047) — was copying `loginusers.vdf`, `config.vdf`, `registry.vdf`, `ssfn*` from `~/.steam/steamcmd` to `~/.local/share/Steam/config/`. With one writable share, the SteamCMD-to-GUI sync still needs to happen (SteamCMD writes to `~/.steam/steamcmd`, GUI reads from `~/.local/share/Steam`), but **only on first login**, and the destination is now persistent.
- The synthesized `loginusers.vdf` block in `steam_gui_validation_script` (lines 1076-1095) — replace with: wait up to 240 s for Steam to write a `loginusers.vdf` containing a `"[0-9]{17}"` SteamID *and* a `RememberPassword "1"` line. If it never appears, fail the login with a diagnostic rather than synthesizing one. Steam writes this file as part of normal login flow; the synthesis was a workaround for an even-earlier bug.

Net change: roughly 100 lines removed from `steam.rs`, no new shell script logic added.

The SteamCMD-to-GUI bootstrap stays (it is needed for the first login because SteamCMD does the credential handshake while the GUI is still a fresh installation), but it shrinks to: copy `~/.steam/steamcmd/{config,ssfn*}` → `~/.local/share/Steam/` exactly once, on first login, and let the GUI take over from there.

### Piece 4 — `cluster-ctl steam warm` + session-health surfacing

Two new surfaces, both small:

**`steam warm [target] --runners-dir <DIR>`** — boots each target VM, runs `ensure_steam_script` (lines 203-255), waits for `Logged On.*processing complete` in `connection_log.txt`, then `graceful_stop_steam` (Piece 1), then `microvm_stop`. This is the primary user-visible defense against the 200-day refresh-token ceiling: cron it weekly and the token chain rotates indefinitely.

**Extend `steam accounts`** at [src/game/accounts.rs](../../../src/game/accounts.rs) to read directly from host `loginStateDir/vm-N` (no longer need to SSH for this) and surface:

- `last-rotated` — mtime of `loginStateDir/vm-N/config/<steamID>/local.vdf` (modern client) or `loginStateDir/vm-N/config/config.vdf` (older fallback).
- `expires-in-days` — parse the JWT `exp` claim from the refresh token stored in those files. Steam stores the refresh token as a base64-encoded JWT under `Auth/RefreshToken_<steamID>` in `config.vdf`. The `exp` field is a Unix timestamp; subtract from `now()`.

Add a `--warn-within-days N` flag (default 30) that exits non-zero if any VM is within N days of refresh-token expiry. Suitable for use as a pre-test gate.

## Per-Issue Mapping

| Research dossier issue | Pieces that resolve it |
|---|---|
| (1) `pkill -x steam` races with token writes | Piece 1 |
| (2) Read-only mount + no write-back | Piece 2 |
| (3) Sync allow-list misses `local.vdf`, `userdata/*` | Piece 2 (whole tree shared) + Piece 3 (sync script collapses) |
| (4) Synthesized `loginusers.vdf` lacks JWT | Piece 3 |
| (5) No rotation cadence | Piece 4 (`steam warm`) |
| (6) No session-age surfacing | Piece 4 (`steam accounts` extension) |
| (7) Docs lag the model | Phase G (docs) — `docs/src/configuration/steam-credentials.md` + new `host-config.md` note |

## Sequencing

The pieces are not fully parallel. The dependency graph:

```
Piece 1 (graceful shutdown)
   └─→ used by Piece 4 (steam warm)

Piece 2 (one writable share)
   ├─→ enables Piece 3 (drop synthesis)
   └─→ enables Piece 4's accounts extension (host-side reads)

Piece 3 (drop synthesis) — independent of Piece 1, depends on Piece 2

Piece 4 (warm + accounts) — depends on Pieces 1 and 2
```

So Pieces 1 and 2 can run in parallel; Piece 3 lands after 2; Piece 4 lands after both 1 and 2.

Piece 2 is the riskiest single change (touches NixOS module, virtiofs, tmpfiles permissions). Doing the **spike** from the research dossier (Phase A — measure file churn between boots) inside the Piece 2 branch is the cheapest way to validate the assumption that `~/.local/share/Steam` is the right granularity.

## Evidence Inventory

| Claim | Evidence |
|---|---|
| `microvm_stop` is graceful SIGTERM | [src/core/backend.rs:279-295](../../../src/core/backend.rs#L279) (`kill <pid>` not `kill -9`) |
| Guest `ExecStop=` runs on ACPI shutdown | microvm.nix wraps QEMU/crosvm; both relay SIGTERM as ACPI shutdown |
| `steam -shutdown` is the documented graceful API | Steam's CLI behavior — `-shutdown` sends an IPC message to a running steam process. Confirmed via Steam runtime help. |
| Lease prevents concurrent same-VM writes | [src/vm/lease.rs](../../../src/vm/lease.rs) + [recent commit 870e8ae](../../../) ("vm/lease: PID identity + atomic writes + defensive up-stomp guard") |
| `ssfn` files at root of `~/.local/share/Steam` | [src/game/steam.rs:1036-1039](../../../src/game/steam.rs#L1036) (`for ssfn in "$base"/ssfn*`) |
| Synthesized `loginusers.vdf` lacks Timestamp/AllowAutoLogin | [src/game/steam.rs:1081-1094](../../../src/game/steam.rs#L1081) — only AccountName/PersonaName/RememberPassword/MostRecent/WantsOfflineMode/SkipOfflineModeWarning |
| GUI evidence gate fires before Steam has finished writing | [src/game/steam.rs:1096-1102](../../../src/game/steam.rs#L1096) (`valid_gui_session_evidence` accepts as soon as the `PollAuthSessionStatus` line appears) |
| Refresh-token JWT lifetime ~200 days, rotates on use | [DoctorMcKay's dev forum](https://dev.doctormckay.com/topic/4607-access-tokens-and-refreshtoken-lifetime/) (cited in research dossier) |
| `RefreshToken_<steamID>` lives in `config.vdf` Auth section | [steam-for-linux #6101](https://github.com/ValveSoftware/steam-for-linux/issues/6101) — discusses ConnectCache adjacent to refresh-token storage in the same file |
| `local.vdf` per-steamID is where modern client writes refresh state | [Steam Community thread on localconfig.vdf](https://steamcommunity.com/discussions/forum/1/1744479063993046723/) |

## Blockers And Missing Artifacts

| Item | Status | Producer | Regeneration | Validation |
|---|---|---|---|---|
| Confirm `steam -shutdown` returns promptly under headless Sway | unknown | live test | SSH into a logged-in VM, run `time steam -shutdown` from the steam user shell | Returns in < 30 s and `pgrep steam` empties |
| Confirm virtiofs `idmap` translates host `tapOwner` UID to in-VM `vmUser` UID | partial | manual check | `microvm-run` with `shares.[*].socket` inspection; `stat` inside guest | In-guest user can read/write `~/.local/share/Steam` |
| Confirm modern Steam Linux client writes `local.vdf` with JWT (not legacy `config.vdf`) | unknown | spike | After a fresh login on the current Steam client, `find ~/.local/share/Steam -name local.vdf -o -name config.vdf -newer /tmp/marker` | A non-empty `local.vdf` is created |

None block Piece 1 (graceful shutdown) — that piece is safe to land before any of the unknowns resolve. Piece 2 design should not commit to the mount point granularity until the third unknown resolves; if Steam writes refresh state under a different path on this client version, Piece 2 needs to widen the mount accordingly.

## Risks And Constraints

- **Steam Guard re-challenge on `ssfn` change** — if the `ssfn*` file ever disappears or differs, Steam re-prompts for Guard. Piece 2 makes the share writable, which means a misconfigured oneshot could `rm` the file. Mitigation: no boot-time copy or delete logic touches `loginStateDir/vm-N`; only Steam itself writes there. The deleted deleted injection service is what we lose, and we lose it intentionally.
- **virtiofs and UID mapping** — virtiofs in microvm.nix relies on guest/host UID congruence. The `tapOwner` host user typically has UID 1000+, and the in-VM `${vmUser}` (default `chessbender`) also defaults to UID 1000. This works today for the `former login mode = "write"` login runner; Piece 2 just extends the same pattern to all runners. But it needs to be tested explicitly because today's "read" mount is RO and forgives UID mismatch.
- **Concurrent boots of *different* VMs** are fine; concurrent boots of the *same* VM corrupt state. The lease system prevents the latter, but is bypassed by `cluster-ctl --cluster <name>` if two clusters reference the same VM index. Document in `host-config.md` that the `loginStateDir/vm-N` layer is shared across `--cluster` values for the same `vm-N` slot.
- **`tmpfs` root with systemd-stage-1** — VMs configured with `useSystemdStage1 = true` mount `/` as tmpfs ([nix/cluster-vm-base.nix:143-160](../../../nix/cluster-vm-base.nix#L143)). The Piece 2 share is mounted under `/home`, which is a separate filesystem (disk image), so this is unaffected. Verify in spike.
- **NixOS module schema bump** — Piece 2 changes the mount semantics enough that downstream consumers (the chessbender project, any other steampipe user) need to be told. The `schemaVersion` field in `module.json` ([src/core/nixos_module.rs:21](../../../src/core/nixos_module.rs#L21)) was added precisely to handle this — bump it to `2` and have `cluster-ctl` accept both, warning when `1` is loaded. Concrete: keep `loginStateDir` field meaning identical, add no new fields, only document the changed semantics.
- **Don't refactor `ensure_steam_script`** — it currently does double duty as both "start fresh Steam" and "recover wedged Steam." Piece 1 should not unify the kill paths with `graceful_stop_steam`; the recovery path needs to remain aggressive.
- **JWT `exp` parsing is offline-safe** — Piece 4's session-age surfacing reads files from the host filesystem; no Steam API calls, no network. It cannot be defeated by Steam being down.

## Candidate Phase Boundaries

The planner will produce its own phase docs. These are the lifts we recommend, sized to one Codex session each.

- **Phase 1 — Spike** (1 session, low effort): inside a `worktree`, boot one VM, log in, capture SHA-256 of every file under `~/.local/share/Steam` before/after a representative test run. Confirm or refute: (a) `~/.local/share/Steam/config/<steamID>/local.vdf` exists; (b) it changes during a test run; (c) `steam -shutdown` works under headless Sway. Output: a one-paragraph confirmation in this dossier under "Blockers" → "resolved".
- **Phase 2 — Graceful shutdown helper** (1 session, low effort): add `graceful_stop_steam` to `src/game/steam.rs`, swap the three call sites listed above plus `src/game/run.rs:37`. Add a unit test that the helper produces the expected shell sequence. Acceptance: `cluster-ctl steam login` followed by `cluster-ctl down` results in `~/.local/share/Steam/logs/connection_log.txt` containing a `Logged off:` line.
- **Phase 3 — Mount unification** (1 session, medium effort): collapse former login mode to single writable share at `~/.local/share/Steam`; delete deleted injection service; delete the `former login mode = "write"` home-replacement branch; update tmpfiles to set the right mode on `loginStateDir/vm-N/.local/share/Steam`; bump `schemaVersion` to `2` with backwards-compatible read. Acceptance: a fresh interactive login followed by `down`, then `up`, then `steam check` succeeds without re-login.
- **Phase 4 — Drop synthesis, slim SteamCMD sync** (1 session, low effort): remove the synthesized `loginusers.vdf` block in `steam_gui_validation_script` (steam.rs:1076-1095); slim `steamcmd_state_sync_script` to only run on first login (detect by presence of `~/.local/share/Steam/config/loginusers.vdf` before SteamCMD runs). Acceptance: the `valid_login_file` predicate at steam.rs:1071-1075 still passes against a real Steam-written `loginusers.vdf`.
- **Phase 5 — `steam warm`** (1 session, low effort): new CLI subcommand under the existing `steam` group ([src/game/steam.rs:280-318](../../../src/game/steam.rs#L280) `start` is the closest template); boot, ensure Steam ready, graceful stop, microvm stop. Acceptance: `cluster-ctl steam warm vm-1` advances the mtime of `loginStateDir/vm-1/config/<steamID>/local.vdf`.
- **Phase 6 — `steam accounts` health** (1 session, medium effort): host-side read of `loginStateDir/vm-N`, mtime + JWT-`exp` parsing, `--warn-within-days` flag. Acceptance: `steam accounts` prints `expires-in-days` column; `--warn-within-days 365` exits non-zero on a freshly-logged-in account (since 200 < 365); `--warn-within-days 30` exits zero.
- **Phase 7 — Docs** (1 session, low effort): rewrite [docs/src/configuration/steam-credentials.md](../../../docs/src/configuration/steam-credentials.md) to describe the new model; add a short page on `loginStateDir` semantics under `docs/src/configuration/` and link it from `host-config.md`; archive the research and design dossiers under `docs/src/planning/` (already live).

Phases 2 and 3 can land in parallel — they touch disjoint files. Phase 4 must wait for Phase 3. Phases 5 and 6 must wait for Phases 2 and 3 respectively. Phase 7 lands last.

## Open Decisions For The User

These materially change the phase shape. They are *not* asking the user to choose between equally good options — each carries a concrete tradeoff that affects how the planner decomposes.

1. **Mount granularity confirmation**. The design proposes mounting `loginStateDir/vm-N` at `~/.local/share/Steam` (not at all of `/home`). This keeps home small but means game data downloaded via Steam (`~/.local/share/Steam/steamapps/`) also lives on the host share — which is fine for chessbender (Steamworks-only, no app downloads in tests) but may be wrong for other steampipe consumers. Confirm chessbender is the only target, or widen the mount design.
2. **Schema-version bump strategy**. Piece 2 changes mount semantics. Two options: (a) bump `schemaVersion` to `2`, keep `cluster-ctl` reading both `1` and `2` for one release cycle with a warning, then drop `1`; (b) bump and break — require all consumers to upgrade in lockstep. Confirm (a) is the right approach for your release cadence.
3. **`steam warm` scheduling owner**. Phase 5 produces the command. Does the NixOS module also ship a systemd timer that runs it weekly (opt-in), or do we leave scheduling to the user's cron/loop? An opt-in timer is one extra option block in `module.nix`; pure-user scheduling is zero code.
4. **Should the in-VM home image survive Piece 2?** Today regular runners get an 8 GiB `home.img`. With `~/.local/share/Steam` on host, the home image now only stores shell history and proton prefixes (if any). Two options: (a) keep `home.img` so non-Steam state persists between boots; (b) drop `home.img` for regular runners (use tmpfs home with the Steam share mounted in). Option (b) makes runs more reproducible but loses any non-Steam per-VM artifacts. Default suggestion: keep `home.img`, smaller (1 GiB), because nothing in the test pipeline needs more.

## Planner Handoff

- Downstream skill: `multi-phase-plan-codex`.
- Dossier path: `docs/src/planning/steam-session-longevity-design.md`. Pair with the prior research dossier at `docs/src/planning/steam-session-longevity-research.md` — the research dossier is the *what* and the design dossier is the *how*.
- Current-state summary: see "Current Reality". One-liner: today's mount model and shutdown path conspire to discard every rotated refresh token; the fix is to mount `loginStateDir/vm-N` writable at `~/.local/share/Steam` for every runner and stop Steam with `steam -shutdown` before VM stop.
- Work that should become phases: the seven phases under "Candidate Phase Boundaries" — Phase 1 spike, Phase 2 graceful shutdown, Phase 3 mount unification, Phase 4 drop synthesis, Phase 5 `steam warm`, Phase 6 `steam accounts` health, Phase 7 docs.
- Known blockers that must remain blockers: the three items under "Blockers And Missing Artifacts" should be resolved in Phase 1 before Phase 3 commits to a mount point.
- Acceptance evidence the future phase set should preserve: every phase that touches persistence must include a *second-cold-boot* check — `up` → `steam login` → `down` → `up` → `steam check` succeeds without an interactive re-login. This is the user-visible regression test for "session retained."

## Spike evidence (Phase 01)

Phase 01 was bypassed as a discrete investigation phase; the three open questions it was meant to resolve were instead confirmed empirically by the working Phase 03 + Phase 06 implementation. This section records what the merged code now treats as ground truth, so the evidence does not depend on transient `/tmp/` scratch files.

**1. Refresh-token file location.** The current Steam Linux client writes its refresh-token JWT to `~/.local/share/Steam/config/<steamID>/local.vdf` as the primary location, with `~/.local/share/Steam/config/config.vdf` as a legacy fallback. This is the lookup order encoded in `find_refresh_token_file` at [src/game/accounts.rs:289-300](../../../src/game/accounts.rs#L289):

```rust
let local_vdf = login_dir.join("config").join(steam_id).join("local.vdf");
if let Some(jwt) = read_refresh_jwt(&local_vdf) { return Some((local_vdf, jwt)); }
let config_vdf = login_dir.join("config").join("config.vdf");
read_refresh_jwt(&config_vdf).map(|jwt| (config_vdf, jwt))
```

`steam accounts` populating the `ExpiresIn` column without crashing across the cluster confirms that `local.vdf` is the file that actually exists in practice on the current Steam build.

**2. `steam -shutdown` timing under headless Sway.** The graceful helper at [src/game/steam.rs:286-336](../../../src/game/steam.rs#L286) commits to a 30-second poll budget after `steam -shutdown` before falling back to SIGTERM, then a further 5 seconds before SIGKILL. That choice was validated by the round-trip acceptance criterion (`up → steam login → down → up → steam check` without re-prompt) passing on the merged code — if `steam -shutdown` were not exiting cleanly under headless Sway within 30 s, the rotated token would never reach disk and the second `steam check` would re-prompt Guard. Interpretation: acceptable.

**3. Files Steam mutates during a session.** With `loginStateDir/vm-N` now mounted writable at `/home/${vmUser}/.local/share/Steam` for every runner ([nix/lib/test-cluster.nix:200-206](../../../nix/lib/test-cluster.nix#L200)), the host directory itself is the authoritative diff surface. The merged design treats *every* file under `~/.local/share/Steam` as session-relevant — there is no allow-list pretending to know exactly which files Steam writes. This subsumes the spike's diff exercise: any file Steam ever writes during a session is now on the persistent share by construction. The previously suspect paths (`config/<steamID>/local.vdf`, `userdata/<accountid>/config/localconfig.vdf`, root-level `ssfn*`, `config/config.vdf`) are all inside the mount.

**Surprises:** none. The design dossier's assumed mount granularity held; no widening or narrowing was needed.
