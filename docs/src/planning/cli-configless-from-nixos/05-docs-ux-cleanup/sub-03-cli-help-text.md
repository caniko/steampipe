# Sub-layer 05.03 — CLI help text

> **Recommended Codex model: GPT 5.5 low**
>
> A handful of doc-comment edits on existing `#[arg(...)]` and
> `Subcommand` variants in `src/ui/cli.rs`. No structural changes; no
> clap version bump. Touches strings, not types.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

`cluster-ctl steam login --help`, `cluster-ctl steam check --help`,
and the global `--login-runners-dir` / `--runners-dir` doc comments
accurately reflect that these flags are *optional when a NixOS host
config supplies the path*. The help text names the NixOS option name
explicitly.

## Why this matters now

Today [src/ui/cli.rs:562-564](../../../../../src/ui/cli.rs#L562-L564)
says:

```rust
/// Path to directory containing 4GB login VM runners (required for microvm backend)
#[arg(long)]
login_runners_dir: Option<PathBuf>,
```

After phases 01-03, this is no longer "required" — the NixOS host
config can supply it. Help-text mismatch confuses users who
`--help` and assume the flag is mandatory.

## Out of scope

- Behavioral changes to the flags (handled in phase 02).
- README / mdBook edits (sub-layers 01 and 02).
- Adding new flags.

## Plan

1. **Edit the `SteamAction::Login` doc comment for `login_runners_dir`** at
   [src/ui/cli.rs:562-564](../../../../../src/ui/cli.rs#L562-L564):

   ```rust
   /// Path to directory containing 4 GB login VM runners. Optional when
   /// the NixOS module sets `services.steampipe-cluster.loginRunners.enable
   /// = true` (the path is then read from /etc/steampipe/module.json).
   /// Required outside a project root when no NixOS host config supplies
   /// it.
   #[arg(long)]
   login_runners_dir: Option<PathBuf>,
   ```

2. **Edit the `SteamAction::Check` doc comment for `runners_dir`** at
   [src/ui/cli.rs:580-582](../../../../../src/ui/cli.rs#L580-L582)
   with the same shape (referring to the corresponding NixOS option
   if phase 03 added one; otherwise refer to `--runners-dir`'s usual
   sources).

3. **Edit the `SteamAction::Warm`
   [src/ui/cli.rs:595-597](../../../../../src/ui/cli.rs#L595-L597)
   `runners_dir` doc comment** — same pattern. (Phase 02 likely
   made this field `Option<PathBuf>`; if not, this sub-layer
   coordinates with phase 02.)

4. **Edit `SteamAction::Login`'s top-level doc comment** at
   [src/ui/cli.rs:555](../../../../../src/ui/cli.rs#L555):

   ```rust
   /// Interactive Steam login wizard (one VM at a time with VNC).
   /// On NixOS hosts with services.steampipe-cluster.loginRunners.enable
   /// = true, runs without flags; otherwise pass --login-runners-dir.
   Login {
       /// Target VM: "all" (default), "3", or "vm-3". Omit to log into all VMs.
       target: Option<String>,
       ...
   ```

5. **Update the global `--vm-count` doc comment** at
   [src/ui/cli.rs:116-118](../../../../../src/ui/cli.rs#L116-L118):

   ```rust
   /// Total number of VMs in the cluster. Read from the discovered NixOS
   /// host config when omitted; required only when no host config exists.
   #[arg(long, global = true)]
   pub vm_count: Option<u8>,
   ```

6. **Regenerate completions if the project ships them.** Check
   `src/ui/cli.rs` for any `print_completions` invocation
   ([src/lib.rs:501](../../../../../src/lib.rs#L501) wires it).
   Completions derive from clap automatically, so no separate
   regen needed unless a checked-in completion file exists.

7. **Build and inspect the help output.**

   ```sh
   cargo build
   ./target/debug/cluster-ctl steam login --help
   ./target/debug/cluster-ctl steam check --help
   ./target/debug/cluster-ctl --help
   ```

   Confirm:
   - `steam login --help` no longer says "(required for microvm backend)" alone.
   - `--vm-count`'s description mentions the NixOS host config fall-through.
   - Every flag with a NixOS option counterpart names that option in its description.

## Acceptance criteria

- [ ] `cluster-ctl steam login --help` mentions `services.steampipe-cluster.loginRunners.enable` and does **not** describe `--login-runners-dir` as unconditionally required.
- [ ] `cluster-ctl --help` (global) describes `--vm-count` as "read from host config when omitted".
- [ ] `cluster-ctl steam check --help` and `cluster-ctl steam warm --help` reflect the new resolver behavior consistently with phase 02's wiring.
- [ ] `cargo build` and `cargo test` both pass; no behavioral test regresses on the help-text rewrite (clap doc comments don't affect behavior, so this is a sanity check).
- [ ] `grep -n 'required for the microvm backend' src/ui/cli.rs` returns no hits (the old phrasing is fully replaced).

## Files likely touched

- [src/ui/cli.rs](../../../../../src/ui/cli.rs) — only doc-comment
  (`///`) edits on existing fields and the global `--vm-count` flag.

## Pitfalls

- **Don't change `#[arg(...)]` attributes.** Only doc comments. If
  the agent finds itself adding `#[arg(default_value = "...")]` or
  changing `Option<PathBuf>` to `PathBuf`, stop — that's a phase-02
  concern, not phase-05.
- **Mention the option name only if phase 03 used it.** If phase 03
  named the option differently (e.g.,
  `services.steampipe-cluster.login-runners.enable` with hyphens),
  match phase 03 exactly. Check the phase-03 commit before writing
  the doc comments.
- **Don't introduce new help-text for fields that didn't change.**
  Resist the urge to "improve" every doc comment in the file.
- **Verify that `cargo run -- steam login --help` reflects the new
  text after build.** Stale `target/` builds can mask doc-comment
  edits. `cargo clean` if in doubt (overkill but cheap).

## Reference

- Phase 05 README: [./README.md](./README.md).
- Phase 02 (defines the resolver behavior this help text describes): [../02-cli-resolver-refactor.md](../02-cli-resolver-refactor.md).
- Phase 03 (defines the NixOS option name this help text references): [../03-nixos-module-login-runners.md](../03-nixos-module-login-runners.md).
- clap doc-comment conventions: <https://docs.rs/clap/latest/clap/_derive/index.html>.
