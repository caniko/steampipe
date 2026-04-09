use std::fmt;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use crate::config::BackendKind;
use crate::output::OutputFormat;

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
    /// Start Weston on virtio-gpu DRM device (VM needs microvm.graphics.enable)
    WestonGpu,
    /// Start Sway on virtio-gpu DRM device (VM needs microvm.graphics.enable)
    SwayGpu,
}

impl DisplayMode {
    /// Whether this display mode requires a virtio-gpu DRM device on the VM.
    pub fn requires_gpu(&self) -> bool {
        matches!(self, Self::WestonGpu | Self::SwayGpu)
    }
}

impl fmt::Display for DisplayMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Headless => f.write_str("headless"),
            Self::Weston => f.write_str("weston"),
            Self::Sway => f.write_str("sway"),
            Self::WestonGpu => f.write_str("weston-gpu"),
            Self::SwayGpu => f.write_str("sway-gpu"),
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

    /// Start VMs. Uses --vm-count to determine how many.
    Up {
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
        /// Load a named test profile from steampipe.toml
        #[arg(long)]
        profile: Option<String>,

        /// Network transport: lan or steam
        #[arg(long)]
        network: Option<NetworkMode>,

        /// Number of players (2-8; host + N-1 VMs)
        #[arg(long)]
        players: Option<u8>,

        /// Args passed to the game binary on each VM
        #[arg(long)]
        vm_args: Option<String>,

        /// Args passed to the local (host) game binary
        #[arg(long)]
        host_args: Option<String>,

        /// Number of test runs
        #[arg(long)]
        max_runs: Option<u32>,

        /// Timeout per run in seconds
        #[arg(long)]
        timeout: Option<u64>,

        /// Grace period (seconds) between SIGTERM and SIGKILL
        #[arg(long)]
        shutdown_timeout: Option<u64>,

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
        #[arg(long)]
        display: Option<DisplayMode>,

        /// Apply a chaos profile during test runs (from [chaos.*] in steampipe.toml)
        #[arg(long)]
        chaos_profile: Option<String>,

        /// Heartbeat stall threshold in seconds (overrides config and default of 30s)
        #[arg(long)]
        heartbeat_stall_secs: Option<u64>,

        /// Output format: text (default), junit, jsonl
        #[arg(long)]
        output_format: Option<OutputFormat>,

        /// Run shell command when test session completes (template vars: {pass_count}, {fail_count}, etc.)
        #[arg(long)]
        on_complete: Option<String>,

        /// Run shell command when any test fails
        #[arg(long)]
        on_failure: Option<String>,
    },

    /// Binary search git history to find the commit that introduced a test failure
    Bisect {
        /// Known good commit (tests pass)
        #[arg(long)]
        good: String,
        /// Known bad commit (tests fail)
        #[arg(long)]
        bad: String,
        /// Network transport: lan or steam
        #[arg(long, default_value = "lan")]
        network: NetworkMode,
        /// Number of players (2-8)
        #[arg(long, default_value_t = 2)]
        players: u8,
        /// Timeout per run in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Number of test runs per commit (more = catches flaky tests)
        #[arg(long, default_value_t = 1)]
        runs_per_step: u32,
        /// Fraction of runs that must pass for a commit to be "good" (0.0-1.0)
        #[arg(long, default_value_t = 1.0)]
        pass_threshold: f64,
        /// VM display mode
        #[arg(long, default_value = "headless")]
        display: DisplayMode,
        /// Args passed to the game binary on VMs
        #[arg(long)]
        vm_args: Option<String>,
        /// Args passed to the local (host) game binary
        #[arg(long)]
        host_args: Option<String>,
    },

    /// Apply network emulation (latency, packet loss, jitter, bandwidth). Requires sudo.
    Netem {
        /// Target VM: "all", "3", "vm-3"
        target: String,
        /// Latency in milliseconds
        #[arg(long, conflicts_with = "netem_profile")]
        latency: Option<u32>,
        /// Jitter in milliseconds (used with --latency)
        #[arg(long, conflicts_with = "netem_profile")]
        jitter: Option<u32>,
        /// Packet loss percentage (0-100)
        #[arg(long, conflicts_with = "netem_profile")]
        loss: Option<f32>,
        /// Bandwidth limit in kbit/s
        #[arg(long, conflicts_with = "netem_profile")]
        rate: Option<u32>,
        /// Apply a named chaos profile from steampipe.toml [chaos.*]
        #[arg(long = "profile", id = "netem_profile")]
        chaos_profile: Option<String>,
    },

    /// Show current network emulation status
    NetemShow,

    /// Reset (remove) all network emulation rules
    NetemReset {
        /// Target VM: "all", "3", "vm-3" (default: all)
        target: Option<String>,
    },

    /// Show test result history, trends, and analysis
    History {
        #[command(subcommand)]
        action: Option<HistoryAction>,
        /// Show only the last N sessions (for default view)
        #[arg(short = 'n', long)]
        last: Option<usize>,
        /// Clear all test history
        #[arg(long)]
        clear: bool,
        /// Export format: junit or jsonl
        #[arg(long, value_enum)]
        format: Option<OutputFormat>,
        /// Write export to file instead of stdout
        #[arg(long)]
        output: Option<PathBuf>,
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

/// History subcommands for analysis.
#[derive(Subcommand)]
pub enum HistoryAction {
    /// Analyze pass rate trends and detect regressions
    Trends {
        /// Comparison window size (number of results)
        #[arg(short, long, default_value_t = 10)]
        window: usize,
        /// Limit analysis to last N sessions
        #[arg(short = 'n', long)]
        last: Option<usize>,
    },
    /// Show flaky test detection results
    Flaky {
        /// Limit analysis to last N sessions
        #[arg(short = 'n', long)]
        last: Option<usize>,
    },
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
                assert!(matches!(network, Some(NetworkMode::Steam)));
                assert_eq!(players, Some(4));
                assert_eq!(max_runs, Some(10));
                assert_eq!(timeout, Some(120));
                assert_eq!(shutdown_timeout, Some(3));
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

    // ── Network mode ─────────────────────────────────────────────────────

    #[test]
    fn network_mode_display() {
        assert_eq!(format!("{}", NetworkMode::Lan), "lan");
        assert_eq!(format!("{}", NetworkMode::Steam), "steam");
    }

    // ── Display mode ────────────────────────────────────────────────────

    #[test]
    fn display_mode_display_all_variants() {
        assert_eq!(format!("{}", DisplayMode::Headless), "headless");
        assert_eq!(format!("{}", DisplayMode::Weston), "weston");
        assert_eq!(format!("{}", DisplayMode::Sway), "sway");
        assert_eq!(format!("{}", DisplayMode::WestonGpu), "weston-gpu");
        assert_eq!(format!("{}", DisplayMode::SwayGpu), "sway-gpu");
    }

    #[test]
    fn display_mode_equality() {
        assert_eq!(DisplayMode::WestonGpu, DisplayMode::WestonGpu);
        assert_eq!(DisplayMode::SwayGpu, DisplayMode::SwayGpu);
        assert_ne!(DisplayMode::WestonGpu, DisplayMode::Weston);
        assert_ne!(DisplayMode::SwayGpu, DisplayMode::Sway);
        assert_ne!(DisplayMode::WestonGpu, DisplayMode::Headless);
    }

    #[test]
    fn display_mode_requires_gpu() {
        assert!(!DisplayMode::Headless.requires_gpu());
        assert!(!DisplayMode::Weston.requires_gpu());
        assert!(!DisplayMode::Sway.requires_gpu());
        assert!(DisplayMode::WestonGpu.requires_gpu());
        assert!(DisplayMode::SwayGpu.requires_gpu());
    }

    #[test]
    fn display_mode_parse_weston_gpu() {
        let cli = parse(&[
            "--vm-count", "3", "test", "--display", "weston-gpu",
        ]);
        match cli.command {
            Commands::Test { display, .. } => {
                assert!(matches!(display, Some(DisplayMode::WestonGpu)));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn display_mode_parse_sway_gpu() {
        let cli = parse(&[
            "--vm-count", "3", "test", "--display", "sway-gpu",
        ]);
        match cli.command {
            Commands::Test { display, .. } => {
                assert!(matches!(display, Some(DisplayMode::SwayGpu)));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn display_mode_parse_rejects_invalid() {
        let mut args = vec!["cluster-ctl"];
        args.extend_from_slice(&[
            "--vm-count", "3", "test", "--display", "opengl",
        ]);
        assert!(Cli::try_parse_from(args).is_err());
    }

    #[test]
    fn bisect_display_weston_gpu() {
        let cli = parse(&[
            "--vm-count", "3", "bisect",
            "--good", "aaa", "--bad", "bbb",
            "--display", "weston-gpu",
        ]);
        match cli.command {
            Commands::Bisect { display, .. } => {
                assert!(matches!(display, DisplayMode::WestonGpu));
            }
            _ => panic!("expected Bisect"),
        }
    }

    #[test]
    fn bisect_display_sway_gpu() {
        let cli = parse(&[
            "--vm-count", "3", "bisect",
            "--good", "aaa", "--bad", "bbb",
            "--display", "sway-gpu",
        ]);
        match cli.command {
            Commands::Bisect { display, .. } => {
                assert!(matches!(display, DisplayMode::SwayGpu));
            }
            _ => panic!("expected Bisect"),
        }
    }

    // ── New feature: Test command optionalized fields ─────────────────

    #[test]
    fn test_fields_default_to_none() {
        let cli = parse(&["--vm-count", "7", "test"]);
        match cli.command {
            Commands::Test {
                profile,
                network,
                players,
                max_runs,
                timeout,
                shutdown_timeout,
                display,
                chaos_profile,
                heartbeat_stall_secs,
                output_format,
                on_complete,
                on_failure,
                ..
            } => {
                assert!(profile.is_none());
                assert!(network.is_none());
                assert!(players.is_none());
                assert!(max_runs.is_none());
                assert!(timeout.is_none());
                assert!(shutdown_timeout.is_none());
                assert!(display.is_none());
                assert!(chaos_profile.is_none());
                assert!(heartbeat_stall_secs.is_none());
                assert!(output_format.is_none());
                assert!(on_complete.is_none());
                assert!(on_failure.is_none());
            }
            _ => panic!("expected Test command"),
        }
    }

    #[test]
    fn test_profile_flag() {
        let cli = parse(&["--vm-count", "7", "test", "--profile", "smoke"]);
        match cli.command {
            Commands::Test { profile, .. } => {
                assert_eq!(profile.as_deref(), Some("smoke"));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_chaos_profile_flag() {
        let cli = parse(&[
            "--vm-count", "7", "test",
            "--chaos-profile", "unstable-wifi",
        ]);
        match cli.command {
            Commands::Test { chaos_profile, .. } => {
                assert_eq!(chaos_profile.as_deref(), Some("unstable-wifi"));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_heartbeat_stall_secs_flag() {
        let cli = parse(&["--vm-count", "7", "test", "--heartbeat-stall-secs", "60"]);
        match cli.command {
            Commands::Test { heartbeat_stall_secs, .. } => {
                assert_eq!(heartbeat_stall_secs, Some(60));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_output_format_junit() {
        let cli = parse(&["--vm-count", "7", "test", "--output-format", "junit"]);
        match cli.command {
            Commands::Test { output_format, .. } => {
                assert!(matches!(output_format, Some(OutputFormat::Junit)));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_output_format_jsonl() {
        let cli = parse(&["--vm-count", "7", "test", "--output-format", "jsonl"]);
        match cli.command {
            Commands::Test { output_format, .. } => {
                assert!(matches!(output_format, Some(OutputFormat::Jsonl)));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_hook_flags() {
        let cli = parse(&[
            "--vm-count", "7", "test",
            "--on-complete", "echo done",
            "--on-failure", "echo fail",
        ]);
        match cli.command {
            Commands::Test { on_complete, on_failure, .. } => {
                assert_eq!(on_complete.as_deref(), Some("echo done"));
                assert_eq!(on_failure.as_deref(), Some("echo fail"));
            }
            _ => panic!("expected Test"),
        }
    }

    // ── Bisect command ──────────────────────────────────────────────────

    #[test]
    fn bisect_defaults() {
        let cli = parse(&[
            "--vm-count", "3", "bisect",
            "--good", "abc123",
            "--bad", "def456",
        ]);
        match cli.command {
            Commands::Bisect {
                good,
                bad,
                network,
                players,
                timeout,
                runs_per_step,
                pass_threshold,
                display,
                vm_args,
                host_args,
            } => {
                assert_eq!(good, "abc123");
                assert_eq!(bad, "def456");
                assert!(matches!(network, NetworkMode::Lan));
                assert_eq!(players, 2);
                assert_eq!(timeout, 300);
                assert_eq!(runs_per_step, 1);
                assert!((pass_threshold - 1.0).abs() < f64::EPSILON);
                assert!(matches!(display, DisplayMode::Headless));
                assert!(vm_args.is_none());
                assert!(host_args.is_none());
            }
            _ => panic!("expected Bisect"),
        }
    }

    #[test]
    fn bisect_with_options() {
        let cli = parse(&[
            "--vm-count", "3", "bisect",
            "--good", "aaa",
            "--bad", "bbb",
            "--network", "steam",
            "--players", "4",
            "--runs-per-step", "3",
            "--pass-threshold", "0.67",
            "--display", "weston",
        ]);
        match cli.command {
            Commands::Bisect {
                network,
                players,
                runs_per_step,
                pass_threshold,
                display,
                ..
            } => {
                assert!(matches!(network, NetworkMode::Steam));
                assert_eq!(players, 4);
                assert_eq!(runs_per_step, 3);
                assert!((pass_threshold - 0.67).abs() < 0.01);
                assert!(matches!(display, DisplayMode::Weston));
            }
            _ => panic!("expected Bisect"),
        }
    }

    // ── History subcommands ─────────────────────────────────────────────

    #[test]
    fn history_bare_defaults() {
        let cli = parse(&["--vm-count", "7", "history"]);
        match cli.command {
            Commands::History { action, last, clear, format, output } => {
                assert!(action.is_none());
                assert!(last.is_none());
                assert!(!clear);
                assert!(format.is_none());
                assert!(output.is_none());
            }
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn history_with_last() {
        let cli = parse(&["--vm-count", "7", "history", "-n", "5"]);
        match cli.command {
            Commands::History { last, .. } => assert_eq!(last, Some(5)),
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn history_clear() {
        let cli = parse(&["--vm-count", "7", "history", "--clear"]);
        match cli.command {
            Commands::History { clear, .. } => assert!(clear),
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn history_export_junit() {
        let cli = parse(&["--vm-count", "7", "history", "--format", "junit"]);
        match cli.command {
            Commands::History { format, .. } => {
                assert!(matches!(format, Some(OutputFormat::Junit)));
            }
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn history_trends_subcommand() {
        let cli = parse(&["--vm-count", "7", "history", "trends", "--window", "5"]);
        match cli.command {
            Commands::History { action, .. } => match action {
                Some(HistoryAction::Trends { window, last }) => {
                    assert_eq!(window, 5);
                    assert!(last.is_none());
                }
                _ => panic!("expected Trends action"),
            },
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn history_flaky_subcommand() {
        let cli = parse(&["--vm-count", "7", "history", "flaky", "-n", "20"]);
        match cli.command {
            Commands::History { action, .. } => match action {
                Some(HistoryAction::Flaky { last }) => {
                    assert_eq!(last, Some(20));
                }
                _ => panic!("expected Flaky action"),
            },
            _ => panic!("expected History"),
        }
    }

    // ── Netem profile flag ──────────────────────────────────────────────

    #[test]
    fn netem_with_profile() {
        let cli = parse(&["--vm-count", "7", "netem", "all", "--profile", "wifi"]);
        match cli.command {
            Commands::Netem { target, chaos_profile, latency, .. } => {
                assert_eq!(target, "all");
                assert_eq!(chaos_profile.as_deref(), Some("wifi"));
                assert!(latency.is_none());
            }
            _ => panic!("expected Netem"),
        }
    }

    #[test]
    fn test_all_new_flags_combined() {
        let cli = parse(&[
            "--vm-count", "3", "test",
            "--profile", "stress",
            "--network", "steam",
            "--players", "4",
            "--chaos-profile", "wifi",
            "--heartbeat-stall-secs", "45",
            "--output-format", "jsonl",
            "--on-complete", "echo {pass_rate}%",
            "--on-failure", "alert",
            "--max-runs", "10",
            "--timeout", "600",
        ]);
        match cli.command {
            Commands::Test {
                profile,
                network,
                players,
                chaos_profile,
                heartbeat_stall_secs,
                output_format,
                on_complete,
                on_failure,
                max_runs,
                timeout,
                ..
            } => {
                assert_eq!(profile.as_deref(), Some("stress"));
                assert!(matches!(network, Some(NetworkMode::Steam)));
                assert_eq!(players, Some(4));
                assert_eq!(chaos_profile.as_deref(), Some("wifi"));
                assert_eq!(heartbeat_stall_secs, Some(45));
                assert!(matches!(output_format, Some(OutputFormat::Jsonl)));
                assert_eq!(on_complete.as_deref(), Some("echo {pass_rate}%"));
                assert_eq!(on_failure.as_deref(), Some("alert"));
                assert_eq!(max_runs, Some(10));
                assert_eq!(timeout, Some(600));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_invalid_output_format_rejected() {
        let args = vec![
            "cluster-ctl", "--vm-count", "7", "test",
            "--output-format", "csv",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err());
    }

    #[test]
    fn bisect_missing_good_fails() {
        let args = vec![
            "cluster-ctl", "--vm-count", "3", "bisect",
            "--bad", "def456",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err());
    }

    #[test]
    fn bisect_missing_bad_fails() {
        let args = vec![
            "cluster-ctl", "--vm-count", "3", "bisect",
            "--good", "abc123",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err());
    }

    #[test]
    fn history_export_with_output_file() {
        let cli = parse(&[
            "--vm-count", "7", "history",
            "--format", "jsonl",
            "--output", "/tmp/export.jsonl",
        ]);
        match cli.command {
            Commands::History { format, output, .. } => {
                assert!(matches!(format, Some(OutputFormat::Jsonl)));
                assert_eq!(output.unwrap().to_str().unwrap(), "/tmp/export.jsonl");
            }
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn history_trends_default_window() {
        let cli = parse(&["--vm-count", "7", "history", "trends"]);
        match cli.command {
            Commands::History { action, .. } => match action {
                Some(HistoryAction::Trends { window, .. }) => {
                    assert_eq!(window, 10); // default
                }
                _ => panic!("expected Trends"),
            },
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn history_flaky_default_no_last() {
        let cli = parse(&["--vm-count", "7", "history", "flaky"]);
        match cli.command {
            Commands::History { action, .. } => match action {
                Some(HistoryAction::Flaky { last }) => {
                    assert!(last.is_none());
                }
                _ => panic!("expected Flaky"),
            },
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn netem_profile_conflicts_with_params() {
        let args = vec![
            "cluster-ctl", "--vm-count", "7", "netem", "all",
            "--profile", "wifi", "--latency", "50",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err(), "profile should conflict with latency");
    }

    // ── Original tests ──────────────────────────────────────────────────

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
