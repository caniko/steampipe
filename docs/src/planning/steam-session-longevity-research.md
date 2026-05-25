# Steam Session Longevity Research Dossier

## Goal And Trigger

User reports that the Steam login session on cluster VMs is often lost "after a while," forcing a re-login (and frequently a fresh Steam Guard challenge). The goal of this dossier is to map exactly what state cluster-ctl persists between boots, where the gap is between "Steam wrote a fresh token" and "next boot reuses it," and what concrete options exist to keep sessions valid for the maximum possible window.

Downstream planner: defaults to `multi-phase-plan-codex` (user did not specify). Phases will be decomposed against this dossier — this skill does not produce them.

## Current Reality

Login state flow today (evidence cited below):

1. **Login VM run** (`cluster-ctl steam login`) boots a 4 GB-RAM "login runner" with `loginStateDir/vm-N` mounted **writable** as the VM user's home (virtiofs). Steam therefore writes session files directly onto the host login state dir.
2. **Login completion** synthesizes a minimal `loginusers.vdf` from the connection log if Steam hasn't written its own yet, then `pkill -x steam`, sleep 2s, stop the VM.
3. **Regular runner boots** (used by `up`, `test`, `steam check`) mount the same host dir at `/mnt/steam-login` **read-only**. A NixOS oneshot (old injection unit) copies `config/loginusers.vdf`, `config/*`, `ssfn*`, and `registry.vdf` into the writable home before Steam starts.
4. **Home volume** for regular runners is a per-VM 8 GiB disk image (`vm-N-home.img`). All Steam writes (including new refresh tokens Steam rotates on every login) go to that image. The image is recreated whenever the runner is rebuilt and is **never synced back** to `loginStateDir/vm-N`.

Net effect: only the snapshot taken at *initial interactive login* survives. Anything Steam updates afterwards (rotated refresh tokens, fresh `ssfn*`, ConnectCache JWTs, `local.vdf` per-steamID state) dies with the home image.

## Evidence Inventory

| Claim | File:Line | What it proves |
|---|---|---|
| Login VM mounts host dir writable | [nix/lib/test-cluster.nix:200-219](../../../nix/lib/test-cluster.nix) | `former login mode == "write"` virtiofs of `loginStateDir/vm-N` as `/home/${vmUser}`. Login flow can persist. |
| Regular VM mount is read-only | [nix/lib/test-cluster.nix:200-219](../../../nix/lib/test-cluster.nix) | Non-login runners mount `loginStateDir/vm-N` at `/mnt/steam-login` with `readOnly = true`. |
| Inject is one-way copy on boot | [nix/lib/test-cluster.nix:223-250](../../../nix/lib/test-cluster.nix) | deleted injection oneshot copies `config/*` and `ssfn*` from share into home; no symmetric write-back service. |
| Home is per-VM disk image, 8 GiB | [nix/lib/test-cluster.nix:191-197](../../../nix/lib/test-cluster.nix) | `volumes = [{ mountPoint = "/home/${vmUser}"; image = "${name}-home.img"; size = 8192; }]` when `former login mode != "write"`. |
| Sync copies a fixed file list only | [src/game/steam.rs:1014-1047](../../../src/game/steam.rs) | `steamcmd_state_sync_script` copies `loginusers.vdf`, `config.vdf`, `registry.vdf`, `ssfn*`. No `~/.local/share/Steam/config/<steamID>/local.vdf`, no `~/.local/share/Steam/userdata/<accountid>/config/localconfig.vdf`. |
| `loginusers.vdf` synthesis is heuristic | [src/game/steam.rs:1076-1095](../../../src/game/steam.rs) | Writes only `AccountName / PersonaName / RememberPassword=1 / MostRecent=1 / WantsOfflineMode=0 / SkipOfflineModeWarning=0`. No refresh-token, no Timestamp, no AllowAutoLogin. |
| GUI session evidence is only the connection-log line | [src/game/steam.rs:1096-1102](../../../src/game/steam.rs) | Validation succeeds as soon as `PollAuthSessionStatus succeeded and has refresh token` appears, *before* Steam has finished writing its own `config.vdf` / `local.vdf`. |
| Steam is always killed with SIGTERM/SIGKILL, never `steam -shutdown` | [src/game/steam.rs:226-229, 539, 619, 1118](../../../src/game/steam.rs); [src/game/run.rs:37](../../../src/game/run.rs) | `pkill -x steam` (sometimes followed by `pkill -KILL` after 3s). Steam's clean shutdown — which flushes session state — is never invoked. |
| `clean-logins` wipes the host dir | [src/game/steam.rs:1408-1477](../../../src/game/steam.rs) | Confirms `loginStateDir/vm-N` is the canonical persistence root; nothing else is searched. |
| External: refresh-token validity | [DoctorMcKay's dev forum](https://dev.doctormckay.com/topic/4607-access-tokens-and-refreshtoken-lifetime/) | Steam-client refresh tokens are JWTs valid ~200 days when "Remember password" is set, but they **rotate on use**. The old token becomes invalid as soon as the new one is issued. |
| External: ConnectCache lives in `config.vdf` | [steam-for-linux #6101](https://github.com/ValveSoftware/steam-for-linux/issues/6101) | `ConnectCache` (long-lived auth blob) is in `config/config.vdf`, supporting the inference that the SteamCMD-style sync misses what the GUI client writes per-steamID. |
| External: per-account state is in `userdata/<accountid>/config/localconfig.vdf` and `config/<steamID>/` | [steamcommunity Help thread](https://steamcommunity.com/discussions/forum/1/1744479063993046723/) | Confirms locations not in the current sync allow-list. |
| Snapshot subsystem does not capture VM home images | [src/vm/snapshot.rs:6-67](../../../src/vm/snapshot.rs) | Snapshots are taken from `config.state_dir` (host PID/socket dirs); no qcow2 / disk-image content. So even snapshot-restore cannot replay a "warm" Steam state into a new boot. |

## Existing Plan Status

No active planning docs target Steam session longevity. The existing planning directories under `docs/book/planning/` cover VM scheduling, in-tree fixtures, VNC visual testing, host-config discovery, and NixOS-module CLI awareness. The git history shows multiple incremental login-related commits (`Bootstrap Steam login with SteamCMD`, `Auto-login Steam on VM boot and system-level login persistence`, `Always expose Steam login state`, `Retry Steam login after client update`, `Avoid repeated Steam Guard challenges`) but none address token persistence after the initial login VM run.

There is therefore no prior plan to retire or fold in — this dossier seeds a new line of work.

## Work That Should Survive Into The Long-Term Plan

These are the candidate problem statements the future phase set must address, in rough dependency order:

1. **Define what "the session" actually is.** Today the codebase treats `loginusers.vdf` + `ssfn*` as the session. In reality the modern Steam Linux client also writes to `config/<steamID>/local.vdf`, `userdata/<accountid>/config/localconfig.vdf`, and `config/config.vdf` (ConnectCache / SteamLoginSecure). The plan must pick one of: (a) include all of these in the persistence set, or (b) document why the subset is intentional.
2. **Stop killing Steam without `steam -shutdown`.** The `pkill -x steam` pattern at [src/game/steam.rs:226-229, 539, 619](../../../src/game/steam.rs#L226) and [src/game/run.rs:37](../../../src/game/run.rs#L37) likely loses the post-rotation token, which is the most common silent expiry path. Plan must wire a graceful shutdown helper that runs `steam -shutdown` and polls until `pgrep steam` empties (with a SIGTERM fallback after, say, 30 s).
3. **Make the regular-runner mount writable, or add a write-back path.** Three viable shapes:
   - flip `former login mode = "read"` to `"write"` for the test runners too (cheapest, but loses isolation across runs — different test runs of the same VM start to interfere);
   - add an overlayfs in-VM (lower = read-only host share, upper = ephemeral tmpfs), then a systemd `ExecStop` that rsyncs the relevant paths from the upper layer back to a small writable virtiofs mount;
   - add a `cluster-ctl steam sync-back` SSH-driven copy that runs after `steam -shutdown` and before VM stop.
4. **Decide the rotation/refresh policy.** Even with perfect persistence, Steam will eventually invalidate the chain (account inactivity, password change, Valve revocation). Plan should specify a cadence — e.g. weekly `cluster-ctl steam warm` that boots each VM, runs Steam to refresh, gracefully shuts down. This sidesteps the 200-day ceiling entirely.
5. **Replace the synthesized `loginusers.vdf`.** The current synthesis (steam.rs:1076-1095) papers over the case where Steam exits before writing its own file. The plan should remove the synthesis once graceful-shutdown + write-back land, because the synthesized file lacks the Timestamp/AllowAutoLogin fields and likely contributes to the symptoms.
6. **Surface session age and rotation health.** `cluster-ctl steam accounts` shows logged-in state but not "token was last rotated N days ago." Adding age + countdown to expiry (read mtime of `config/<steamID>/local.vdf` or parse the JWT `exp`) would give the user a visible warning before a session lapses.
7. **Document the chosen model** in `docs/src/configuration/steam-credentials.md` so the contract between login VM, test runner, and host dir is no longer implicit.

## Blockers And Missing Artifacts

| Item | Status | Producer | How to regenerate | Validation |
|---|---|---|---|---|
| Authoritative list of files Steam writes after first login | unknown | manual experiment | Run `cluster-ctl steam login vm-1`, then `find $loginStateDir/vm-1 -mmin -10` after a few boots/quits | Diff against the current sync allow-list in [src/game/steam.rs:1014-1047](../../../src/game/steam.rs) |
| Confirm refresh-token rotation actually breaks sessions in this setup | unknown | log analysis | Capture `connection_log.txt` over a sequence of cold boots, look for `RefreshTokenSubmittedForOAuth` vs new `Steam Guard code:` prompts | If the second boot re-prompts Guard, hypothesis is confirmed |
| Whether `steam -shutdown` works under headless Sway used in test runs | unknown | live test | SSH into a logged-in VM and run `steam -shutdown` from the steam user shell; check `wait` semantics + log | Steam process exits cleanly within ≤30 s and `pgrep steam` becomes empty |
| `loginStateDir` permissions in the read+write split | partial | inspection | `stat /var/lib/steampipe/logins/vm-1` and confirm `tapOwner` ownership matches what virtiofs needs | Login VM should be able to write; test VMs should not be able to corrupt via overlay write-back |

None of these are hard blockers for *planning* — they are blockers for *implementation*. The plan should schedule them as pre-implementation spikes.

## Risks And Constraints

- **Steam Guard re-challenge** — any change that alters the `MachineName` Steam sees (e.g. switching from `~/.local/share/Steam` on disk image to `~/.local/share/Steam` on virtiofs share with different device-id semantics) will trigger Steam Guard re-prompting on next login. Persistence must preserve the `ssfn*` machine-trust file along with the rest, otherwise the first re-login after a change will demand a guard code.
- **Concurrency** — multiple VMs can run the same Steam account if a credentials file is misconfigured. Making `loginStateDir` writable to all runners simultaneously risks two VMs racing to write `config.vdf`. Today this is constrained by `vm-N` per-account isolation, which must be preserved.
- **Snapshot semantics** — current snapshots ([src/vm/snapshot.rs](../../../src/vm/snapshot.rs)) operate on `config.state_dir` only and explicitly do not include VM home images. Any plan that introduces "session warmth restored by snapshot" needs to either expand snapshot scope or stay separate from snapshot-restore.
- **NixOS module shape** — the module pins `loginStateDir = /var/lib/steampipe/logins` ([nix/module.nix:169-178](../../../nix/module.nix#L169)) with read-by-default `users` group. Switching to overlayfs or a writable share for normal runners likely requires module-level changes (tmpfiles rules, possibly `tapOwner` adjustments).
- **`tmpfs` root with systemd-stage-1** — VMs configured with `useSystemdStage1 = true` mount `/` as tmpfs ([nix/cluster-vm-base.nix:143-160](../../../nix/cluster-vm-base.nix#L143)). The home volume is a separate disk image, so Steam state inside the home volume survives reboots, but anything Steam writes outside `/home/${vmUser}` (e.g. `/tmp`) does not. Any redesign needs to keep Steam confined to the persistent paths.
- **`pkill -x steam` is intentional in places** — the comment context for the kill at [src/game/steam.rs:226-229](../../../src/game/steam.rs#L226) shows it is used to recover a stuck/stale Steam during `ensure_steam`. The plan must distinguish "Steam is wedged, must die" (where `-KILL` is fine) from "session is good, we want to stop" (where `-shutdown` is correct).

## Candidate Phase Boundaries

These are slices the planner can lift; they are not committed phase numbers.

- **Phase A — Spike: measure today's loss.** Add ad-hoc instrumentation to print SHA-256 of `config/<steamID>/local.vdf`, `config/config.vdf`, and `ssfn*` before and after a representative test run. Goal: confirm/refute that Steam writes new bytes during a run and that those bytes are lost. Spike only — no production code expected.
- **Phase B — Graceful shutdown of Steam.** Introduce a `graceful_stop_steam(backend, ip, vm_user)` helper that runs `steam -shutdown`, polls `pgrep steam` up to N seconds, falls back to SIGTERM then SIGKILL. Replace `pkill -x steam` call-sites: [src/game/steam.rs:226-229, 539, 619, 1118](../../../src/game/steam.rs#L226) and [src/game/run.rs:37](../../../src/game/run.rs#L37). Acceptance: `connection_log.txt` shows a `Logged off:` line on shutdown and `ssfn*` mtimes update across a boot/quit cycle.
- **Phase C — Expand sync allow-list.** Update [src/game/steam.rs:1014-1047](../../../src/game/steam.rs#L1014) to also copy `config/<steamID>/`, `userdata/<accountid>/config/localconfig.vdf` (anything under `userdata`), and any `*.vdf` newer than the login-runner start time. Drop the synthesized `loginusers.vdf` ([src/game/steam.rs:1076-1095](../../../src/game/steam.rs#L1076)) once the sync covers the real file. Acceptance: a logged-in VM that is wiped and re-injected from `loginStateDir/vm-N` still passes `steam check` without re-login.
- **Phase D — Write-back for regular runners.** Pick one of: writable virtiofs, overlayfs + rsync on shutdown, or SSH-driven copy after `steam -shutdown`. The overlayfs option keeps run-to-run isolation while still capturing token rotation. Touches [nix/lib/test-cluster.nix:191-262](../../../nix/lib/test-cluster.nix#L191). Acceptance: after a test run that exercises Steam, the host `loginStateDir/vm-N/.../config/<steamID>/local.vdf` mtime advances.
- **Phase E — Refresh cadence.** Add `cluster-ctl steam warm [vm-N|all]` that boots each VM, lets Steam come up, waits for `PollAuthSessionStatus succeeded`, gracefully stops. Suitable for cron/`/loop` weekly. Acceptance: running `steam warm` before any test session prevents the "session lost" failure mode in long gaps.
- **Phase F — Session health surfaced.** Extend `cluster-ctl steam accounts` to show `last-rotated` (mtime) and `expires-in-days` (parse JWT `exp` from `config/config.vdf` or `local.vdf`). Acceptance: `steam accounts` warns when any VM is within 14 days of expiry.
- **Phase G — Docs.** Update [docs/src/configuration/steam-credentials.md](../../../docs/src/configuration/steam-credentials.md) and add a short page on the persistence model. Acceptance: documented contract matches the implemented behaviour from phases B-F.

Phases B, C, D each have natural sub-layers (one for runtime code, one for shell scripts, one for tests) and the planner may parallelize per `multi-phase-dispatch` rules. Phase E and F are independent of D once B has landed.

## Open Decisions For The User

These materially change the phase shape:

1. **Acceptable isolation trade-off.** Are you OK with the regular-runner home dir becoming a writable virtiofs share to host (loses run-to-run isolation in exchange for free persistence), or do you require the overlayfs/copy-back design (more code, keeps isolation)?
2. **Synthesized `loginusers.vdf` — keep or remove?** Confirm that the heuristic at [src/game/steam.rs:1076-1095](../../../src/game/steam.rs#L1076) was a workaround you would prefer to retire once Steam shutdown is graceful.
3. **`steam warm` cadence default.** If we add Phase E, do you want it auto-scheduled (e.g. a systemd timer in the NixOS module), or strictly user-driven (`cluster-ctl` only)?
4. **Scope: this repo only, or also chessbender?** Some session-warming would benefit from a `chessbender`-side helper that exits cleanly on a known signal. Confirm whether changes are confined to steampipe.

## Planner Handoff

- Downstream skill: `multi-phase-plan-codex` (default; switch to `-claude` or `-mixed` on request).
- Dossier path: `docs/src/planning/steam-session-longevity-research.md`.
- Current-state summary: see "Current Reality". One-liner: the cluster persists only the initial-login snapshot; every Steam-token rotation after that is written to a per-VM disk image and discarded, while shutdown is a hard `pkill` that may interrupt the very write that would have rotated the token onto the host share.
- Work that should become phases: A (spike), B (graceful shutdown), C (sync allow-list + drop synthesis), D (write-back), E (warm cadence), F (session-health surfacing), G (docs). See "Candidate Phase Boundaries" for acceptance hints.
- Known blockers to keep as blockers (do not let phases plan around them): authoritative file-list spike (Phase A is the discovery), `steam -shutdown` behaviour under headless Sway (also Phase A scope), and concurrency safety of any write-back path (Phase D design constraint).
- Acceptance evidence the future phase set should preserve: every phase that touches persistence must include a check that a *second cold boot, with no fresh interactive login*, results in `cluster-ctl steam check` reporting OK — this is the user-visible regression test for "session retained."
