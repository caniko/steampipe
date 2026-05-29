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

- [Headless Steam login](./planning/headless-steam-login/README.md)
  - [Phase 01 — Virtiofsd lifecycle](./planning/headless-steam-login/01-virtiofsd-lifecycle.md)
  - [Phase 02 — Parallel login + reuse](./planning/headless-steam-login/02-parallel-login-and-reuse.md)
  - [Phase 03 — Docs refresh](./planning/headless-steam-login/03-docs-refresh.md)
- [Headless Steam login — research](./planning/headless-steam-login-research.md)
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
- [Guard dialog exit-101 — investigation](./planning/guard-dialog-101-investigation/README.md)
  - [Phase 01 — Ground-truth capture](./planning/guard-dialog-101-investigation/01-ground-truth-capture.md)
  - [Phase 02 — GPU runtime stack](./planning/guard-dialog-101-investigation/02-gpu-runtime-stack.md)
  - [Phase 03 — Env propagation](./planning/guard-dialog-101-investigation/03-env-propagation.md)
  - [Phase 04 — iced fallback & build](./planning/guard-dialog-101-investigation/04-iced-fallback-build.md)
  - [Phase 05 — Synthesis & fix](./planning/guard-dialog-101-investigation/05-synthesis-and-fix.md)
