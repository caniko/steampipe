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

#[derive(Parser)]
#[command(name = "cluster-ctl", about = "VM cluster orchestration for multiplayer game testing")]
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

    /// Backend: microvm (default), docker, or local
    #[arg(long, global = true, value_enum)]
    pub backend: Option<BackendKind>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Show status of all VMs (running, SSH, Steam, game)
    Status,

    /// Start VMs (uses --vm-count to determine how many)
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
    StopGame,

    /// Collect game.log from all VMs (or stream live with --follow)
    Logs {
        /// Output directory (default: logs/cluster-<timestamp>)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Stream logs in real-time from all VMs (tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Number of tail lines to show initially when following
        #[arg(short = 'n', long, default_value_t = 20)]
        tail: u32,
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

        /// Stop after first failure
        #[arg(long, default_value_t = true)]
        stop_on_failure: bool,

        /// Deploy binary + assets before testing
        #[arg(long, default_value_t = true)]
        deploy: bool,

        /// Build before deploying
        #[arg(long, default_value_t = true)]
        build: bool,

        /// Regex filter for output lines
        #[arg(long)]
        filter_pattern: Option<String>,

        /// Write full output to this file
        #[arg(long)]
        output_file: Option<PathBuf>,

        /// Capture screenshots on test failure
        #[arg(long)]
        capture_on_failure: bool,
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

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

/// Print shell completions to stdout.
pub fn print_completions(shell: Shell) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "cluster-ctl", &mut std::io::stdout());
}
