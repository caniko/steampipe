# Stabilization And Hardening Research Dossier

## Goal And Trigger

User asked for an evidence-backed assessment of `steampipe` (the
`cluster-ctl` NixOS microVM cluster orchestrator) to identify
stabilizations and hardenings. The repo just retired most of the
`hypervisor-restructure` and `login-runner-ssh-fix` plan sets, has an
in-flight `steam login` fill-the-gaps change on `trunk`, and is the
first long-lived feature crate to graduate from chessbender-internal
tooling toward general-purpose use. Now is the right moment to take
stock of where load-bearing code can shed risk before the next major
addition.

This is a *research dossier*, not an implementation plan. Candidate
next steps are listed but not phased.

## Current Reality

- **Workspace**: single-crate Cargo package `steampipe` 0.1.0, edition
  2024, `rust-version = "1.87"`. Library exposes `pub mod core | game
  | harness | mcp | net | ui | vm | fixture` and one binary
  `cluster-ctl`. Repo root `[Cargo.toml](../../../Cargo.toml)`.
- **Size**: 26,107 LOC of Rust across 50 files. Hot spots dominate
  the codebase:
  - [src/core/config.rs](../../../src/core/config.rs) — 2,925 lines
  - [src/harness/runner.rs](../../../src/harness/runner.rs) — 2,423
  - [src/game/steam.rs](../../../src/game/steam.rs) — 2,279
  - [src/ui/cli.rs](../../../src/ui/cli.rs) — 2,255
  - [src/lib.rs](../../../src/lib.rs) — 1,643
  - [src/vm/preflight.rs](../../../src/vm/preflight.rs) — 1,296
  - [src/game/capture.rs](../../../src/game/capture.rs) — 1,188
  - [src/harness/history.rs](../../../src/harness/history.rs) — 930
  - [src/mcp.rs](../../../src/mcp.rs) — 814
- **Health right now is good.**
  - `cargo clippy --no-deps --all-targets` is clean (0 warnings).
  - `cargo test --lib` passes 441/441.
  - `nix flake check` is green; all NixOS module / warm-timer /
    hypervisor-matrix evals pass.
  - `cargo fmt --check` is clean.
- **Toolchain pin**: `.cargo/config.toml` declares `[unstable]
  codegen-backend = true` plus `-Zshare-generics -Zthreads=0`. The
  pinned channel is `nightly` (rs-harbor `mkCargoConfig { channel =
  "nightly"; }` at [flake.nix:36](../../../flake.nix#L36)). `cargo
  --version` inside the devShell is `1.97.0-nightly`. So
  `rust-version = "1.87"` in `Cargo.toml` is aspirational only —
  building outside the flake's nightly devshell will fail.
- **CI**: none. No `.github/`, `.forgejo/`, `.gitea/`, `*.yml`, or
  `*.yaml` workflow files exist. All gating lives in `nix flake
  check` (Nix-side evals only) and the local devshell.
- **Security tooling**: no `cargo-audit`, no `cargo-deny`, no
  `cargo-vet` in the devshell or anywhere in the flake. RUSTSEC
  advisories on dependencies are not surfaced.
- **Logging**: `tracing` is a dependency and is `use`d in 3 source
  sites (`tracing::warn!` in [src/core/admission.rs:59,
  72](../../../src/core/admission.rs#L59) and one other). But
  **no subscriber is ever installed** in the binary
  [src/bin/cluster-ctl.rs](../../../src/bin/cluster-ctl.rs) (11
  lines: `Cli::parse` → `steampipe::run` → `std::process::exit`).
  Every `tracing::warn!` and `tracing::info!` is silently dropped.
  `tracing-subscriber = { ... features = ["fmt"] }` ships unused.
- **In-flight work** (unstaged): `steam login` fill-the-gaps —
  - `[arg(long)] force: bool` added in [src/ui/cli.rs:628-631](../../../src/ui/cli.rs#L628)
  - `steam-credentials.md` docs updated for the new default
  - `src/game/accounts.rs` gains 174 lines (session-status helper
    extracted, as recommended in
    [steam-login-fill-gaps-findings.md](./steam-login-fill-gaps-findings.md))
  - `src/game/steam.rs` gains 51 lines (precheck loop integrated)
  - These match the recommendation in the findings doc and the work
    appears to be ~complete and ready to ship.

## Evidence Inventory

| Source | What it proves |
|---|---|
| `cargo clippy --no-deps --all-targets` output | Lint posture clean today. |
| `cargo test --lib` output: `441 passed; 0 failed` | Library tests green. |
| `nix flake check --no-build` output: `all checks passed!` | Nix-side module/warm-timer evals green. |
| `find . -name '*.yml' -o -name '*.yaml'` returns nothing under workspace | No CI workflows committed. |
| `which cargo-audit cargo-deny` in nix devshell: not found | No dep-vuln gating. |
| [src/bin/cluster-ctl.rs](../../../src/bin/cluster-ctl.rs) (full file) | `tracing-subscriber` never initialized. |
| `rg -c "tracing::" src/` → 3 lines | Tracing call sites are sparse; warnings about admission-provider failures and one other site go to /dev/null today. |
| `rg "unwrap\(\)"` count by file, with `^#[cfg(test)]` boundary | Production code uses `anyhow`; nearly all `.unwrap()` is inside `mod tests`. `src/core/config.rs` has 102 `.unwrap()` but all after line 623 (`^#[cfg(test)]`). [src/ui/cli.rs](../../../src/ui/cli.rs) has 1 unwrap, in tests. |
| `rg "\.expect\("` non-test sample | 14 `expect("project root must exist for X")` invocations in [src/lib.rs](../../../src/lib.rs) (lines 589, 603, 649, 658, 992, 1011, 1057, 1221, 1253, 1316, 1374, 1387, 1393, 1399, 1409). All depend on an implicit contract from the match in `run_async` at [src/lib.rs:551](../../../src/lib.rs#L551). |
| `wc -l docs/src/planning/*.md` (via git stash → read) | Existing plan sets are extensive (~6,200 lines across two plans + two findings docs). |
| `git status --short` | Working tree has `docs/src/planning/*` and `docs/migration-baseline/*` staged for deletion — operator is in the middle of retiring shipped plan docs. The planning directory does **not** exist on disk in the current working tree. |
| `git log --oneline -30` head commits | Wave 0–2 of hypervisor-restructure shipped (`015f4d5`, `4ef2de8`); login-runner-ssh-fix retired through commits `b3cd24d`, `294a03d`, `27f773a`, `108b8a2`. |
| [Cargo.toml](../../../Cargo.toml) `[dependencies]` | 21 direct deps; `image-compare = "=0.4.2"` is the only exact pin. `memory-admission = "*"` is wildcard. `tokio = "1"` w/ broad features. `rmcp = "0.17"` (server, transport-io). |
| [Cargo.toml](../../../Cargo.toml) `[lints.rust] unsafe_code = "deny"` | No `unsafe` in workspace code. |
| [src/vm/lease.rs](../../../src/vm/lease.rs) header doc + impl | Atomic claim writes + PID-cmdline identity verification already in place (`tempfile + persist`, startup-grace window, audit eprintln). Recent hardening. |
| [src/core/credentials.rs](../../../src/core/credentials.rs) (full file) | Plaintext `steam_pass: String` in `VmCredentials`; no `Zeroize`; no `Drop`-on-scope wipe. Decryption returns `String`. |
| [src/game/steam.rs:1599-1612](../../../src/game/steam.rs#L1599) | `redact_sensitive` and `shell_escape` are best-effort string replacements (POSIX single-quote escaping). Used for both redaction in command output and quoting in command construction. Single-quote-only — fine for `'…'` wrappers but the same function is reused in two distinct roles. |
| [src/core/backend.rs:144,149,434,497](../../../src/core/backend.rs#L144) ; [src/game/capture.rs:783](../../../src/game/capture.rs#L783) ; [src/ui/hooks.rs:45](../../../src/ui/hooks.rs#L45) | Multiple `sh -c <string>` exec sites. SSH layer at [src/core/ssh.rs](../../../src/core/ssh.rs) also passes a single command argument; safety depends on each caller's quoting discipline. |
| [src/mcp.rs](../../../src/mcp.rs) lines 1, 302; `rmcp 0.17` `tool_router` | The MCP server exposes a tool surface to an external client (Claude / LLM). Inputs land in `ClusterParams { cluster, vm_count }` and similar structs and reach `nix` and other subprocesses. No `tracing-subscriber` means MCP server tool calls produce no host-side audit log. |
| [src/core/admission.rs](../../../src/core/admission.rs) | `OnceLock`-cached global `AdmissionGate`. Failure path is "log and disengage throttling" — defense in depth, not a hard dependency. |
| [tests/cli_steam_standalone.rs](../../../tests/cli_steam_standalone.rs) | Only one integration test file (526 lines). Uses `serial_test` + `STEAMPIPE_ETC_DIR` env-var sandboxing. Comprehensive for CLI surface but only one binary integration suite. |
| [website/](../../../website/) directory exists | Zola site sources committed; no Pages workflow exists. mdBook docs likewise have no publish workflow. |
| `git log -1 --pretty=%s -- docs/src/planning/hypervisor-restructure/README.md` ancestry vs. `git status` | Planning docs are not deleted in HEAD — the deletion is in the operator's working tree, pending. Suggests retire-in-progress, not retired. |

## Existing Plan Status

The repo had three plan sets in HEAD; their state vs. the unstaged
delete-everything-planning working tree:

| Plan set | HEAD state | Working-tree state | Audit verdict |
|---|---|---|---|
| `login-runner-ssh-fix/` (5 phases + findings) | Present in HEAD. | Deleted (`D` in git status). | **Shipped.** Commits `b3cd24d`, `294a03d`, `27f773a`, `108b8a2` cover P2-P5. P1 (canix-side) is recorded in `108b8a2` as "fill-the-gaps findings"; the canix rebuild lives outside this repo. Retire is appropriate. |
| `hypervisor-restructure/` (11 phases + design + research) | Present in HEAD. | Deleted in working tree (subdirectory). The top-level `hypervisor-restructure-design.md` and `-research.md` are also deleted; `-results.md` is *added* (untracked) to capture wave outcomes. | **Mostly shipped.** Wave 0/1/2 phases landed via `015f4d5`. Phase 10 (drop `cloud-hypervisor-graphics` override) landed via `4ef2de8`. The `crosvm` overlay block is intentionally retained as a deprecated escape hatch per design D3. Retire is appropriate; the un-tracked `-results.md` carries the durable evidence forward. |
| `steam-login-fill-gaps-findings.md` (single findings doc) | Present in HEAD. | Deleted in working tree. | **In-flight.** The recommended fix in section "Recommended Fix" (force flag, classify-and-skip via `accounts::read_account_info`, summary line, docs update, parse-test extension) is partially landed in unstaged worktree (force flag wired in `[src/ui/cli.rs:628-631](../../../src/ui/cli.rs#L628)`, `force_flag_*` tests added, `accounts.rs` and `steam.rs` extended). Open Decisions section ("Does STALE re-login by default?" and "Does explicit single target imply intent?") may or may not be resolved — worth confirming when the worktree is committed. **Do not delete the findings doc until the work is committed and the open decisions are answered in the commit message or in `steam-credentials.md`.** |

The `docs/migration-baseline/` directory is being deleted as part of
the same cleanup — its content is build-memory and migration evidence
that was relevant during the earlier crosvm→QEMU exploration and is
now historical. Retire is appropriate.

## Work That Should Survive

These are the unfinished, still-relevant work items that the
retiring plan docs leave behind:

1. **Decision D3 cutoff for `crosvm`**. The hypervisor design pins
   the deprecation warning to fire today but commits to *removal in
   0.4*. There is no tracking note for that cutoff anywhere in the
   non-planning docs.
2. **Phase 10 partial completion**. The `cloud-hypervisor-graphics`
   override is deleted, but the `crosvm` overlay block in
   `nix/flake/overlay.nix` is intentionally retained. The schedule
   for re-evaluation is "2026-Q4 nixpkgs (post QEMU 11.x / Mesa
   27.x)". This is a calendar-anchored decision that nothing else
   in the repo will surface when the time arrives.
3. **Open decisions in `steam-login-fill-gaps-findings.md`**:
   - Does `STALE` get a fresh login by default, or is `STALE`
     treated as good-enough until `EXPIRED`?
   - Does explicit single-target (`steam login vm-3`) skip-if-OK
     still apply, or does naming a VM imply intent to log in?
4. **Cosmetic hostname suffix on the host-generated `cluster_key`
   pubkey** (from `login-runner-ssh-fix/05-cluster-key-hostname-cosmetic.md`).
   `git log` shows commit `31444bf module: bake host name into
   login-runner key comment` — looks shipped. Verify before
   retiring the phase doc.
5. **`vm.log` tail bug** (from `login-runner-ssh-fix/04-vm-log-tail-unavailable-fix.md`).
   `git log` shows `27f773a cluster-ctl: print VM log tail tolerantly`
   — looks shipped. Verify before retiring.

## Blockers And Missing Artifacts

None blocking this dossier. No foundational input is missing.

## Risks And Constraints

The following items, in priority order, are the load-bearing
stabilization/hardening surfaces. Each is anchored in evidence above
and has a concrete remediation that this dossier does *not* phase
out.

### S1 — Tracing subscriber is never installed (high impact, low effort)

[src/bin/cluster-ctl.rs](../../../src/bin/cluster-ctl.rs) does not
initialise a `tracing-subscriber`. Every `tracing::warn!` in
production code — including admission-gate provider failures
([src/core/admission.rs:59,72](../../../src/core/admission.rs#L59)) —
goes to /dev/null. Operators have no way to surface "the memory
admission throttle silently disengaged because /proc/meminfo
returned an error" except by reading the source. `tracing-subscriber`
is already a dependency. One `tracing_subscriber::fmt::init()` (or
an `EnvFilter::from_default_env`-driven variant) in `main` makes
every existing call site observable. Add a `RUST_LOG`-equivalent env
var and document it in `installation.md`. Same applies to the MCP
binary path — without an installed subscriber, the server side of
LLM tool calls leaves no audit trail.

### S2 — No CI gate (high impact, medium effort)

The repo has zero committed CI workflow files. All gating is local:
`nix flake check`, `cargo test`, `cargo clippy`, `cargo fmt`. This
means any contribution path other than the maintainer's local
devshell is unguarded. Stabilization options:
- Add a Forgejo Actions workflow for the self-hosted atlas runner
  (the skill `forgejo-atlas-ci` exists and is purpose-built for this
  repo's hosting). Minimum gates: `nix flake check`, `cargo clippy
  --no-deps --all-targets -- -D warnings`, `cargo test`, `cargo fmt
  --check`. That matches the local invariants.
- A second light-weight path on Codeberg's shared runners is
  feasible but the nightly toolchain + Nix build requirements make
  the atlas-only path simpler.

### S3 — No supply-chain audit (medium impact, low effort)

No `cargo-audit`, no `cargo-deny`, no `cargo-vet` in the devshell or
flake. The crate has 21 direct deps with one wildcard
(`memory-admission = "*"`) and one exact pin (`image-compare =
"=0.4.2"`). RUSTSEC advisories on indirect deps (image, vnc,
crossterm, tokio, serde, …) are invisible. Add `cargo-audit` to
`nix/flake/dev-shells.nix` and run it as a `nix flake check` step.
`cargo-deny` is heavier but answers wildcard-version and license
policy in one pass. Picking one is enough to start.

### S4 — Wildcard dependency on `memory-admission` (medium impact, low effort)

[Cargo.toml](../../../Cargo.toml) line 27: `memory-admission = { version = "*", default-features = false, features = ["async"] }`.
`Cargo.lock` will pin to whatever resolved at last update, but a
wildcard makes the next `cargo update` semantically open-ended. The
crate is critical: it is the host-side OOM defense. Pin to a
caret-bounded version (e.g. `"0.x"`) and document the upstream
source so future contributors know where the crate lives.

### S5 — `expect("project root must exist for X")` pattern in `lib.rs` (medium impact, low effort)

14 invocations of `expect("project root must exist for X")` in
[src/lib.rs](../../../src/lib.rs) (lines 589, 603, 649, 658, 992,
1011, 1057, 1221, 1253, 1316, 1374, 1387, 1393, 1399, 1409). Each
is reachable only because of an implicit contract enforced by the
match in `run_async` at line 551 that decides when to skip
project-root detection. Adding a new "doesn't need project root"
command requires updating the match arms; missing one detonates a
panic with a non-recoverable error. Refactor to one of:
- A `RequireProjectRoot` enum on the command match and a single
  `ok_or_else(|| anyhow!("..."))` at the call site, so a new arm
  must explicitly declare its policy.
- A helper `project_root_or_bail(cli, command_name) -> Result<PathBuf>`
  that converts the `expect` into a typed `bail!`.

### S6 — `lib.rs` is a god-router (medium impact, high effort)

[src/lib.rs](../../../src/lib.rs) is 1,643 lines with only 23
functions. It dispatches every CLI subcommand in `run_async`. The
in-flight `steam login` work modifies it again. Every subcommand
addition incurs a merge conflict surface in `lib.rs`. Split the
dispatch by subcommand into `src/ui/dispatch/{steam,visual,test,…}.rs`
modules; keep `run_async` as a small router. Defer until the
hypervisor-restructure / steam-login working tree is committed —
this is a structural change and conflicts with active work.

### S7 — `ui/cli.rs` is 2,255 lines (low impact, high effort)

Mostly a single `clap`-derived `Cli` struct + a massive test module
(test_start at line 826, so ~1,400 lines of clap derive surface).
Refactor opportunity: split per subcommand group into submodules of
`ui/cli/`, but the marginal stability gain is small. Lower
priority than S6.

### S8 — Credentials are plaintext `String` in memory (low impact, medium effort)

[src/core/credentials.rs:9-12](../../../src/core/credentials.rs#L9):
`steam_pass: String`. No `Zeroize`, no `Drop` impl. Decrypted age
output is materialised in a `String` and parsed via `toml::from_str`.
The process lifetime is short (CLI invocation), and on Linux core
dumps are not collected by default for cluster-ctl. Risk is real but
small. Adding the `secrecy` crate's `SecretString` (or `zeroize`
manually) and routing all secret reads through it costs ~50 LOC and
is the right shape for the next major version.

### S9 — `shell_escape` is single-quote-only and shared between roles (low impact, low effort)

[src/game/steam.rs:1614](../../../src/game/steam.rs#L1614):
`fn shell_escape(s: &str) -> String { s.replace('\'', "'\\''") }`.
This is correct for use inside `'…'` wrappers and is the function
used to wrap user-supplied Steam credentials when constructing
`steamcmd` bootstrap scripts. The same function is also used as
the second-pass key inside `redact_sensitive` to redact a *shell-
escaped form* of a secret. That double duty works today but the
roles are not independent. Tests at lines 1846-1857 cover the
single-quote and empty cases. Add an assertion that the function is
only ever called inside single-quoted contexts (or rename it
`single_quote_escape` to make the precondition self-documenting).

### S10 — `sh -c <command-string>` is used in 6+ places (low impact, medium effort)

`sh -c` is the chosen exec model in
[src/core/backend.rs](../../../src/core/backend.rs) (local backend,
docker exec), [src/game/capture.rs:783](../../../src/game/capture.rs#L783),
[src/ui/hooks.rs:45](../../../src/ui/hooks.rs#L45),
[src/vm/preflight.rs:146,548](../../../src/vm/preflight.rs#L146).
For backend/docker, the command string is typed-typed input that
flows through `shell_escape` upstream. For
[src/ui/hooks.rs:45](../../../src/ui/hooks.rs#L45), the command is
operator-supplied from config (TOML) — already a trust boundary.
The risk is contained but the pattern means a future contributor
who adds a "format a string then `sh -c` it" path inherits the
same expectations. A documented helper that wraps `Command::new("sh").arg("-c").arg(cmd)`
with a single-line comment about the expected escaping discipline
costs ~10 LOC and gives a grep handle.

### S11 — No CHANGELOG, no SECURITY.md, no CONTRIBUTING.md, no public release process (low impact, low effort)

`README.md` is 1,032 lines and serves as a user manual but there is
no release-note path. The crate is `version = "0.1.0"` and the
design docs target "removal in 0.4" for `crosvm` — implying at least
0.2, 0.3, 0.4 are planned. Add a `CHANGELOG.md` (Keep-a-Changelog
shape), a thin `SECURITY.md` (one paragraph: how to report; what is
in scope — operator handles their own Steam credentials), and a
`CONTRIBUTING.md` that points contributors at the devshell and the
local check sequence. Pick a versioning policy and announce 0.1.x
vs. 0.2.0 in the next release note.

### S12 — `result` symlinks committed (cosmetic, instant)

[result](../../../result) and [result-tournament-uifull-up](../../../result-tournament-uifull-up)
are committed dangling symlinks into `/nix/store/…`. `.gitignore`
doesn't cover them. They mutate on every build. Add `/result` and
`/result-*` to `.gitignore` (matches the existing `tests/fixture/result*`
entry pattern).

### S13 — `image-compare = "=0.4.2"` exact pin (cosmetic, low effort)

Pin is intentional (very likely workaround for upstream API churn).
Add a one-line comment in `Cargo.toml` near the dep noting *why* the
exact pin exists and what the upgrade path is. Otherwise the next
contributor will try to relax it.

### S14 — Single integration-test binary (low impact, medium effort)

`tests/cli_steam_standalone.rs` (526 lines) covers the CLI surface
but is the only integration test. The harness, capture, MCP, and
backend layers are unit-tested only. Adding integration tests for
MCP (one tool call end-to-end via `rmcp` in-process transport) and
for the backend (local mode against `tempdir` work_dir) would catch
regressions that unit tests can't. The fixture-tools / e2e tests
require a real microvm bridge, so they live in `tests/fixture/` and
are run via `just diag-fixture` — they are not normally exercised
in CI.

### S15 — `crosvm` overlay deprecation has no enforcement date (calendar risk)

The design says "Removal scheduled for 0.4." but there is no marker
in the source for when 0.4 ships. The deprecation warning in
[nix/flake/checks.nix](../../../nix/flake/checks.nix) confirms the
warning fires. Add a `// REMOVE-AT-0.4` (or equivalent) breadcrumb
in the overlay block so the cutoff is greppable.

### S16 — Public API not documented (low impact, low effort)

`src/lib.rs` exposes 8 public modules; `Cli` is re-exported and
intended as the library API surface (per commit `1996e06 Expose
steampipe as a library`). There is no rustdoc top-level pass. The
crate is not yet on crates.io. If publishing is on the roadmap, the
`rust-crate-release-prep` skill flow applies: license metadata,
README install snippet, rustdoc on all `pub` items, docs.rs config,
crates.io metadata. The repo is structurally close (LICENSE
present: EUPL-1.2; Cargo.toml has license; `lints.rust unsafe_code =
"deny"`).

## Candidate Next Steps

Listed without sequencing — see Open Decisions section for what the
user needs to choose first.

| ID | Step | Effort | Depends on |
|---|---|---|---|
| N1 | Install `tracing-subscriber` in `cluster-ctl` main; document `RUST_LOG`. | XS | — |
| N2 | Add Forgejo Actions workflow on atlas runner (clippy/test/fmt/flake-check). | S | choose host (atlas vs Codeberg shared) |
| N3 | Add `cargo-audit` step to devshell + `nix flake check`. | XS | — |
| N4 | Pin `memory-admission` to a versioned range. | XS | upstream version available |
| N5 | Replace `expect("project root must exist for X")` with a typed `bail!`. | S | — |
| N6 | Split `lib.rs` dispatch into per-subcommand modules. | M | merge of in-flight steam-login fill-gaps |
| N7 | Adopt `secrecy::SecretString` for `VmCredentials.steam_pass`. | S | — |
| N8 | Rename `shell_escape` → `single_quote_escape`; add doc-test for round-trip. | XS | — |
| N9 | Add CHANGELOG.md, SECURITY.md, CONTRIBUTING.md. | XS | release-process decision |
| N10 | Add `/result` + `/result-*` to `.gitignore`; clean committed symlinks. | XS | — |
| N11 | Comment exact pin reason on `image-compare = "=0.4.2"`. | XS | — |
| N12 | Add MCP integration test using `rmcp` in-process transport. | S | — |
| N13 | Add `// REMOVE-AT-0.4` breadcrumb in `nix/flake/overlay.nix` crosvm block. | XS | — |
| N14 | Public-API rustdoc pass (precondition for crates.io). | M | release goal |

`XS` ≈ <1 hour, `S` ≈ ½ day, `M` ≈ 1–2 days. None require external
coordination.

Natural batches:
- **Batch A (paper-cuts, no risk)**: N1, N3, N4, N8, N10, N11, N13.
  One commit each; all independent.
- **Batch B (typed bail + dispatch split)**: N5 then N6, after the
  in-flight `steam login` change is committed.
- **Batch C (CI + audit gating)**: N2 + the audit step from N3
  enforced as a flake check.
- **Batch D (release readiness)**: N7, N9, N14 — only if publishing
  to crates.io or cutting a tagged 0.2.

## Open Decisions For The User

The following choices change which Batches in `Candidate Next Steps`
apply. Each is a real fork, not a paper-cut.

1. **Where does CI run?** Atlas self-hosted runner (skill
   `forgejo-atlas-ci`) or a Codeberg shared runner path? Atlas is
   simpler given the Nix + nightly requirements; the shared path
   requires either skipping flake-check or installing Nix on the
   shared runner.

2. **Is crates.io publication on the roadmap?** If yes, Batch D is
   load-bearing. If no, N7/N9/N14 drop to "nice to have".

3. **Resolve the two open decisions in
   `steam-login-fill-gaps-findings.md` before retiring it.** The
   in-flight worktree presumably encodes a choice for both
   (STALE-default and explicit-target-default); record that choice in
   the commit message and in
   `docs/src/configuration/steam-credentials.md` so the policy
   survives the doc deletion. Until done, **do not delete the
   findings doc**.

4. **Pick a deprecation cutoff for `crosvm`**. The design says
   "0.4". Decide whether 0.4 is the next-after-next release or
   calendar-anchored. The deprecation-warning text could quote a
   date instead of a version if calendar pressure is the real
   driver.

5. **Tracking note for hypervisor-restructure leftover**. The
   un-tracked
   [hypervisor-restructure-results.md](./hypervisor-restructure-results.md)
   needs a home in `SUMMARY.md` if it is intended to survive (and a
   `Plan retired: <date>` header would help future contributors
   place it). Otherwise either commit it as a brief "retired-plans
   summary" note or fold its durable points into
   `docs/src/configuration/vm-resources.md` and
   `docs/src/configuration/graphics-and-games.md` and discard.
