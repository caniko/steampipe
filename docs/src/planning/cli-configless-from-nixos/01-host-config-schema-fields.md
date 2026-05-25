# Phase 01 — Add optional fields to host config schema

> **Recommended Codex model: GPT 5.5 low**
>
> Adding three optional `Option<PathBuf>` fields to a serde-derived
> struct, extending two test fixtures to cover them, and editing the
> field-reference table in one markdown file. Mechanical leaf-level
> work with no design content. A smaller tier could ship this in one
> pass; 5.5 low gives margin for the test-fixture details without
> wasting reasoning tokens.

## Working tree

`/data/nvme0/can/Projects/steampipe` (the steampipe repository).

## Goal

`NixosModuleConfig` parses three new **optional** fields —
`loginRunnersDir`, `sshKey`, `vmUser` — from `host.json` / `module.json`
without breaking existing schemaVersion 1 or 2 readers. The
documentation reflects the new fields. No behavior change at the CLI
layer yet — this phase is pure capacity.

## Why this matters now

The dossier identifies `loginRunnersDir` as the load-bearing missing
field that forces `cluster-ctl steam login` to demand
`--login-runners-dir` even on canix-provisioned NixOS hosts. The CLI
already falls through `vmCount` from the host config
([src/lib.rs:634-641](../../../../src/lib.rs#L634-L641)); we just need
the schema to carry the additional fields so phase 02 has something to
read.

Originating symptom (user transcript):

```text
$ cargo run -- steam login
warning: /home/can/.config/steampipe/host.json uses schemaVersion 1; ...
--login-runners-dir is required when running outside a project root
```

## Out of scope

- Bumping `LATEST_SCHEMA_VERSION` to 3 (decision: stay v2 with
  additive optional fields).
- Emitting the new fields from the NixOS module (that's phase 03).
- Wiring the new fields into resolver logic (that's phase 02).
- Updating any README quick-start or wrapper script
  (`nix/lib/test-cluster.nix`) — phases 05 and 06.
- Adding new CLI flags. Existing `--ssh-key`, `--vm-count`, and
  `--login-runners-dir` are reused as overrides; this phase doesn't
  touch the CLI parser.

## Plan

1. Extend `NixosModuleConfig` in [src/core/nixos_module.rs](../../../../src/core/nixos_module.rs#L51-L69):

   ```rust
   #[serde(rename = "loginRunnersDir")]
   pub login_runners_dir: Option<PathBuf>,
   #[serde(rename = "sshKey")]
   pub ssh_key: Option<PathBuf>,
   #[serde(rename = "vmUser")]
   pub vm_user: Option<String>,
   ```

   Keep them after the existing fields. All three must be `Option<…>`
   so existing `host.json` files (which lack these keys) continue to
   parse.

2. Extend the two parser-roundtrip tests in the same file
   ([src/core/nixos_module.rs:286-335](../../../../src/core/nixos_module.rs#L286-L335)):

   - `load_from_parses_minimal_fixtures_for_supported_schema_versions`:
     after the existing assertions, assert that the three new fields
     parse as `None` when absent.
   - `load_from_parses_full_fixtures_for_supported_schema_versions`:
     extend the fixture JSON to include `loginRunnersDir`, `sshKey`,
     and `vmUser`. Assert they parse to the expected `Some(...)` values
     for **both** `schemaVersion: 1` and `schemaVersion: 2`. (Optional
     fields are version-agnostic; both readers must accept them.)

3. Add one new test asserting that an unrecognized extra field in the
   JSON does not break parsing (forward-compat sanity). Serde's
   default is to ignore unknown fields, so this is a one-liner; do it
   anyway so the contract is explicit.

4. Update [docs/src/configuration/host-config.md](../../../../docs/src/configuration/host-config.md):

   - In the example JSON block under `## Schema` (lines 19-35), add
     the three new keys with example values:

     ```json
     "loginRunnersDir": "/run/current-system/sw/share/steampipe/login-runners",
     "sshKey": "/etc/steampipe/ssh/cluster_key",
     "vmUser": "cluster",
     ```

   - In the field reference list (lines 37-55), add three entries:

     - `loginRunnersDir` (`path string | null`): optional host-level
       directory containing per-VM `microvm-run` wrappers for the
       interactive Steam login wizard. When present, `cluster-ctl
       steam login` does not require `--login-runners-dir` outside a
       project root. Populated by the NixOS module when
       `services.steampipe-cluster.loginRunners.enable = true`.
     - `sshKey` (`path string | null`): optional path to the SSH
       **private** key used to enter cluster VMs. When present,
       `cluster-ctl` uses it as the default `--ssh-key` outside a
       project root.
     - `vmUser` (`string | null`): optional in-guest username that
       owns Steam state and runs the game binary. Defaults to
       `"cluster"`.

   - Note that none of the new fields require a schemaVersion bump;
     they are additive and ignored by older readers.

5. Run the parser test suite:

   ```sh
   cargo test --package steampipe --lib core::nixos_module
   ```

   All existing tests must still pass; the two extended fixtures must
   exercise both schema versions; the new forward-compat test must
   pass.

## Acceptance criteria

- [ ] `cargo test --package steampipe --lib core::nixos_module` passes with the three new fields added to `NixosModuleConfig` as `Option<…>`.
- [ ] Both `load_from_parses_minimal_fixtures_for_supported_schema_versions` and `load_from_parses_full_fixtures_for_supported_schema_versions` exercise the new fields for both `schemaVersion: 1` and `schemaVersion: 2`.
- [ ] A new test (e.g., `load_from_ignores_unknown_fields`) parses a fixture containing a bogus extra key like `"futureField": "x"` and produces `Ok(Some(...))`.
- [ ] `cargo run -- steam info` still parses the current production `/etc/steampipe/module.json` (schemaVersion 1, no new fields) without error or panic.
- [ ] `docs/src/configuration/host-config.md` lists the three new fields in both the example JSON and the field reference, calls out that they are additive and require no schema bump, and is mdBook-renderable (`mdbook build` is clean if mdBook is installed).
- [ ] `grep -n loginRunnersDir src/core/nixos_module.rs` returns at least three hits (struct field, two test fixtures).

## Files likely touched

- [src/core/nixos_module.rs](../../../../src/core/nixos_module.rs) — struct + tests.
- [docs/src/configuration/host-config.md](../../../../docs/src/configuration/host-config.md) — schema docs.

## Pitfalls

- **Forgetting `Option<…>`.** If any new field is non-optional, every
  existing `host.json` on disk (including atlas's) breaks immediately.
  Verify by reading `/etc/steampipe/module.json` on a NixOS host (or a
  unit-test fixture without the keys) after the change.
- **Serde rename collision.** The struct uses
  `#[serde(rename = "schemaVersion")]` style for camelCase keys. Match
  that exactly — `loginRunnersDir`, not `login_runners_dir`, in the
  JSON. The Rust field name stays `snake_case`.
- **Test fixtures are inline string literals.** Look at
  [src/core/nixos_module.rs:288-289](../../../../src/core/nixos_module.rs#L288-L289)
  — the JSON is embedded with `{schema_version}` interpolation. When
  extending, keep the keys alphabetically sorted (the existing
  fixtures do this) so future diffs stay clean.
- **Schema version warning.** The reader currently warns on
  `schemaVersion: 1` because `LATEST_SCHEMA_VERSION = 2`. Do **not**
  silence that warning, and do **not** bump `LATEST_SCHEMA_VERSION` to
  3 — additive fields don't justify a version bump.
- **`#[allow(dead_code)]` on the struct.** The struct already carries
  this attribute ([src/core/nixos_module.rs:50](../../../../src/core/nixos_module.rs#L50)).
  New fields will trigger no warnings as long as they're inside the
  same struct definition; resist the urge to add per-field allow
  attributes.

## Reference

- Dossier: [../cli-configless-from-nixos-research.md](../cli-configless-from-nixos-research.md) — see "Current Reality > What the NixOS module emits today" and "Open Decisions For The User > schema bump".
- Reader implementation: [src/core/nixos_module.rs](../../../../src/core/nixos_module.rs).
- Existing schema doc: [docs/src/configuration/host-config.md](../../../../docs/src/configuration/host-config.md).
- Phase 02 (next consumer of these fields): [02-cli-resolver-refactor.md](./02-cli-resolver-refactor.md).
- Phase 03 (NixOS-side producer): [03-nixos-module-login-runners.md](./03-nixos-module-login-runners.md).
