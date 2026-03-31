use std::fmt;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use crate::config::BackendKind;

/// Network transport layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NetworkMode {
    Lan,
    Steam,
}

impl fmt::Display for NetworkMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lan => f.write_str("lan"),
            Self::Steam => f.write_str("steam"),
        }
    }
}

/// VM display mode: controls compositor setup on VMs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DisplayMode {
    /// No compositor — game binary gets `--headless`
    Headless,
    /// Start Weston (headless backend) — game renders to virtual display
    Weston,
    /// Start Sway compositor
    Sway,
}

impl fmt::Display for DisplayMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Headless => f.write_str("headless"),
            Self::Weston => f.write_str("weston"),
            Self::Sway => f.write_str("sway"),
        }
    }
}

#[derive(Parser)]
#[command(
    name = "cluster-ctl",
    about = "VM cluster orchestration for multiplayer game testing"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Project root (auto-detected from git/Cargo.toml workspace)
    #[arg(long, global = true)]
    pub project_root: Option<PathBuf>,

    /// SSH key path (default: <project_root>/nix/test-cluster/cluster_key)
    #[arg(long, global = true)]
    pub ssh_key: Option<PathBuf>,

    /// Total number of VMs in the cluster (required)
    #[arg(long, global = true)]
    pub vm_count: Option<u8>,

    /// Path to TOML file with per-VM Steam credentials and game keys
    #[arg(long, global = true)]
    pub credentials: Option<PathBuf>,

    /// Cluster name (for running multiple independent clusters)
    #[arg(long, global = true)]
    pub cluster: Option<String>,

    /// Override state directory (share cluster state across projects)
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,

    /// Backend: microvm (default), docker, or local
    #[arg(long, global = true, value_enum)]
    pub backend: Option<BackendKind>,

    /// Override lock directory for VM reservation (default: $XDG_RUNTIME_DIR/steampipe/)
    #[arg(long, global = true)]
    pub lock_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Show status of all VMs (running, SSH, Steam, game)
    Status,

    /// Start VMs and hold locks. Uses --vm-count to determine how many.
    Up {
        /// Path to directory containing vm-N/bin/microvm-run runners (required for microvm backend)
        #[arg(long)]
        runners_dir: Option<PathBuf>,

        /// Daemonize: start VMs, then background a lease-holding process and exit
        #[arg(long)]
        detach: bool,
    },

    /// Internal: hold VM leases in foreground (used by `up --detach`)
    #[command(hide = true)]
    HoldLeases {
        /// Path to directory containing vm-N/bin/microvm-run runners (required for microvm backend)
        #[arg(long)]
        runners_dir: Option<PathBuf>,
    },

    /// Stop all VMs
    Down,

    /// Restart VMs (stop then start)
    Restart {
        /// Target VM: "all", "3", "vm-3" (default: all)
        target: Option<String>,
        /// Path to directory containing vm-N/bin/microvm-run runners (required for microvm backend)
        #[arg(long)]
        runners_dir: Option<PathBuf>,
    },

    /// Set up cluster networking (bridge, TAP devices, NAT rules). Requires sudo.
    NetUp {
        /// Path to nft binary
        #[arg(long, default_value = "nft")]
        nft: String,
    },

    /// Tear down cluster networking. Requires sudo.
    NetDown {
        /// Path to nft binary
        #[arg(long, default_value = "nft")]
        nft: String,
    },

    /// Build and deploy binary + assets to all VMs
    Deploy {
        /// Skip cargo build
        #[arg(long)]
        no_build: bool,
        /// Verify deployed files with checksums
        #[arg(long)]
        verify: bool,
    },

    /// Interactive Steam login wizard (one VM at a time with VNC)
    SteamLogin {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
        /// Continue from target through vm-7 (e.g., "3" means vm-3..vm-7)
        #[arg(short = '+', long)]
        continue_from: bool,
        /// Path to directory containing 4GB login VM runners (required for microvm backend)
        #[arg(long)]
        login_runners_dir: Option<PathBuf>,
    },

    /// Check Steam login + health on VMs (boots each VM to verify)
    SteamCheck {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
        /// Path to directory containing VM runners (required for microvm backend)
        #[arg(long)]
        runners_dir: Option<PathBuf>,
    },

    /// Start weston + Steam on VMs
    SteamStart {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
    },

    /// Start weston + Steam + game on all VMs
    Run {
        /// Args passed to the game binary on VMs
        #[arg(trailing_var_arg = true)]
        extra_args: Vec<String>,
    },

    /// Kill game process on all VMs
    StopGame {
        /// Also kill Steam and Weston processes
        #[arg(long)]
        kill_steam: bool,
    },

    /// Collect game.log from all VMs (or stream live with --follow)
    Logs {
        /// Target VM: "all", "3", "vm-3" (default: all)
        target: Option<String>,
        /// Output directory (collect mode: save logs to files)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Stream logs in real-time from all VMs (tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show (default: 20 for --follow, 100 for stdout)
        #[arg(short = 'n', long)]
        lines: Option<u32>,
        /// Read from beginning instead of end
        #[arg(long)]
        head: bool,
        /// Filter output lines by regex pattern
        #[arg(long)]
        pattern: Option<String>,
    },

    /// Run end-to-end tests: deploy -> launch -> monitor -> collect logs -> report
    Test {
        /// Network transport: lan or steam
        #[arg(long, default_value = "lan")]
        network: NetworkMode,

        /// Number of players (2-8; host + N-1 VMs)
        #[arg(long, default_value_t = 8)]
        players: u8,

        /// Args passed to the game binary on each VM
        #[arg(long)]
        vm_args: Option<String>,

        /// Args passed to the local (host) game binary
        #[arg(long)]
        host_args: Option<String>,

        /// Number of test runs
        #[arg(long, default_value_t = 1)]
        max_runs: u32,

        /// Timeout per run in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,

        /// Grace period (seconds) between SIGTERM and SIGKILL
        #[arg(long, default_value_t = 5)]
        shutdown_timeout: u64,

        /// Don't stop after first failure
        #[arg(long)]
        no_stop_on_failure: bool,

        /// Skip deployment before testing
        #[arg(long)]
        no_deploy: bool,

        /// Skip cargo build before deploying
        #[arg(long)]
        no_build: bool,

        /// Regex filter for output lines
        #[arg(long)]
        filter_pattern: Option<String>,

        /// Write full output to this file
        #[arg(long)]
        output_file: Option<PathBuf>,

        /// Capture screenshots on test failure
        #[arg(long)]
        capture_on_failure: bool,

        /// VM display mode: headless, weston (default), or sway
        #[arg(long, default_value = "weston")]
        display: DisplayMode,
    },

    /// Diagnose and repair cluster health issues
    Doctor {
        /// Auto-fix problems where possible
        #[arg(long)]
        fix: bool,
    },

    /// Apply network emulation (latency, packet loss, jitter, bandwidth). Requires sudo.
    Netem {
        /// Target VM: "all", "3", "vm-3"
        target: String,
        /// Latency in milliseconds
        #[arg(long)]
        latency: Option<u32>,
        /// Jitter in milliseconds (used with --latency)
        #[arg(long)]
        jitter: Option<u32>,
        /// Packet loss percentage (0-100)
        #[arg(long)]
        loss: Option<f32>,
        /// Bandwidth limit in kbit/s
        #[arg(long)]
        rate: Option<u32>,
    },

    /// Show current network emulation status
    NetemShow,

    /// Reset (remove) all network emulation rules
    NetemReset {
        /// Target VM: "all", "3", "vm-3" (default: all)
        target: Option<String>,
    },

    /// Show test result history
    History {
        /// Show only the last N sessions
        #[arg(short = 'n', long)]
        last: Option<usize>,
        /// Clear all test history
        #[arg(long)]
        clear: bool,
    },

    /// Save a snapshot of VM state
    SnapshotSave {
        /// Snapshot name
        name: String,
    },

    /// Restore a VM state snapshot
    SnapshotRestore {
        /// Snapshot name
        name: String,
    },

    /// List available snapshots
    SnapshotList,

    /// Delete a snapshot
    SnapshotDelete {
        /// Snapshot name
        name: String,
    },

    /// Capture screenshots from all VMs
    Screenshot {
        /// Output directory (default: screenshots/<timestamp>)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Show Steam account status across all VMs
    Accounts,

    /// Live TUI dashboard showing VM status
    Watch {
        /// Refresh interval in seconds
        #[arg(short, long, default_value_t = 5)]
        interval: u64,
    },

    /// Generate a steampipe.toml config template
    Init,

    /// Generate a .yh.cluster.toml config file for yh-mcp cluster defaults
    YhConfig {
        /// Overwrite existing file
        #[arg(long)]
        force: bool,
    },

    /// Start MCP server (JSON-RPC over stdio)
    Mcp,

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

impl Cli {
    /// Reconstruct global CLI args for re-exec (e.g. spawning the lease daemon).
    pub fn global_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(count) = self.vm_count {
            args.extend(["--vm-count".into(), count.to_string()]);
        }
        if let Some(ref root) = self.project_root {
            args.extend(["--project-root".into(), root.display().to_string()]);
        }
        if let Some(ref key) = self.ssh_key {
            args.extend(["--ssh-key".into(), key.display().to_string()]);
        }
        if let Some(ref creds) = self.credentials {
            args.extend(["--credentials".into(), creds.display().to_string()]);
        }
        if let Some(ref cluster) = self.cluster {
            args.extend(["--cluster".into(), cluster.clone()]);
        }
        if let Some(ref dir) = self.state_dir {
            args.extend(["--state-dir".into(), dir.display().to_string()]);
        }
        if let Some(ref backend) = self.backend {
            args.extend(["--backend".into(), format!("{backend:?}").to_lowercase()]);
        }
        if let Some(ref dir) = self.lock_dir {
            args.extend(["--lock-dir".into(), dir.display().to_string()]);
        }
        args
    }
}

/// Print shell completions to stdout.
pub fn print_completions(shell: Shell) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "cluster-ctl", &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse a command line into Cli, prepending the binary name.
    fn parse(args: &[&str]) -> Cli {
        let mut full = vec!["cluster-ctl"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).expect("CLI parse failed")
    }

    // ── Logs command ─────────────────────────────────────────────────────

    #[test]
    fn logs_defaults() {
        let cli = parse(&["--vm-count", "7", "logs"]);
        match cli.command {
            Commands::Logs {
                target,
                output,
                follow,
                lines,
                head,
                pattern,
            } => {
                assert!(target.is_none());
                assert!(output.is_none());
                assert!(!follow);
                assert!(lines.is_none());
                assert!(!head);
                assert!(pattern.is_none());
            }
            _ => panic!("expected Logs command"),
        }
    }

    #[test]
    fn logs_with_target() {
        let cli = parse(&["--vm-count", "7", "logs", "vm-3"]);
        match cli.command {
            Commands::Logs { target, .. } => {
                assert_eq!(target.as_deref(), Some("vm-3"));
            }
            _ => panic!("expected Logs command"),
        }
    }

    #[test]
    fn logs_with_all_flags() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "logs",
            "--head",
            "--pattern",
            "ERROR|WARN",
            "-n",
            "50",
            "vm-1",
        ]);
        match cli.command {
            Commands::Logs {
                target,
                head,
                pattern,
                lines,
                follow,
                ..
            } => {
                assert_eq!(target.as_deref(), Some("vm-1"));
                assert!(head);
                assert_eq!(pattern.as_deref(), Some("ERROR|WARN"));
                assert_eq!(lines, Some(50));
                assert!(!follow);
            }
            _ => panic!("expected Logs command"),
        }
    }

    #[test]
    fn logs_follow_with_target() {
        let cli = parse(&["--vm-count", "7", "logs", "--follow", "vm-2"]);
        match cli.command {
            Commands::Logs { follow, target, .. } => {
                assert!(follow);
                assert_eq!(target.as_deref(), Some("vm-2"));
            }
            _ => panic!("expected Logs command"),
        }
    }

    // ── StopGame command ─────────────────────────────────────────────────

    #[test]
    fn stop_game_default() {
        let cli = parse(&["--vm-count", "7", "stop-game"]);
        match cli.command {
            Commands::StopGame { kill_steam } => assert!(!kill_steam),
            _ => panic!("expected StopGame"),
        }
    }

    #[test]
    fn stop_game_kill_steam() {
        let cli = parse(&["--vm-count", "7", "stop-game", "--kill-steam"]);
        match cli.command {
            Commands::StopGame { kill_steam } => assert!(kill_steam),
            _ => panic!("expected StopGame"),
        }
    }

    // ── Test command boolean negation ────────────────────────────────────

    #[test]
    fn test_defaults_enable_everything() {
        let cli = parse(&["--vm-count", "7", "test"]);
        match cli.command {
            Commands::Test {
                no_build,
                no_deploy,
                no_stop_on_failure,
                ..
            } => {
                assert!(!no_build, "build should be enabled by default");
                assert!(!no_deploy, "deploy should be enabled by default");
                assert!(
                    !no_stop_on_failure,
                    "stop_on_failure should be enabled by default"
                );
            }
            _ => panic!("expected Test command"),
        }
    }

    #[test]
    fn test_no_build() {
        let cli = parse(&["--vm-count", "7", "test", "--no-build"]);
        match cli.command {
            Commands::Test {
                no_build,
                no_deploy,
                ..
            } => {
                assert!(no_build);
                assert!(!no_deploy);
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_no_deploy() {
        let cli = parse(&["--vm-count", "7", "test", "--no-deploy"]);
        match cli.command {
            Commands::Test {
                no_deploy,
                no_build,
                ..
            } => {
                assert!(no_deploy);
                assert!(!no_build);
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_no_stop_on_failure() {
        let cli = parse(&["--vm-count", "7", "test", "--no-stop-on-failure"]);
        match cli.command {
            Commands::Test {
                no_stop_on_failure, ..
            } => assert!(no_stop_on_failure),
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_all_negated() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "test",
            "--no-build",
            "--no-deploy",
            "--no-stop-on-failure",
        ]);
        match cli.command {
            Commands::Test {
                no_build,
                no_deploy,
                no_stop_on_failure,
                ..
            } => {
                assert!(no_build);
                assert!(no_deploy);
                assert!(no_stop_on_failure);
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_with_all_options() {
        let cli = parse(&[
            "--vm-count",
            "3",
            "--cluster",
            "ci",
            "test",
            "--network",
            "steam",
            "--players",
            "4",
            "--max-runs",
            "10",
            "--timeout",
            "120",
            "--shutdown-timeout",
            "3",
            "--no-build",
            "--filter-pattern",
            "PASS|FAIL",
            "--capture-on-failure",
            "--host-args=--headless",
            "--vm-args=--auto-join",
        ]);
        match cli.command {
            Commands::Test {
                network,
                players,
                max_runs,
                timeout,
                shutdown_timeout,
                no_build,
                no_deploy,
                no_stop_on_failure,
                filter_pattern,
                capture_on_failure,
                host_args,
                vm_args,
                ..
            } => {
                assert!(matches!(network, NetworkMode::Steam));
                assert_eq!(players, 4);
                assert_eq!(max_runs, 10);
                assert_eq!(timeout, 120);
                assert_eq!(shutdown_timeout, 3);
                assert!(no_build);
                assert!(!no_deploy);
                assert!(!no_stop_on_failure);
                assert_eq!(filter_pattern.as_deref(), Some("PASS|FAIL"));
                assert!(capture_on_failure);
                assert_eq!(
                    host_args.as_deref(),
                    Some("--headless"),
                    "host_args should pass through verbatim"
                );
                assert_eq!(vm_args.as_deref(), Some("--auto-join"));
            }
            _ => panic!("expected Test"),
        }
    }

    // ── Global args roundtrip ────────────────────────────────────────────

    #[test]
    fn global_args_roundtrip() {
        let cli = parse(&["--vm-count", "3", "--cluster", "mytest", "status"]);
        let args = cli.global_args();
        assert!(args.contains(&"--vm-count".to_string()));
        assert!(args.contains(&"3".to_string()));
        assert!(args.contains(&"--cluster".to_string()));
        assert!(args.contains(&"mytest".to_string()));
    }

    #[test]
    fn global_args_minimal() {
        let cli = parse(&["--vm-count", "1", "status"]);
        let args = cli.global_args();
        assert!(args.contains(&"--vm-count".to_string()));
        assert!(!args.contains(&"--cluster".to_string()));
    }

    // ── Deploy ───────────────────────────────────────────────────────────

    #[test]
    fn deploy_defaults() {
        let cli = parse(&["--vm-count", "7", "deploy"]);
        match cli.command {
            Commands::Deploy { no_build, verify } => {
                assert!(!no_build);
                assert!(!verify);
            }
            _ => panic!("expected Deploy"),
        }
    }

    #[test]
    fn deploy_no_build_verify() {
        let cli = parse(&["--vm-count", "7", "deploy", "--no-build", "--verify"]);
        match cli.command {
            Commands::Deploy { no_build, verify } => {
                assert!(no_build);
                assert!(verify);
            }
            _ => panic!("expected Deploy"),
        }
    }

    // ── Up ───────────────────────────────────────────────────────────────

    #[test]
    fn up_detach() {
        let cli = parse(&[
            "--vm-count",
            "1",
            "up",
            "--detach",
            "--runners-dir",
            "/tmp/r",
        ]);
        match cli.command {
            Commands::Up {
                detach,
                runners_dir,
            } => {
                assert!(detach);
                assert!(runners_dir.is_some());
            }
            _ => panic!("expected Up"),
        }
    }

    // ── Network mode ─────────────────────────────────────────────────────

    #[test]
    fn network_mode_display() {
        assert_eq!(format!("{}", NetworkMode::Lan), "lan");
        assert_eq!(format!("{}", NetworkMode::Steam), "steam");
    }

    #[test]
    fn invalid_network_rejected() {
        let args = vec![
            "cluster-ctl",
            "--vm-count",
            "7",
            "test",
            "--network",
            "bluetooth",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err());
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn vm_count_boundary() {
        assert_eq!(parse(&["--vm-count", "0", "status"]).vm_count, Some(0));
        assert_eq!(parse(&["--vm-count", "255", "status"]).vm_count, Some(255));
    }

    #[test]
    fn missing_vm_count_is_none() {
        let cli = parse(&["status"]);
        assert!(cli.vm_count.is_none());
    }
}
