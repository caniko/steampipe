# Phase 02 — Unified resolver for runners-dir, ssh-key, and friends

> **Recommended Codex model: GPT 5.5 medium**
>
> Touches multiple call sites in `src/lib.rs` (and a few error-message
> strings in `src/ui/cli.rs`). The refactor is mechanical *per site*
> but the agent must hold the precedence rule (CLI flag > project
> config > host config > error) coherently across every Steam
> subcommand and must rewrite each error message to name the right
> NixOS option. That's moderate complexity at a leaf-to-sub-agent
> role — 5.5 medium covers the multi-site sweep without inflating to
> high.

## Working tree

`/data/nvme0/can/Projects/steampipe` (the steampipe repository).

## Goal

`cluster-ctl steam login` and the rest of the Steam subcommand family
no longer hard-bail on `--login-runners-dir` (and friends) when a host
config provides them. A single resolver helper enforces a uniform
precedence:

```
CLI flag  >  project config  >  host config  >  error naming the NixOS option
```

The user can run `cluster-ctl steam login` from `$HOME` on atlas and,
provided phase 03 has wired `/etc/steampipe/module.json` to emit
`loginRunnersDir`, the CLI uses that path automatically. When the
path is *not* in the host config, the error message names both the
flag (`--login-runners-dir`) **and** the NixOS option
(`services.steampipe-cluster.loginRunners.enable = true`).

## Why this matters now

Phase 01 added the schema fields. Without phase 02, those fields sit
inert — the CLI still calls `require_login_runners_dir` and bails
([src/lib.rs:461-474](../../../../src/lib.rs#L461-L474)) without
checking the host config. This phase is the unlock for the entire
config-less story.

Originating symptom (user transcript):

```text
--login-runners-dir is required when running outside a project root
```

The error today doesn't mention the NixOS module option that would
populate the field, so users don't know how to fix it config-side.

## Out of scope

- Adding new CLI flags. We only change the *resolution* of existing
  flags. The CLI surface stays identical.
- Emitting `loginRunnersDir` from the NixOS module — phase 03.
- Touching `nix/lib/test-cluster.nix` wrappers — phase 06.
- Help-text rewrites on `Cli` global flags or `SteamAction` variants
  — that's docs/UX (phase 05).
- Schema reader changes — done in phase 01.

## Plan

1. **Confirm phase 01 has landed.** `git log` should show the commit
   adding `login_runners_dir`, `ssh_key`, `vm_user` to
   `NixosModuleConfig`. If not, stop and re-route.

2. **Design a single resolver helper.** Add to [src/lib.rs](../../../../src/lib.rs)
   near the existing `require_*` helpers (around line 440):

   ```rust
   fn resolve_path_field(
       flag_value: Option<PathBuf>,
       module: Option<&NixosModuleConfig>,
       extract: impl Fn(&NixosModuleConfig) -> Option<&PathBuf>,
       backend_kind: BackendKind,
       project_root: Option<&std::path::Path>,
       field_name: &str,
       cli_flag: &str,
       nixos_option: &str,
   ) -> anyhow::Result<PathBuf> {
       if let Some(path) = flag_value {
           return Ok(path);
       }
       if backend_kind != BackendKind::Microvm {
           return Ok(PathBuf::from("/dev/null"));
       }
       if let Some(path) = module.and_then(extract) {
           return Ok(path.clone());
       }
       if project_root.is_none() {
           anyhow::bail!(
               "{field_name} is not configured. Either pass `{cli_flag} <path>`, \
                or set `{nixos_option}` in the NixOS module and run \
                `nixos-rebuild switch`.",
           );
       }
       anyhow::bail!(
           "{field_name} is not configured. Either pass `{cli_flag} <path>` \
            from inside the project root, or set `{nixos_option}` in the \
            NixOS module."
       );
   }
   ```

   Keep the existing `require_runners_dir` (regular runners) and
   `require_login_runners_dir` as thin wrappers that delegate to
   `resolve_path_field`, so external call signatures don't churn.

3. **Wire `login_runners_dir` resolution.** At [src/lib.rs:820-841](../../../../src/lib.rs#L820-L841)
   the `SteamAction::Login` arm calls `require_login_runners_dir`.
   Rewrite that to call `resolve_path_field` with:

   ```text
   field_name    = "login runners dir"
   cli_flag      = "--login-runners-dir"
   nixos_option  = "services.steampipe-cluster.loginRunners.enable"
   extract       = |m| m.login_runners_dir.as_ref()
   ```

   Pass `module.as_ref()` (the module is already in scope from
   [src/lib.rs:505](../../../../src/lib.rs#L505)).

4. **Wire `runners_dir` resolution for `steam check` and `steam warm`.**
   Mirror the same pattern at [src/lib.rs:800-818](../../../../src/lib.rs#L800-L818).
   Use:

   ```text
   field_name    = "runners dir"
   cli_flag      = "--runners-dir"
   nixos_option  = "services.steampipe-cluster.runners.enable"
   extract       = |m| m.runners_dir.as_ref()   // new field; see step 5
   ```

   `runners_dir` is not in phase 01's field list. **Decision point:**
   either add it now in phase 02 (read-only — the NixOS module
   doesn't populate it yet, but the field is there for phase 03 to
   fill later) or defer. Recommendation: **add it now** as a fourth
   optional field in `NixosModuleConfig`, matching `login_runners_dir`.
   Update phase 01's host-config doc accordingly if not already done.
   The NixOS-module side wiring lands in phase 03.

5. **Wire `ssh_key` resolution.** [src/lib.rs:673-675](../../../../src/lib.rs#L673-L675)
   reads `cli.ssh_key`. Extend the assignment to fall through to
   `module.ssh_key`:

   ```rust
   if let Some(key) = &cli.ssh_key {
       config.ssh_key = key.clone();
   } else if let Some(key) = module.as_ref().and_then(|m| m.ssh_key.as_ref()) {
       config.ssh_key = key.clone();
   }
   ```

   Same pattern for `vm_user`: check `ClusterConfig::from_host_config`
   ([src/core/config.rs](../../../../src/core/config.rs)) for where
   `vm_user` is read. If it currently takes a hardcoded default
   `"cluster"`, plumb `module.vm_user` through as an override.

6. **Error message audit.** Every other `anyhow::bail!` in `src/lib.rs`
   that names a required field outside a project root must also name
   the corresponding NixOS option (or explicitly state "no NixOS
   module option exists for this — pass the flag"). Grep:

   ```sh
   rg -n 'anyhow::bail!|anyhow!\(' src/lib.rs
   ```

   For each, decide: does this error sit on a field the NixOS module
   could populate? If yes, mention the option name. If no, leave
   alone.

7. **Tests.** Add unit tests next to the resolver:

   - `resolve_path_field` returns `Ok(flag_value)` when the flag is set, regardless of module/project state.
   - Returns `Ok(module_field)` when flag is unset, module has the field, project root is `None`.
   - Returns `Ok(module_field)` when flag is unset, module has the field, project root is `Some(_)` (flag-wins is the only override; module beats nothing else).
   - Returns `Err` with both `cli_flag` and `nixos_option` substrings present, when nothing is configured and project root is `None`.

   Integration-style smoke test:
   - With a fixture `host.json` containing `loginRunnersDir`, invoke
     the existing CLI dispatch up to the resolver point (use the
     `STEAMPIPE_MODULE_CONFIG` env var
     ([src/core/nixos_module.rs:23](../../../../src/core/nixos_module.rs#L23))).
     Assert the resolver returns the fixture value.

8. **Run the existing test suite.**

   ```sh
   cargo test --package steampipe
   ```

   No existing test should break. New resolver tests must pass.

## Acceptance criteria

- [ ] `cargo test --package steampipe` passes; no existing tests modified except where they exercise the renamed wrapper helpers.
- [ ] `resolve_path_field` exists in [src/lib.rs](../../../../src/lib.rs); both `require_runners_dir` and `require_login_runners_dir` delegate to it.
- [ ] When `/etc/steampipe/module.json` contains `"loginRunnersDir": "<path>"`, `cluster-ctl steam login` from outside a project root resolves the path from the module instead of bailing on `--login-runners-dir`. Verify with a fixture file via `STEAMPIPE_MODULE_CONFIG=/tmp/fixture.json cargo run -- steam login --help` running cleanly *and* a real dispatch (with the actual login runners path; can be `/tmp` for parse-only verification — phase 04 does the real boot).
- [ ] Error message when nothing resolves contains *both* the CLI flag (`--login-runners-dir`) and the NixOS option name (`services.steampipe-cluster.loginRunners.enable`).
- [ ] `cli.ssh_key` falls through to `module.ssh_key` when omitted; `cli.vm_count` continues to fall through to `module.vm_count` (no regression).
- [ ] `cluster-ctl steam login --login-runners-dir /tmp/foo` (with the flag set) still resolves to `/tmp/foo` even when the module supplies a different path. Flag-precedence preserved.
- [ ] `grep -n require_login_runners_dir src/lib.rs` shows no callers outside the helper definition (or, if the wrapper is kept, only the wrapper itself).

## Files likely touched

- [src/lib.rs](../../../../src/lib.rs) — resolver helper + every Steam subcommand call site + fall-through for `ssh_key`/`vm_user`.
- [src/core/nixos_module.rs](../../../../src/core/nixos_module.rs) — possibly add `runners_dir` field if not done in phase 01.
- [src/core/config.rs](../../../../src/core/config.rs) — `ClusterConfig::from_host_config` may need a small `vm_user` override pass-through.
- New tests added inline to either of the above.

## Pitfalls

- **Forgetting backend gating.** The current `require_*` helpers
  return `/dev/null` when `backend_kind != BackendKind::Microvm`
  ([src/lib.rs:454-457, 468](../../../../src/lib.rs#L454-L457)). The
  unified resolver must preserve that — non-microvm backends (docker,
  local) don't need runners dirs. Without this, docker/local backends
  break.
- **Error message order.** When project root is present *and* a
  required field is missing, today's error says
  "`--login-runners-dir` is required for the microvm backend". The
  rewrite must still distinguish "no project root, no module"
  (suggest NixOS option *or* flag) from "project root present but
  field unset" (suggest only the flag — project-root mode doesn't
  consult the NixOS module path). Both branches need explicit error
  text.
- **Module reference lifetime.** `module` at
  [src/lib.rs:505](../../../../src/lib.rs#L505) is
  `Option<NixosModuleConfig>`. Pass `module.as_ref()` into the
  resolver. Cloning the whole module is wasteful but harmless if the
  agent reaches for `.clone()` — leave a comment if so.
- **`ssh_key` is a `PathBuf` in `ClusterConfig`, not `Option<PathBuf>`.**
  Check [src/core/config.rs](../../../../src/core/config.rs) for the
  default. The fall-through must respect whichever sentinel
  (`PathBuf::new()`, a hardcoded path) means "unset" today.
- **`runners_dir` schema field.** If you add it in this phase, update
  the host-config doc in the same commit. Phase 01's acceptance
  criteria don't cover it, so it's a phase-02 add-on, not a phase-01
  regression.
- **`steam check --runners-dir` is currently a `PathBuf` (required)
  in clap.** Look at [src/ui/cli.rs:582](../../../../src/ui/cli.rs#L582)
  — `runners_dir: Option<PathBuf>`. Good; no clap change needed.
  `steam warm` at [src/ui/cli.rs:597](../../../../src/ui/cli.rs#L597)
  has `runners_dir: PathBuf` (non-optional). Make it
  `Option<PathBuf>` to enable host-config resolution, and route
  through `resolve_path_field` like the others.

## Reference

- Dossier: [../cli-configless-from-nixos-research.md](../cli-configless-from-nixos-research.md) — see "Work That Should Survive > B. CLI Resolver Refactor" and "Current Reality > CLI demands today" (table).
- Existing resolver site: [src/lib.rs:440-474](../../../../src/lib.rs#L440-L474).
- Existing Steam dispatch sites: [src/lib.rs:798-869](../../../../src/lib.rs#L798-L869).
- Phase 01 (prerequisite): [01-host-config-schema-fields.md](./01-host-config-schema-fields.md).
- Phase 03 (parallel sibling, populates the field this phase reads): [03-nixos-module-login-runners.md](./03-nixos-module-login-runners.md).
