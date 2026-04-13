+++
title = "steampipe"
sort_by = "weight"

[extra]
lead = 'A NixOS microVM cluster orchestrator for automated multiplayer game testing. Manage fleets of lightweight VMs, handle Steam authentication, deploy game binaries, and run coordinated end-to-end test sessions.'

url = "/docs/getting-started/installation/"
url_button = "Get started"

repo_version = "v0.1.0"
repo_license = "Open-source AGPL-3.0."
repo_url = "https://codeberg.org/caniko/steampipe"

[[extra.list]]
title = "MicroVM orchestration"
content = "Spawn, monitor, and tear down NixOS microVMs on a single host. Parallel operations across all VMs with automatic SSH readiness detection."

[[extra.list]]
title = "Steam authentication"
content = "Interactive VNC-based login or automated credential-file login with Steam Guard support. Persistent session snapshots to skip re-authentication."

[[extra.list]]
title = "E2E test harness"
content = "Automated build → deploy → run → monitor → report cycle. Heartbeat-based stall detection, failure capture, and historical result tracking."

[[extra.list]]
title = "Network simulation"
content = "Inject latency, jitter, packet loss, and bandwidth limits on VM TAP devices via tc netem. Test your netcode under real-world conditions."

[[extra.list]]
title = "Live monitoring"
content = "Full-screen TUI dashboard with real-time VM status. Color-coded log streaming from all VMs simultaneously."

[[extra.list]]
title = "Nix-native deployment"
content = "Flake-based VM images, reproducible builds, and rsync-based binary deployment with SHA-256 verification."

[[extra.list]]
title = "Multi-cluster support"
content = "Run independent clusters simultaneously with isolated state directories. Test different branches or configurations in parallel."
+++
