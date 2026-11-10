# steampipe vision

`steampipe` is a NixOS microVM cluster orchestrator for automated multiplayer
game testing. The `cluster-ctl` CLI runs on one host, boots a fleet of
lightweight NixOS VMs, logs each into Steam, deploys a game build, and drives
coordinated end-to-end sessions with monitoring, failure capture and history.
It grew out of internal tooling for chessbender, and every game-specific value
is configurable through `steampipe.toml`, so other Steam multiplayer games can
adopt it unchanged.

## Problem

Testing a networked game needs several real peers, a real Steam session, and
control over a hostile network. That is normally a rack of machines and a
manual checklist. `steampipe` compresses it to one host: VMs are disposable,
Steam login is reusable, and deployment, launch, screenshots and traffic
shaping are steps of one command instead of a runbook. The hard part is not
starting VMs, it is keeping identity, binaries and network conditions in sync
across all of them without a human watching.

## Principles

- **One host is the cluster.** VMs live on a single Linux host behind a bridge;
  setup is `up`, teardown is `down`, and nothing depends on external
  provisioning.
- **Illegal states are unrepresentable.** `ClusterConfig` is a typestate
  (`Unchecked` then `BridgeReady`) so that `up`, Steam login and checks cannot
  be reached before the bridge is verified, at zero runtime cost.
- **Parallel by default.** Multi-VM operations fan out with `par_each_vm`, one
  task per VM, collecting results rather than serializing SSH calls.
- **Game specifics are configuration.** Binary name, package, steam_api
  library, assets, log file, users and network ranges all come from
  `steampipe.toml`; the code stays generic over Steam games.
- **Failures are artifacts.** Sessions capture logs, screenshots and test
  history so a flake can be inspected after the fact instead of re-run blind.

## Scope

- VM lifecycle: reserve, boot, restart, status, snapshot, teardown.
- Steam: interactive and credential-driven login, session reuse, Guard-code
  handling, per-VM state checks.
- Deployment: build the game, push binary and assets, run it under a managed
  session.
- End-to-end testing: declarative test steps, heartbeat monitoring, failure
  capture, historical results and a TUI plus CLI reporting surface.
- Network: bridge/NAT setup and `tc netem` latency, jitter, loss and rate
  limits, including simulated hostile conditions.
- Diagnostics: fixture-based boot/compositor/GPU/VNC diagnosis and screenshot
  comparison.

## Non-goals

- Not a general-purpose hypervisor manager or cloud orchestrator; it targets
  NixOS microVM runners on one host.
- Not a game server or matchmaker; it launches the game's own binaries.
- Fully unattended Steam login through a stored TOTP shared secret is
  explicitly deferred.
- No MCP tools that initiate or complete a Steam Guard login; headless and
  agent contexts fail fast with a typed "guard code needed" error, and the GUI
  helper is operator-only.

## Direction

The active plan is the Steam login overhaul: replace the two-phase Guard flow
(login bails, operator later runs `steam guard --code`) with one invocation
that detects a missing session, runs `steamcmd`, and relays a Guard code into
the live prompt from a host-side iced dialog, serialized through a host queue.
It also splits the GUI into a separate `cluster-guard-prompt` helper so
`cluster-ctl` stays GUI-free. Alongside it sit stabilization and hardening
research, virtiofsd/supervisord findings, and headless token-mint exploration.
Recent commits are mostly platform: Simit-generated CI, harbor input
migrations, and pin refreshes.

## Place in the fleet

The canix flake consumes the repository as the `steampipe` input and imports
its NixOS module on atlas and nomad, which configure
`services.steampipe-cluster` and its login-runner SSH key.
