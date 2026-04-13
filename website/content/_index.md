+++
title = "steampipe"
description = "NixOS microVM cluster orchestrator for automated multiplayer game testing"

[extra]
tagline = "Automated multiplayer game testing on NixOS"
subtitle = "Orchestrate fleets of microVMs, manage Steam sessions, deploy game binaries, and run coordinated end-to-end tests — all from a single CLI."

[[extra.features]]
title = "MicroVM orchestration"
description = "Spawn, monitor, and tear down NixOS microVMs on a single host. Parallel SSH operations with automatic readiness detection and exponential backoff."

[[extra.features]]
title = "Steam authentication"
description = "Interactive VNC-based login or automated credential-file login with Steam Guard support. Snapshot sessions to skip re-authentication."

[[extra.features]]
title = "E2E test harness"
description = "Automated build → deploy → run → monitor → report cycle with heartbeat-based stall detection, failure capture, and historical result tracking."

[[extra.features]]
title = "Network simulation"
description = "Inject latency, jitter, packet loss, and bandwidth limits on VM TAP devices via tc netem. Test netcode under real-world conditions."

[[extra.features]]
title = "Live monitoring"
description = "Full-screen TUI dashboard with real-time VM status. Color-coded log streaming from all VMs simultaneously."

[[extra.features]]
title = "Nix-native deployment"
description = "Flake-based VM images, reproducible builds, and rsync-based binary deployment with SHA-256 checksum verification."

[[extra.features]]
title = "Multi-cluster support"
description = "Run independent clusters simultaneously with isolated state directories. Test different branches or configurations in parallel."
+++
