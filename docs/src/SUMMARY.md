# Summary

[Introduction](./introduction.md)

# Getting Started

- [Installation](./getting-started/installation.md)
- [Quick Start](./getting-started/quick-start.md)

# Configuration

- [steampipe.toml](./configuration/steampipe-toml.md)
- [Visual Testing](./configuration/visual-testing.md)
- [Graphics and games](./configuration/graphics-and-games.md)
- [Global host config](./configuration/host-config.md)
- [Steam Credentials](./configuration/steam-credentials.md)
- [VM Lifecycle](./configuration/vm-lifecycle.md)
- [VM resources](./configuration/vm-resources.md)

# Testing

- [E2E Tests](./testing/e2e-tests.md)
- [Test History](./testing/history.md)
- [Measuring build memory](./testing/build-memory.md)
- [Fixture diagnostics](./testing/fixture-diagnostics.md)

# Networking

- [Cluster Network](./networking/cluster-network.md)
- [Network Simulation](./networking/network-simulation.md)

# Planning

- [Steam login overhaul (iced + auto-steamcmd) — research](./planning/steam-login-iced-overhaul-research.md)
- [Steam login overhaul — plan](./planning/steam-login-iced-overhaul/README.md)
  - [Phase 01 — Login control-plane refactor](./planning/steam-login-iced-overhaul/01-login-control-plane.md)
  - [Phase 02 — Host context detection](./planning/steam-login-iced-overhaul/02-context-detection.md)
  - [Phase 03 — iced helper binary](./planning/steam-login-iced-overhaul/03-iced-helper-binary.md)
  - [Phase 04 — HelperGui provider wiring](./planning/steam-login-iced-overhaul/04-helper-provider-wiring.md)
  - [Phase 05 — State-sync convergence](./planning/steam-login-iced-overhaul/05-state-sync-convergence.md)
  - [Phase 06 — Headless fail-fast contract](./planning/steam-login-iced-overhaul/06-headless-failfast-contract.md)
  - [Phase 07 — Retire VNC login](./planning/steam-login-iced-overhaul/07-retire-vnc-login.md)
  - [Phase 08 — Docs & tests](./planning/steam-login-iced-overhaul/08-docs-and-tests/README.md)
    - [Sub 01 — Docs rewrite](./planning/steam-login-iced-overhaul/08-docs-and-tests/sub-01-docs-rewrite.md)
    - [Sub 02 — Tests & CLI help](./planning/steam-login-iced-overhaul/08-docs-and-tests/sub-02-tests-and-cli-help.md)
- [Headless Steam token-mint login — plan](./planning/headless-steam-token-mint/README.md)
  - [Phase 01 — Mint feasibility spike](./planning/headless-steam-token-mint/01-mint-feasibility-spike.md)
  - [Phase 02 — Mint engine + GuardProvider](./planning/headless-steam-token-mint/02-mint-engine.md)
  - [Phase 03 — Artifact writer (VDF)](./planning/headless-steam-token-mint/03-artifact-writer.md)
  - [Phase 04 — Wire mint into steam login](./planning/headless-steam-token-mint/04-wire-login-engine.md)
  - [Phase 05 — Retire internals & verify](./planning/headless-steam-token-mint/05-retire-and-verify.md)
