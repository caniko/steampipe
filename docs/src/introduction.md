# steampipe

`steampipe` is a NixOS microVM cluster orchestrator for automated multiplayer
game testing. The project ships the `cluster-ctl` CLI for provisioning runner
VMs, managing Steam logins, deploying game builds, and coordinating end-to-end
sessions from a single host.

## Highlights

- Manage fleets of lightweight NixOS microVMs on one host.
- Reuse Steam sessions with interactive or credential-driven login flows.
- Build, deploy, run, monitor, and report multiplayer test sessions.
- Inject latency, jitter, loss, and rate limits with `tc netem`.
- Track historical results and inspect failures from the TUI or CLI tools.

## Project links

- Source repository: <https://codeberg.org/caniko/steampipe>
- Landing page: <https://caniko.codeberg.page/steampipe>

## Documentation map

- [Installation](./getting-started/installation.md)
- [Quick Start](./getting-started/quick-start.md)
- [steampipe.toml](./configuration/steampipe-toml.md)
- [Steam Credentials](./configuration/steam-credentials.md)
- [VM Lifecycle](./configuration/vm-lifecycle.md)
- [E2E Tests](./testing/e2e-tests.md)
- [Test History](./testing/history.md)
- [Cluster Network](./networking/cluster-network.md)
- [Network Simulation](./networking/network-simulation.md)
