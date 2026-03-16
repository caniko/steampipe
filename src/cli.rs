use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cluster-ctl", about = "Chessbender VM cluster orchestration")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Project root (auto-detected from git/Cargo.toml workspace)
    #[arg(long, global = true)]
    pub project_root: Option<PathBuf>,

    /// SSH key path (default: <project_root>/nix/test-cluster/cluster_key)
    #[arg(long, global = true)]
    pub ssh_key: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Show status of all VMs (running, SSH, Steam, game)
    Status,

    /// Start all VMs
    Up {
        /// Path to directory containing vm-N/bin/microvm-run runners
        #[arg(long)]
        runners_dir: PathBuf,
    },

    /// Stop all VMs
    Down,

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
    },

    /// Interactive Steam login wizard (one VM at a time with VNC)
    SteamLogin {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
        /// Continue from target through vm-7 (e.g., "3" means vm-3..vm-7)
        #[arg(short = '+', long)]
        continue_from: bool,
        /// Path to directory containing 4GB login VM runners
        #[arg(long)]
        login_runners_dir: PathBuf,
    },

    /// Check Steam login + health on VMs (boots each VM to verify)
    SteamCheck {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
        /// Path to directory containing VM runners
        #[arg(long)]
        runners_dir: PathBuf,
    },

    /// Start weston + Steam on VMs
    SteamStart {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
    },

    /// Start weston + Steam + chessbender on all VMs (tournament mode)
    Run {
        /// Extra args passed to chessbender on VMs
        #[arg(trailing_var_arg = true)]
        extra_args: Vec<String>,
    },

    /// Kill chessbender on all VMs
    StopGame,

    /// Collect game.log from all VMs
    Logs {
        /// Output directory (default: logs/cluster-<timestamp>)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}
