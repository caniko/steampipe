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

# Networking

- [Cluster Network](./networking/cluster-network.md)
- [Network Simulation](./networking/network-simulation.md)

# Plan: login-runner-ssh-fix

- [Overview](./planning/login-runner-ssh-fix/README.md)
- [01 — canix explicit key unblock](./planning/login-runner-ssh-fix/01-canix-explicit-key-unblock.md)
- [02 — drop pathExists default and assert](./planning/login-runner-ssh-fix/02-drop-pathexists-default-and-assert.md)
- [03 — flake check empty-key regression](./planning/login-runner-ssh-fix/03-flake-check-empty-key-regression.md)
- [04 — vm.log tail unavailable fix](./planning/login-runner-ssh-fix/04-vm-log-tail-unavailable-fix.md)
- [05 — cluster-key hostname cosmetic](./planning/login-runner-ssh-fix/05-cluster-key-hostname-cosmetic.md)
- [Findings dossier](./planning/login-runner-ssh-unreachable-findings.md)

# Research

- [Hypervisor + compositor research](./planning/hypervisor-restructure-research.md)
- [Hypervisor + compositor design](./planning/hypervisor-restructure-design.md)

# Plan: hypervisor-restructure

- [Overview](./planning/hypervisor-restructure/README.md)
- [01 — validate QEMU virgl on atlas](./planning/hypervisor-restructure/01-validate-qemu-virgl-on-atlas.md)
- [02 — add cloud-hypervisor headless](./planning/hypervisor-restructure/02-add-cloud-hypervisor-headless.md)
- [03 — shrink login home image + reset](./planning/hypervisor-restructure/03-shrink-login-home-image.md)
- [04 — empty-stderr diagnostic](./planning/hypervisor-restructure/04-empty-stderr-diagnostic.md)
- [05 — document hyprland-no](./planning/hypervisor-restructure/05-document-hyprland-no.md)
- [06 — memory-admission gate](./planning/hypervisor-restructure/06-memory-admission-gate.md)
- [07 — canix rebuild + KSM](./planning/hypervisor-restructure/07-canix-rebuild-and-ksm.md)
- [08 — flip login default to QEMU](./planning/hypervisor-restructure/08-flip-login-default-to-qemu.md)
- [09 — memory budgets](./planning/hypervisor-restructure/09-memory-budgets.md)
- [10 — drop CHV-graphics override + overlay marker](./planning/hypervisor-restructure/10-drop-crosvm-overlays.md)
- [11 — document graphics-and-games](./planning/hypervisor-restructure/11-document-graphics-and-games.md)
