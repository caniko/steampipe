use std::fmt;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use crate::core::config::BackendKind;
use crate::harness::output::OutputFormat;

/// Screenshot capture backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScreenshotBackend {
    /// Capture from the VM's Wayland compositor using grim.
    #[value(alias = "wayland")]
    Grim,
    /// Capture the viewer-visible VNC framebuffer.
    Vnc,
}

impl fmt::Display for ScreenshotBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grim => f.write_str("grim"),
            Self::Vnc => f.write_str("vnc"),
        }
    }
}

/// Lifecycle for automated screenshot capture during `cluster-ctl test`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CaptureMode {
    Off,
    Failure,
    FailureAndStall,
    Timelapse,
}

impl fmt::Display for CaptureMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Failure => f.write_str("failure"),
            Self::FailureAndStall => f.write_str("failure-and-stall"),
            Self::Timelapse => f.write_str("timelapse"),
        }
    }
}

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
    /// Start Sway compositor (default)
    Sway,
    /// Start Weston (headless backend) — experimental
    Weston,
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

    /// Total number of VMs in the cluster. Read from the discovered NixOS
    /// host config when omitted; required only when no host config exists.
    #[arg(long, global = true)]
    pub vm_count: Option<u8>,

    /// Path to TOML file with per-VM Steam credentials and game keys
    #[arg(long, global = true)]
    pub credentials: Option<PathBuf>,

    /// Path to a global host config JSON file. Overrides discovery at
    /// $XDG_CONFIG_HOME/steampipe/host.json and /etc/steampipe/{host,module}.json.
    #[arg(long, global = true)]
    pub host_config: Option<PathBuf>,

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

    /// Never prompt for input or open host UI windows; fail fast when interaction is required.
    #[arg(long, global = true)]
    pub non_interactive: bool,

    /// Allow terminal prompts but never open the Steam Guard GUI helper.
    #[arg(long, global = true)]
    pub no_gui: bool,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum Commands {
    /// Inspect admission gates used before VM startup
    Admission {
        #[command(subcommand)]
        action: AdmissionAction,
    },

    /// Show status of the VMs currently leased by this cluster
    Status,

    /// Show compositor readiness state on all VMs
    CompositorStatus,

    /// Start VMs. Uses --vm-count to determine how many.
    Up {
        /// Path to directory containing vm-N/bin/microvm-run runners. Optional
        /// when the discovered host config supplies runnersDir; otherwise
        /// required when using the microvm backend.
        #[arg(long)]
        runners_dir: Option<PathBuf>,
    },

    /// Stop VMs with graceful shutdown (falls back to force-kill if frozen or unreachable)
    Down {
        /// Skip graceful shutdown, force-kill immediately
        #[arg(long)]
        force: bool,
        /// Seconds to wait for graceful VM shutdown (default: 30)
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },

    /// Reclaim one live VM from another verified cluster lease
    Reclaim {
        /// VM name: "3" or "vm-3"
        #[arg(long)]
        vm: String,
        /// Expected current owning cluster. Reclaim fails if the live holder differs.
        #[arg(long)]
        from_cluster: String,
    },

    /// Restart VMs (stop then start)
    Restart {
        /// Target VM: "all", "3", "vm-3" (default: all)
        target: Option<String>,
        /// Path to directory containing vm-N/bin/microvm-run runners. Optional
        /// when the discovered host config supplies runnersDir; otherwise
        /// required when using the microvm backend.
        #[arg(long)]
        runners_dir: Option<PathBuf>,
    },

    /// Remove on-disk VM state so next boot starts fresh
    Reset {
        /// VM name: "3" or "vm-3"
        vm: String,
        /// Remove the home image. Required.
        #[arg(long, required = true)]
        home: bool,
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

    /// Steam-related operations (login, refresh, guard, check, start, accounts, clean-logins)
    Steam {
        #[command(subcommand)]
        action: SteamAction,
    },

    /// Verify in-VM GPU, Vulkan, and Wayland readiness
    GpuPreflight {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
        /// Path to directory containing VM runners (used to boot stopped VMs)
        #[arg(long)]
        runners_dir: Option<PathBuf>,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Start compositor + Steam + game on all VMs
    Run {
        /// VM display mode: headless, sway (default), weston, weston-gpu, or sway-gpu
        #[arg(long, default_value = "sway")]
        display: DisplayMode,
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
        #[arg(long, allow_hyphen_values = true)]
        vm_args: Option<String>,

        /// Args passed to the local (host) game binary
        #[arg(long, allow_hyphen_values = true)]
        host_args: Option<String>,

        /// Number of test runs
        #[arg(long)]
        max_runs: Option<u32>,

        /// No-progress timeout per run in seconds
        #[arg(long)]
        timeout: Option<u64>,

        /// Emergency wall-clock timeout per run in seconds
        #[arg(long)]
        hard_timeout: Option<u64>,

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

        /// Capture lifecycle mode: off, failure, failure-and-stall, or timelapse
        #[arg(long)]
        capture_mode: Option<CaptureMode>,

        /// Timelapse interval in seconds (used with --capture-mode timelapse)
        #[arg(long)]
        capture_interval: Option<u32>,

        /// Record a WebM video from each VM during the run
        #[arg(long)]
        record_video: bool,

        /// Comma-separated visual scenes to drive and capture during each run
        #[arg(long, value_delimiter = ',')]
        scenes: Vec<String>,

        /// VM to drive for scene captures (default: first leased VM)
        #[arg(long)]
        scene_vm: Option<String>,

        /// VM display mode: headless, weston (default), sway, weston-gpu, or sway-gpu
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

        /// Launch the local host process under strace (captures network syscalls)
        #[arg(long)]
        strace: bool,
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
        #[arg(long, allow_hyphen_values = true)]
        vm_args: Option<String>,
        /// Args passed to the local (host) game binary
        #[arg(long, allow_hyphen_values = true)]
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
        /// Visual scene to validate (default: all configured scenes)
        #[arg(long)]
        scene: Option<String>,
        /// Load validator defaults from a named test profile
        #[arg(long)]
        profile: Option<String>,
        /// Screenshot backend: grim or vnc
        #[arg(long = "screenshot-backend")]
        screenshot_backend: Option<ScreenshotBackend>,
        /// Run the configured project visual validator for each screenshot
        #[arg(long)]
        validate: bool,
    },

    /// Inspect or manage structured visual golden-image scenes
    Visual {
        #[command(subcommand)]
        action: VisualAction,
    },

    /// Live TUI dashboard showing VM status
    Watch {
        /// Refresh interval in seconds
        #[arg(short, long, default_value_t = 5)]
        interval: u64,
    },

    /// Health check and repair
    Doctor {
        /// Remove stale PID files, orphaned microvm processes, and leftover sockets
        #[arg(long)]
        fix: bool,
    },

    /// Clean up stale VM state: dead PID files, orphaned processes, leftover sockets
    Cleanup {
        /// Also remove home images for unclaimed VMs
        #[arg(long)]
        reset: bool,
        /// Preview changes without applying them
        #[arg(long)]
        dry_run: bool,
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

    /// Fixture-only maintenance commands
    #[cfg(feature = "fixture-tools")]
    Fixture {
        #[command(subcommand)]
        action: FixtureAction,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

#[derive(Subcommand)]
pub enum AdmissionAction {
    /// Print host memory stats and configured watermarks
    Status,
}

pub fn print_admission_status() {
    let policy = crate::core::admission::policy();
    match crate::core::admission::current_stats() {
        Some(stats) => {
            println!("memory-admission status:");
            println!("  total_mb       = {}", stats.total_bytes / 1024 / 1024);
            println!("  available_mb   = {}", stats.available_bytes / 1024 / 1024);
            println!(
                "  page_cache_mb  = {}",
                stats.page_cache_bytes / 1024 / 1024
            );
            println!("  high watermark = {} MB", policy.high_watermark_mb);
            println!("  low  watermark = {} MB", policy.low_watermark_mb);
            println!(
                "  memory scheduler = {}",
                if policy.memory_scheduler_enabled {
                    "active"
                } else {
                    "disengaged"
                }
            );
            println!("  max_ram_fraction = {:.4}", policy.max_ram_fraction);
            println!("  resume_hysteresis = {:.4}", policy.resume_hysteresis);
        }
        None => {
            println!(
                "memory-admission status: provider unavailable (gate is disengaged; startup is not throttled)"
            );
        }
    }
}

impl Commands {
    /// Whether this command requires the global `--vm-count` flag.
    ///
    /// `steam guard` targets exactly one VM by name; `netem` either targets
    /// one named VM or fans out across the already-leased set. Both derive
    /// their operational VM set from the cluster's lease state, so the user
    /// shouldn't have to repeat the pool size on the command line.
    pub fn needs_vm_count(&self) -> bool {
        !matches!(
            self,
            Self::Admission { .. }
                | Self::Steam {
                    action: SteamAction::Guard { .. }
                        | SteamAction::Refresh { .. }
                        | SteamAction::Info
                }
                | Self::Netem { .. }
        )
    }
}

/// Steam-related subcommands. Grouped under `cluster-ctl steam` to keep the
/// top-level CLI focused.
#[derive(Subcommand)]
pub enum SteamAction {
    /// Interactive Steam login wizard (one VM at a time with VNC).
    /// On NixOS hosts with services.steampipe-cluster.loginRunners.enable
    /// = true, runs without flags; otherwise pass --login-runners-dir.
    Login {
        /// Target VM: "all" (default), "3", or "vm-3". Omit to log into all VMs.
        target: Option<String>,
        /// Continue from target through vm-7 (e.g., "3" means vm-3..vm-7)
        #[arg(short = '+', long)]
        continue_from: bool,
        /// Path to directory containing 4 GB login VM runners. Optional when
        /// the NixOS module sets `services.steampipe-cluster.loginRunners.enable
        /// = true` (the path is then read from /etc/steampipe/module.json).
        /// Required outside a project root when no NixOS host config supplies
        /// it.
        #[arg(long)]
        login_runners_dir: Option<PathBuf>,
        /// Re-login every selected VM even if it already has a valid Steam session.
        #[arg(long)]
        force: bool,
        /// Pre-supplied Steam Guard code for single-shot automated login.
        #[arg(long)]
        code: Option<String>,
    },

    /// Refresh persisted SteamClient sessions host-side without booting VMs
    Refresh {
        /// Target VM: "all" (default), "3", or "vm-3". Omit to refresh all VMs.
        target: Option<String>,
        /// Continue from target through vm-7 (e.g., "3" means vm-3..vm-7)
        #[arg(short = '+', long)]
        continue_from: bool,
        /// Re-login every selected VM even if it already has a valid Steam GUI session.
        #[arg(long)]
        force: bool,
        /// Pre-supplied Steam Guard code for single-shot GUI login.
        #[arg(long)]
        code: Option<String>,
    },

    /// Submit Steam Guard code to a running Steam GUI login VM
    Guard {
        /// Target VM: "3" or "vm-3"
        target: String,
        /// Steam Guard code. If omitted, read one line from stdin.
        #[arg(long)]
        code: Option<String>,
    },

    /// Check Steam login + health on VMs (boots each VM to verify)
    Check {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
        /// Path to directory containing VM runners. Optional when the NixOS
        /// module sets `services.steampipe-cluster.runners.enable = true`
        /// (the path is then read from /etc/steampipe/module.json). Required
        /// outside a project root when no NixOS host config supplies it.
        #[arg(long)]
        runners_dir: Option<PathBuf>,
        /// VM display mode: headless, sway (default), weston, weston-gpu, or sway-gpu
        #[arg(long, default_value = "sway")]
        display: DisplayMode,
    },

    /// Deprecated: Steam GUI sessions must be created with `steam login`
    Warm {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
        /// Continue from target through vm-7 (e.g., "3" means vm-3..vm-7)
        #[arg(short = '+', long)]
        continue_from: bool,
        /// Path to directory containing VM runners. Optional when the NixOS
        /// module sets `services.steampipe-cluster.runners.enable = true`
        /// (the path is then read from /etc/steampipe/module.json). Required
        /// outside a project root when no NixOS host config supplies it.
        #[arg(long)]
        runners_dir: Option<PathBuf>,
    },

    /// Start compositor + Steam on VMs
    Start {
        /// Target VM: "all", "3", "vm-3"
        target: Option<String>,
        /// VM display mode: headless, sway (default), weston, weston-gpu, or sway-gpu
        #[arg(long, default_value = "sway")]
        display: DisplayMode,
    },

    /// Show Steam account status across all VMs
    Accounts {
        /// Exit non-zero if any VM is missing GUI-valid Steam session evidence.
        #[arg(long, default_value_t = 30)]
        warn_within_days: i64,
    },

    /// Show the discovered global host config + local login-state summary
    Info,

    /// Remove persisted Steam login state (from loginStateDir)
    CleanLogins {
        /// Target VM: "all", "3", "vm-3" (default: all)
        target: Option<String>,
        /// Login state directory (default: /var/lib/steampipe/logins)
        #[arg(long)]
        login_state_dir: Option<PathBuf>,
    },
}

/// Fixture-only maintenance command groups.
#[cfg(feature = "fixture-tools")]
#[derive(Subcommand)]
pub enum FixtureAction {
    /// Manage the in-tree fixture golden image set
    Golden {
        #[command(subcommand)]
        action: FixtureGoldenAction,
    },
}

/// In-tree fixture golden image commands.
#[cfg(feature = "fixture-tools")]
#[derive(Subcommand)]
pub enum FixtureGoldenAction {
    /// Verify committed fixture goldens against MANIFEST.toml
    Verify {
        /// Golden directory containing MANIFEST.toml and PNGs
        #[arg(long)]
        golden_dir: Option<PathBuf>,
        /// Manifest path; defaults to <golden-dir>/MANIFEST.toml
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Regenerate candidate fixture golden PNGs without promoting them
    Regenerate {
        /// Fixture directory
        #[arg(long)]
        fixture: Option<PathBuf>,
        /// Output directory for candidate PNGs
        #[arg(long)]
        output: Option<PathBuf>,
        /// Allow non-canonical capture from a dirty checkout or fixture lockfile
        #[arg(long)]
        allow_dirty: bool,
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

/// Visual golden-image testing subcommands.
#[derive(Subcommand)]
pub enum VisualAction {
    /// List configured visual scenes
    List,
    /// Drive a scripted scene on one VM, then capture and validate it
    Capture {
        /// Scene name from [[visual.scene]]
        scene: String,
        /// Target VM: "vm-1" or "1" (default: first VM)
        #[arg(long)]
        vm: Option<String>,
        /// Output directory for the captured frame
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Capture the current frame and write it as the scene golden image
    Record {
        /// Either <scene> or <vm> <scene>
        #[arg(value_name = "VM_OR_SCENE", num_args = 1..=2)]
        args: Vec<String>,
        /// Overwrite an existing golden image
        #[arg(long)]
        force: bool,
    },
    /// Promote an actual frame from a previous run to the scene golden image
    Bless {
        /// Scene name from [[visual.scene]]
        scene: String,
        /// Failed run label to bless from
        #[arg(long = "from")]
        from: Option<String>,
        /// Skip interactive confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Re-run comparison against an already-captured actual frame
    Diff {
        /// Scene name from [[visual.scene]]
        scene: String,
        /// Run label containing the captured actual frame
        #[arg(long = "from")]
        from: Option<String>,
    },
    /// Generate an HTML report from stored visual artifacts
    Report {
        /// Run label to render (default: most recent run with visual results)
        #[arg(long = "run")]
        run_label: Option<String>,
        /// Output directory for the generated report
        #[arg(long)]
        output: Option<PathBuf>,
        /// Inline images and videos as data URLs
        #[arg(long)]
        single_file: bool,
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

    #[test]
    fn reclaim_parses_vm_and_from_cluster() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "--cluster",
            "tournament",
            "reclaim",
            "--vm",
            "vm-1",
            "--from-cluster",
            "1v1",
        ]);
        match cli.command {
            Commands::Reclaim { vm, from_cluster } => {
                assert_eq!(vm, "vm-1");
                assert_eq!(from_cluster, "1v1");
            }
            _ => panic!("expected Reclaim command"),
        }
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
    fn steam_login_target_accepts_global_credentials_after_subcommand() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "steam",
            "login",
            "vm-1",
            "--credentials",
            "/etc/steampipe/credentials.toml",
        ]);
        assert_eq!(
            cli.credentials.as_deref(),
            Some(std::path::Path::new("/etc/steampipe/credentials.toml"))
        );
        match cli.command {
            Commands::Steam {
                action:
                    SteamAction::Login {
                        target,
                        continue_from,
                        login_runners_dir,
                        force,
                        code,
                    },
            } => {
                assert_eq!(target.as_deref(), Some("vm-1"));
                assert!(!continue_from);
                assert!(login_runners_dir.is_none());
                assert!(!force);
                assert!(code.is_none());
            }
            _ => panic!("expected Steam login command"),
        }
    }

    #[test]
    fn steam_login_force_flag_parses_and_defaults_false() {
        let cli = parse(&["--vm-count", "7", "steam", "login"]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Login { force, .. },
            } => assert!(!force),
            _ => panic!("expected Steam login command"),
        }
    }

    #[test]
    fn steam_login_force_flag_flips_true() {
        let cli = parse(&["--vm-count", "7", "steam", "login", "--force"]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Login { force, .. },
            } => assert!(force),
            _ => panic!("expected Steam login command"),
        }
    }

    #[test]
    fn global_interactivity_flags_parse() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "--non-interactive",
            "--no-gui",
            "steam",
            "login",
        ]);
        assert!(cli.non_interactive);
        assert!(cli.no_gui);
    }

    #[test]
    fn steam_login_accepts_guard_code() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "steam",
            "login",
            "vm-1",
            "--code",
            "ABCDE",
        ]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Login { target, code, .. },
            } => {
                assert_eq!(target.as_deref(), Some("vm-1"));
                assert_eq!(code.as_deref(), Some("ABCDE"));
            }
            _ => panic!("expected Steam login command"),
        }
    }

    #[test]
    fn steam_refresh_accepts_target_force_and_code() {
        let cli = parse(&["steam", "refresh", "vm-3", "--force", "--code", "ABCDE"]);
        assert!(cli.vm_count.is_none());
        assert!(!cli.command.needs_vm_count());
        match cli.command {
            Commands::Steam {
                action:
                    SteamAction::Refresh {
                        target,
                        continue_from,
                        force,
                        code,
                    },
            } => {
                assert_eq!(target.as_deref(), Some("vm-3"));
                assert!(!continue_from);
                assert!(force);
                assert_eq!(code.as_deref(), Some("ABCDE"));
            }
            _ => panic!("expected Steam refresh command"),
        }
    }

    #[test]
    fn steam_refresh_accepts_continue_from_short_flag() {
        let cli = parse(&["steam", "refresh", "-+", "3"]);
        assert!(!cli.command.needs_vm_count());
        match cli.command {
            Commands::Steam {
                action:
                    SteamAction::Refresh {
                        target,
                        continue_from,
                        force,
                        code,
                    },
            } => {
                assert_eq!(target.as_deref(), Some("3"));
                assert!(continue_from);
                assert!(!force);
                assert!(code.is_none());
            }
            _ => panic!("expected Steam refresh command"),
        }
    }

    #[test]
    fn steam_guard_accepts_target_and_code() {
        let cli = parse(&["steam", "guard", "vm-1", "--code", "ABCDE"]);
        assert!(cli.vm_count.is_none());
        assert!(!cli.command.needs_vm_count());
        match cli.command {
            Commands::Steam {
                action: SteamAction::Guard { target, code },
            } => {
                assert_eq!(target, "vm-1");
                assert_eq!(code.as_deref(), Some("ABCDE"));
            }
            _ => panic!("expected Steam guard command"),
        }
    }

    #[test]
    fn steam_guard_accepts_global_credentials_after_subcommand() {
        let cli = parse(&[
            "steam",
            "guard",
            "vm-1",
            "--credentials",
            "/etc/steampipe/credentials.toml",
            "--code",
            "ABCDE",
        ]);
        assert_eq!(
            cli.credentials.as_deref(),
            Some(std::path::Path::new("/etc/steampipe/credentials.toml"))
        );
        match cli.command {
            Commands::Steam {
                action: SteamAction::Guard { target, code },
            } => {
                assert_eq!(target, "vm-1");
                assert_eq!(code.as_deref(), Some("ABCDE"));
            }
            _ => panic!("expected Steam guard command"),
        }
    }

    #[test]
    fn steam_info_does_not_need_vm_count() {
        let cli = parse(&["steam", "info"]);
        assert!(!cli.command.needs_vm_count());
        match cli.command {
            Commands::Steam {
                action: SteamAction::Info,
            } => {}
            _ => panic!("expected Steam info command"),
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

    #[test]
    fn screenshot_backend_vnc() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "screenshot",
            "--screenshot-backend",
            "vnc",
            "--validate",
        ]);
        match cli.command {
            Commands::Screenshot {
                screenshot_backend,
                validate,
                ..
            } => {
                assert!(matches!(screenshot_backend, Some(ScreenshotBackend::Vnc)));
                assert!(validate);
            }
            _ => panic!("expected Screenshot command"),
        }
    }

    #[test]
    fn screenshot_profile_flag() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "screenshot",
            "--profile",
            "uifull-1v1-lan",
            "--validate",
        ]);
        match cli.command {
            Commands::Screenshot {
                profile,
                screenshot_backend,
                validate,
                ..
            } => {
                assert_eq!(profile.as_deref(), Some("uifull-1v1-lan"));
                assert!(screenshot_backend.is_none());
                assert!(validate);
            }
            _ => panic!("expected Screenshot command"),
        }
    }

    #[test]
    fn compositor_status_parses() {
        let cli = parse(&["--vm-count", "7", "compositor-status"]);
        match cli.command {
            Commands::CompositorStatus => {}
            _ => panic!("expected CompositorStatus command"),
        }
    }

    #[test]
    fn visual_list_parses_without_vm_count() {
        let cli = parse(&["visual", "list"]);
        match cli.command {
            Commands::Visual {
                action: VisualAction::List,
            } => {}
            _ => panic!("expected Visual list"),
        }
    }

    #[test]
    fn visual_record_accepts_vm_and_scene() {
        let cli = parse(&["visual", "record", "vm-1", "main-menu"]);
        match cli.command {
            Commands::Visual {
                action: VisualAction::Record { args, .. },
            } => {
                assert_eq!(args, vec!["vm-1", "main-menu"]);
            }
            _ => panic!("expected Visual record"),
        }
    }

    #[test]
    fn visual_capture_accepts_scene_and_vm() {
        let cli = parse(&["visual", "capture", "main-menu", "--vm", "vm-2"]);
        match cli.command {
            Commands::Visual {
                action: VisualAction::Capture { scene, vm, .. },
            } => {
                assert_eq!(scene, "main-menu");
                assert_eq!(vm.as_deref(), Some("vm-2"));
            }
            _ => panic!("expected Visual capture"),
        }
    }

    #[test]
    fn test_accepts_scene_list() {
        let cli = parse(&["--vm-count", "7", "test", "--scenes", "a,b,c"]);
        match cli.command {
            Commands::Test { scenes, .. } => {
                assert_eq!(scenes, vec!["a", "b", "c"]);
            }
            _ => panic!("expected Test"),
        }
    }

    // ── Run command display mode ──────────────────────────────────────────

    #[test]
    fn run_display_default_is_sway() {
        let cli = parse(&["--vm-count", "7", "run"]);
        match cli.command {
            Commands::Run { display, .. } => {
                assert!(matches!(display, DisplayMode::Sway));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_display_weston_gpu() {
        let cli = parse(&["--vm-count", "7", "run", "--display", "weston-gpu"]);
        match cli.command {
            Commands::Run { display, .. } => {
                assert!(matches!(display, DisplayMode::WestonGpu));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_display_sway_gpu() {
        let cli = parse(&["--vm-count", "7", "run", "--display", "sway-gpu"]);
        match cli.command {
            Commands::Run { display, .. } => {
                assert!(matches!(display, DisplayMode::SwayGpu));
            }
            _ => panic!("expected Run"),
        }
    }

    // ── Steam start display mode ────────────────────────────────────────

    #[test]
    fn steam_start_display_default_is_sway() {
        let cli = parse(&["--vm-count", "7", "steam", "start"]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Start { display, .. },
            } => {
                assert!(matches!(display, DisplayMode::Sway));
            }
            _ => panic!("expected Steam start"),
        }
    }

    #[test]
    fn steam_start_display_weston_gpu() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "steam",
            "start",
            "--display",
            "weston-gpu",
        ]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Start { display, .. },
            } => {
                assert!(matches!(display, DisplayMode::WestonGpu));
            }
            _ => panic!("expected Steam start"),
        }
    }

    // ── Steam check display mode ────────────────────────────────────────

    #[test]
    fn steam_check_display_default_is_sway() {
        let cli = parse(&["--vm-count", "7", "steam", "check"]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Check { display, .. },
            } => {
                assert!(matches!(display, DisplayMode::Sway));
            }
            _ => panic!("expected Steam check"),
        }
    }

    #[test]
    fn steam_check_display_sway_gpu() {
        let cli = parse(&["--vm-count", "7", "steam", "check", "--display", "sway-gpu"]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Check { display, .. },
            } => {
                assert!(matches!(display, DisplayMode::SwayGpu));
            }
            _ => panic!("expected Steam check"),
        }
    }

    #[test]
    fn steam_accounts_warn_within_days_defaults_to_30() {
        let cli = parse(&["--vm-count", "7", "steam", "accounts"]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Accounts { warn_within_days },
            } => {
                assert_eq!(warn_within_days, 30);
            }
            _ => panic!("expected Steam accounts"),
        }
    }

    #[test]
    fn steam_accounts_accepts_warn_within_days() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "steam",
            "accounts",
            "--warn-within-days",
            "365",
        ]);
        match cli.command {
            Commands::Steam {
                action: SteamAction::Accounts { warn_within_days },
            } => {
                assert_eq!(warn_within_days, 365);
            }
            _ => panic!("expected Steam accounts"),
        }
    }

    #[test]
    fn steam_warm_accepts_runners_dir() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "steam",
            "warm",
            "vm-3",
            "--runners-dir",
            "./result",
        ]);
        match cli.command {
            Commands::Steam {
                action:
                    SteamAction::Warm {
                        target,
                        continue_from,
                        runners_dir,
                    },
            } => {
                assert_eq!(target.as_deref(), Some("vm-3"));
                assert!(!continue_from);
                assert_eq!(runners_dir, Some(std::path::PathBuf::from("./result")));
            }
            _ => panic!("expected Steam warm"),
        }
    }

    #[test]
    fn steam_warm_accepts_continue_from_short_flag() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "steam",
            "warm",
            "-+",
            "vm-3",
            "--runners-dir",
            "./result",
        ]);
        match cli.command {
            Commands::Steam {
                action:
                    SteamAction::Warm {
                        target,
                        continue_from,
                        runners_dir,
                    },
            } => {
                assert_eq!(target.as_deref(), Some("vm-3"));
                assert!(continue_from);
                assert_eq!(runners_dir, Some(std::path::PathBuf::from("./result")));
            }
            _ => panic!("expected Steam warm"),
        }
    }

    #[test]
    fn steam_warm_accepts_missing_runners_dir() {
        let cli = parse(&["--vm-count", "7", "steam", "warm", "vm-3"]);
        match cli.command {
            Commands::Steam {
                action:
                    SteamAction::Warm {
                        target,
                        runners_dir,
                        ..
                    },
            } => {
                assert_eq!(target.as_deref(), Some("vm-3"));
                assert!(runners_dir.is_none());
            }
            _ => panic!("expected Steam warm"),
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
            "--hard-timeout",
            "600",
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
                hard_timeout,
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
                assert_eq!(hard_timeout, Some(600));
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

    #[test]
    fn test_allows_space_separated_hyphen_values_for_game_args() {
        let cli = parse(&[
            "test",
            "--host-args",
            "--headless",
            "--vm-args",
            "--auto-join",
        ]);
        match cli.command {
            Commands::Test {
                host_args, vm_args, ..
            } => {
                assert_eq!(host_args.as_deref(), Some("--headless"));
                assert_eq!(vm_args.as_deref(), Some("--auto-join"));
            }
            _ => panic!("expected Test"),
        }

        let cli = parse(&[
            "test",
            "--host-args=--auto-host-udp 0.0.0.0:7777",
            "--vm-args=--auto-join-udp 1.2.3.4:7777",
        ]);
        match cli.command {
            Commands::Test {
                host_args, vm_args, ..
            } => {
                assert_eq!(host_args.as_deref(), Some("--auto-host-udp 0.0.0.0:7777"));
                assert_eq!(vm_args.as_deref(), Some("--auto-join-udp 1.2.3.4:7777"));
            }
            _ => panic!("expected Test"),
        }

        // allow_hyphen_values only permits a flag-like value; it does not make
        // the option consume multiple space-separated tokens.
        let args = [
            "cluster-ctl",
            "test",
            "--host-args",
            "--auto-host-udp",
            "0.0.0.0:7777",
            "--vm-args",
            "--auto-join-udp",
            "1.2.3.4:7777",
        ];
        assert!(Cli::try_parse_from(args).is_err());
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
        let cli = parse(&["--vm-count", "3", "test", "--display", "weston-gpu"]);
        match cli.command {
            Commands::Test { display, .. } => {
                assert!(matches!(display, Some(DisplayMode::WestonGpu)));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn display_mode_parse_sway_gpu() {
        let cli = parse(&["--vm-count", "3", "test", "--display", "sway-gpu"]);
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
        args.extend_from_slice(&["--vm-count", "3", "test", "--display", "opengl"]);
        assert!(Cli::try_parse_from(args).is_err());
    }

    #[test]
    fn bisect_display_weston_gpu() {
        let cli = parse(&[
            "--vm-count",
            "3",
            "bisect",
            "--good",
            "aaa",
            "--bad",
            "bbb",
            "--display",
            "weston-gpu",
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
            "--vm-count",
            "3",
            "bisect",
            "--good",
            "aaa",
            "--bad",
            "bbb",
            "--display",
            "sway-gpu",
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
                hard_timeout,
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
                assert!(hard_timeout.is_none());
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
            "--vm-count",
            "7",
            "test",
            "--chaos-profile",
            "unstable-wifi",
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
            Commands::Test {
                heartbeat_stall_secs,
                ..
            } => {
                assert_eq!(heartbeat_stall_secs, Some(60));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_hard_timeout_flag() {
        let cli = parse(&["--vm-count", "7", "test", "--hard-timeout", "900"]);
        match cli.command {
            Commands::Test { hard_timeout, .. } => {
                assert_eq!(hard_timeout, Some(900));
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
            "--vm-count",
            "7",
            "test",
            "--on-complete",
            "echo done",
            "--on-failure",
            "echo fail",
        ]);
        match cli.command {
            Commands::Test {
                on_complete,
                on_failure,
                ..
            } => {
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
            "--vm-count",
            "3",
            "bisect",
            "--good",
            "abc123",
            "--bad",
            "def456",
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
            "--vm-count",
            "3",
            "bisect",
            "--good",
            "aaa",
            "--bad",
            "bbb",
            "--network",
            "steam",
            "--players",
            "4",
            "--runs-per-step",
            "3",
            "--pass-threshold",
            "0.67",
            "--display",
            "weston",
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
            Commands::History {
                action,
                last,
                clear,
                format,
                output,
            } => {
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
        let cli = parse(&["netem", "all", "--profile", "wifi"]);
        assert!(cli.vm_count.is_none());
        assert!(!cli.command.needs_vm_count());
        match cli.command {
            Commands::Netem {
                target,
                chaos_profile,
                latency,
                ..
            } => {
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
            "--vm-count",
            "3",
            "test",
            "--profile",
            "stress",
            "--network",
            "steam",
            "--players",
            "4",
            "--chaos-profile",
            "wifi",
            "--heartbeat-stall-secs",
            "45",
            "--output-format",
            "jsonl",
            "--on-complete",
            "echo {pass_rate}%",
            "--on-failure",
            "alert",
            "--max-runs",
            "10",
            "--timeout",
            "600",
            "--hard-timeout",
            "1800",
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
                hard_timeout,
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
                assert_eq!(hard_timeout, Some(1800));
            }
            _ => panic!("expected Test"),
        }
    }

    #[test]
    fn test_invalid_output_format_rejected() {
        let args = vec![
            "cluster-ctl",
            "--vm-count",
            "7",
            "test",
            "--output-format",
            "csv",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err());
    }

    #[test]
    fn bisect_missing_good_fails() {
        let args = vec![
            "cluster-ctl",
            "--vm-count",
            "3",
            "bisect",
            "--bad",
            "def456",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err());
    }

    #[test]
    fn bisect_missing_bad_fails() {
        let args = vec![
            "cluster-ctl",
            "--vm-count",
            "3",
            "bisect",
            "--good",
            "abc123",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err());
    }

    #[test]
    fn history_export_with_output_file() {
        let cli = parse(&[
            "--vm-count",
            "7",
            "history",
            "--format",
            "jsonl",
            "--output",
            "/tmp/export.jsonl",
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
            "cluster-ctl",
            "netem",
            "all",
            "--profile",
            "wifi",
            "--latency",
            "50",
        ];
        let result = Cli::try_parse_from(args);
        assert!(result.is_err(), "profile should conflict with latency");
    }

    #[test]
    fn needs_vm_count_waives_steam_guard_info_and_netem() {
        assert!(
            !parse(&["steam", "guard", "vm-1", "--code", "X"])
                .command
                .needs_vm_count()
        );
        assert!(!parse(&["steam", "info"]).command.needs_vm_count());
        assert!(
            !parse(&["netem", "vm-1", "--latency", "50"])
                .command
                .needs_vm_count()
        );
        assert!(
            !parse(&["netem", "all", "--loss", "1"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "7", "status"])
                .command
                .needs_vm_count()
        );
        assert!(parse(&["--vm-count", "7", "up"]).command.needs_vm_count());
        assert!(parse(&["--vm-count", "7", "down"]).command.needs_vm_count());
        // `steam login` and `steam check` still need vm-count (they fan out)
        assert!(
            parse(&["--vm-count", "7", "steam", "login"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "7", "steam", "check"])
                .command
                .needs_vm_count()
        );
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

    // ── Up / Down / Restart ───────────────────────────────────────────

    #[test]
    fn up_parses_basic() {
        let cli = parse(&["--vm-count", "3", "up"]);
        assert!(matches!(cli.command, Commands::Up { .. }));
    }

    #[test]
    fn up_accepts_runners_dir() {
        let cli = parse(&["--vm-count", "3", "up", "--runners-dir", "/runners"]);
        assert!(matches!(cli.command, Commands::Up { .. }));
    }

    #[test]
    fn down_parses_basic() {
        let cli = parse(&["--vm-count", "3", "down"]);
        assert!(matches!(cli.command, Commands::Down { .. }));
    }

    #[test]
    fn down_accepts_force_and_timeout() {
        let cli = parse(&["--vm-count", "3", "down", "--force", "--timeout", "60"]);
        assert!(matches!(cli.command, Commands::Down { .. }));
    }

    #[test]
    fn restart_parses_basic() {
        let cli = parse(&["--vm-count", "3", "restart"]);
        assert!(matches!(cli.command, Commands::Restart { .. }));
    }

    #[test]
    fn restart_accepts_target_and_runners_dir() {
        let cli = parse(&["--vm-count", "3", "restart", "vm-2", "--runners-dir", "/r"]);
        assert!(matches!(cli.command, Commands::Restart { .. }));
    }

    #[test]
    fn reset_requires_home() {
        let cli = parse(&["--vm-count", "3", "reset", "vm-3", "--home"]);
        assert!(matches!(cli.command, Commands::Reset { .. }));
    }

    // ── Net commands ───────────────────────────────────────────────────

    #[test]
    fn net_up_parses() {
        let cli = parse(&["net-up"]);
        assert!(matches!(cli.command, Commands::NetUp { .. }));
    }

    #[test]
    fn net_up_accepts_nft_override() {
        let cli = parse(&["net-up", "--nft", "/usr/sbin/nft"]);
        assert!(matches!(cli.command, Commands::NetUp { .. }));
    }

    #[test]
    fn net_down_parses() {
        let cli = parse(&["net-down"]);
        assert!(matches!(cli.command, Commands::NetDown { .. }));
    }

    #[test]
    fn net_down_accepts_nft_override() {
        let cli = parse(&["net-down", "--nft", "/usr/sbin/nft"]);
        assert!(matches!(cli.command, Commands::NetDown { .. }));
    }

    // ── Netem commands ─────────────────────────────────────────────────

    #[test]
    fn netem_show_parses() {
        let cli = parse(&["--vm-count", "3", "netem-show"]);
        assert!(matches!(cli.command, Commands::NetemShow));
    }

    #[test]
    fn netem_reset_parses() {
        let cli = parse(&["--vm-count", "3", "netem-reset"]);
        assert!(matches!(cli.command, Commands::NetemReset { .. }));
    }

    #[test]
    fn netem_reset_accepts_target() {
        let cli = parse(&["--vm-count", "3", "netem-reset", "vm-2"]);
        assert!(matches!(cli.command, Commands::NetemReset { .. }));
    }

    // ── Snapshot commands ──────────────────────────────────────────────

    #[test]
    fn snapshot_save_parses() {
        let cli = parse(&["--vm-count", "3", "snapshot-save", "pre-update"]);
        assert!(matches!(cli.command, Commands::SnapshotSave { .. }));
    }

    #[test]
    fn snapshot_restore_parses() {
        let cli = parse(&["--vm-count", "3", "snapshot-restore", "pre-update"]);
        assert!(matches!(cli.command, Commands::SnapshotRestore { .. }));
    }

    #[test]
    fn snapshot_list_parses() {
        let cli = parse(&["--vm-count", "3", "snapshot-list"]);
        assert!(matches!(cli.command, Commands::SnapshotList));
    }

    #[test]
    fn snapshot_delete_parses() {
        let cli = parse(&["--vm-count", "3", "snapshot-delete", "pre-update"]);
        assert!(matches!(cli.command, Commands::SnapshotDelete { .. }));
    }

    // ── Visual subcommands ─────────────────────────────────────────────

    #[test]
    fn visual_bless_parses() {
        let cli = parse(&[
            "--vm-count",
            "3",
            "visual",
            "bless",
            "main-menu",
            "--from",
            "run-7",
        ]);
        assert!(matches!(
            cli.command,
            Commands::Visual {
                action: VisualAction::Bless { .. }
            }
        ));
    }

    #[test]
    fn visual_bless_accepts_yes() {
        let cli = parse(&[
            "--vm-count",
            "3",
            "visual",
            "bless",
            "main-menu",
            "--from",
            "run-7",
            "--yes",
        ]);
        assert!(matches!(
            cli.command,
            Commands::Visual {
                action: VisualAction::Bless { .. }
            }
        ));
    }

    #[test]
    fn visual_diff_parses() {
        let cli = parse(&[
            "--vm-count",
            "3",
            "visual",
            "diff",
            "main-menu",
            "--from",
            "run-7",
        ]);
        assert!(matches!(
            cli.command,
            Commands::Visual {
                action: VisualAction::Diff { .. }
            }
        ));
    }

    #[test]
    fn visual_report_parses() {
        let cli = parse(&["--vm-count", "3", "visual", "report"]);
        assert!(matches!(
            cli.command,
            Commands::Visual {
                action: VisualAction::Report { .. }
            }
        ));
    }

    #[test]
    fn visual_report_accepts_run_output_and_single_file() {
        let cli = parse(&[
            "--vm-count",
            "3",
            "visual",
            "report",
            "--run",
            "run-7",
            "--output",
            "/tmp/report",
            "--single-file",
        ]);
        assert!(matches!(
            cli.command,
            Commands::Visual {
                action: VisualAction::Report { .. }
            }
        ));
    }

    // ── GPU preflight ──────────────────────────────────────────────────

    #[test]
    fn gpu_preflight_parses() {
        let cli = parse(&["--vm-count", "3", "gpu-preflight"]);
        assert!(matches!(cli.command, Commands::GpuPreflight { .. }));
    }

    #[test]
    fn gpu_preflight_accepts_target_and_json() {
        let cli = parse(&["--vm-count", "3", "gpu-preflight", "vm-2", "--json"]);
        assert!(matches!(cli.command, Commands::GpuPreflight { .. }));
    }

    #[test]
    fn gpu_preflight_accepts_runners_dir() {
        let cli = parse(&["--vm-count", "3", "gpu-preflight", "--runners-dir", "/r"]);
        assert!(matches!(cli.command, Commands::GpuPreflight { .. }));
    }

    // ── Steam clean-logins ─────────────────────────────────────────────

    #[test]
    fn steam_clean_logins_parses() {
        let cli = parse(&["--vm-count", "3", "steam", "clean-logins"]);
        assert!(matches!(
            cli.command,
            Commands::Steam {
                action: SteamAction::CleanLogins { .. }
            }
        ));
    }

    #[test]
    fn steam_clean_logins_accepts_target_and_state_dir() {
        let cli = parse(&[
            "--vm-count",
            "3",
            "steam",
            "clean-logins",
            "vm-2",
            "--login-state-dir",
            "/srv/logins",
        ]);
        assert!(matches!(
            cli.command,
            Commands::Steam {
                action: SteamAction::CleanLogins { .. }
            }
        ));
    }

    // ── Watch / Doctor ─────────────────────────────────────────────────

    #[test]
    fn watch_parses() {
        let cli = parse(&["--vm-count", "3", "watch"]);
        assert!(matches!(cli.command, Commands::Watch { .. }));
    }

    #[test]
    fn watch_accepts_interval() {
        let cli = parse(&["--vm-count", "3", "watch", "--interval", "10"]);
        assert!(matches!(cli.command, Commands::Watch { .. }));
    }

    #[test]
    fn doctor_parses() {
        let cli = parse(&["doctor"]);
        assert!(matches!(cli.command, Commands::Doctor { .. }));
    }

    #[test]
    fn doctor_accepts_fix() {
        let cli = parse(&["doctor", "--fix"]);
        assert!(matches!(cli.command, Commands::Doctor { .. }));
    }

    // ── Init / YhConfig / Mcp / Completions ────────────────────────────

    #[test]
    fn init_parses() {
        let cli = parse(&["init"]);
        assert!(matches!(cli.command, Commands::Init));
    }

    #[test]
    fn yh_config_parses() {
        let cli = parse(&["yh-config"]);
        assert!(matches!(cli.command, Commands::YhConfig { .. }));
    }

    #[test]
    fn yh_config_accepts_force() {
        let cli = parse(&["yh-config", "--force"]);
        assert!(matches!(cli.command, Commands::YhConfig { .. }));
    }

    #[test]
    fn mcp_parses() {
        let cli = parse(&["mcp"]);
        assert!(matches!(cli.command, Commands::Mcp));
    }

    #[test]
    fn completions_parses() {
        let cli = parse(&["completions", "bash"]);
        assert!(matches!(cli.command, Commands::Completions { .. }));
    }

    #[test]
    fn completions_accepts_all_shells() {
        for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
            let cli = parse(&["completions", shell]);
            assert!(
                matches!(cli.command, Commands::Completions { .. }),
                "shell={shell}"
            );
        }
    }

    // ── Cleanup / Admission subcommands ─────────────────────────────────

    #[test]
    fn cleanup_parses() {
        let cli = parse(&["--vm-count", "3", "cleanup"]);
        assert!(matches!(cli.command, Commands::Cleanup { .. }));
    }

    #[test]
    fn cleanup_accepts_reset_and_dry_run() {
        let cli = parse(&["--vm-count", "3", "cleanup", "--reset", "--dry-run"]);
        assert!(matches!(cli.command, Commands::Cleanup { .. }));
    }

    #[test]
    fn admission_status_parses() {
        let cli = parse(&["admission", "status"]);
        assert!(matches!(cli.command, Commands::Admission { .. }));
    }

    // ── needs_vm_count exhaustive ───────────────────────────────────────

    #[test]
    fn needs_vm_count_exempts_correct_commands() {
        // Commands that do NOT need --vm-count
        assert!(!parse(&["admission", "status"]).command.needs_vm_count());
        assert!(
            !parse(&["steam", "guard", "vm-1"]).command.needs_vm_count(),
            "steam guard"
        );
        assert!(
            !parse(&["steam", "refresh"]).command.needs_vm_count(),
            "steam refresh"
        );
        assert!(
            !parse(&["steam", "info"]).command.needs_vm_count(),
            "steam info"
        );
        assert!(
            !parse(&["netem", "vm-1", "--latency", "50"])
                .command
                .needs_vm_count(),
            "netem"
        );

        // Commands that DO need --vm-count (confirm the others are NOT exempt)
        assert!(
            parse(&["completions", "bash"]).command.needs_vm_count(),
            "completions"
        );
        assert!(parse(&["mcp"]).command.needs_vm_count(), "mcp");
        assert!(parse(&["init"]).command.needs_vm_count(), "init");
        assert!(parse(&["yh-config"]).command.needs_vm_count(), "yh-config");
        assert!(parse(&["doctor"]).command.needs_vm_count(), "doctor");
        assert!(parse(&["net-up"]).command.needs_vm_count(), "net-up");
        assert!(parse(&["net-down"]).command.needs_vm_count(), "net-down");
    }

    #[test]
    fn needs_vm_count_requires_for_most_commands() {
        // All other commands REQUIRE --vm-count
        assert!(parse(&["--vm-count", "3", "up"]).command.needs_vm_count());
        assert!(parse(&["--vm-count", "3", "down"]).command.needs_vm_count());
        assert!(
            parse(&["--vm-count", "3", "restart"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "reset", "vm-3", "--home"])
                .command
                .needs_vm_count()
        );
        assert!(parse(&["net-up"]).command.needs_vm_count());
        assert!(parse(&["net-down"]).command.needs_vm_count());
        assert!(
            parse(&["--vm-count", "3", "deploy"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "steam", "login"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "steam", "check"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "steam", "start"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "steam", "accounts"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "steam", "clean-logins"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "steam", "warm"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "gpu-preflight"])
                .command
                .needs_vm_count()
        );
        assert!(parse(&["--vm-count", "3", "run"]).command.needs_vm_count());
        assert!(
            parse(&["--vm-count", "3", "stop-game"])
                .command
                .needs_vm_count()
        );
        assert!(parse(&["--vm-count", "3", "logs"]).command.needs_vm_count());
        assert!(parse(&["--vm-count", "3", "test"]).command.needs_vm_count());
        assert!(
            parse(&["--vm-count", "3", "bisect", "--good", "a", "--bad", "b"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "netem-show"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "netem-reset"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "history"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "snapshot-save", "s1"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "snapshot-restore", "s1"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "snapshot-list"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "snapshot-delete", "s1"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "screenshot"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "visual", "list"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "visual", "capture", "main"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "visual", "record", "main"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "visual", "bless", "main", "--from", "r1"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "visual", "diff", "main", "--from", "r1"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "visual", "report"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "watch"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "doctor"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "compositor-status"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "status"])
                .command
                .needs_vm_count()
        );
        assert!(
            parse(&["--vm-count", "3", "cleanup"])
                .command
                .needs_vm_count()
        );
        // Init doesn't need vm_count
        assert!(parse(&["--vm-count", "3", "init"]).command.needs_vm_count());
        assert!(
            parse(&["--vm-count", "3", "yh-config"])
                .command
                .needs_vm_count()
        );
    }

    // ── Global flag interactions ───────────────────────────────────────

    #[test]
    fn global_project_root_flag_parses() {
        let cli = parse(&["--project-root", "/proj", "--vm-count", "3", "status"]);
        assert_eq!(cli.project_root, Some(std::path::PathBuf::from("/proj")));
    }

    #[test]
    fn global_ssh_key_flag_parses() {
        let cli = parse(&["--ssh-key", "/my/key", "--vm-count", "3", "status"]);
        assert_eq!(cli.ssh_key, Some(std::path::PathBuf::from("/my/key")));
    }

    #[test]
    fn global_state_dir_flag_parses() {
        let cli = parse(&["--state-dir", "/tmp/state", "--vm-count", "3", "status"]);
        assert_eq!(cli.state_dir, Some(std::path::PathBuf::from("/tmp/state")));
    }

    #[test]
    fn global_lock_dir_flag_parses() {
        let cli = parse(&["--lock-dir", "/tmp/locks", "--vm-count", "3", "status"]);
        assert_eq!(cli.lock_dir, Some(std::path::PathBuf::from("/tmp/locks")));
    }

    #[test]
    fn global_credentials_flag_parses() {
        let cli = parse(&[
            "--credentials",
            "/tmp/creds.toml",
            "--vm-count",
            "3",
            "status",
        ]);
        assert_eq!(
            cli.credentials,
            Some(std::path::PathBuf::from("/tmp/creds.toml"))
        );
    }

    #[test]
    fn global_non_interactive_flag_parses() {
        let cli = parse(&["--non-interactive", "--vm-count", "3", "status"]);
        assert!(cli.non_interactive);
    }

    #[test]
    fn global_no_gui_flag_parses() {
        let cli = parse(&["--no-gui", "--vm-count", "3", "status"]);
        assert!(cli.no_gui);
    }

    #[test]
    fn global_backend_flag_parses() {
        let cli = parse(&["--backend", "docker", "--vm-count", "3", "status"]);
        assert_eq!(cli.backend, Some(crate::config::BackendKind::Docker));
    }

    #[test]
    fn global_flags_work_across_commands() {
        // Verify global flags parse correctly with different subcommands
        for cmd in &["status", "down", "logs", "stop-game"] {
            let cli = parse(&[
                "--project-root",
                "/p",
                "--state-dir",
                "/s",
                "--lock-dir",
                "/l",
                "--vm-count",
                "3",
                cmd,
            ]);
            assert_eq!(
                cli.project_root,
                Some(std::path::PathBuf::from("/p")),
                "cmd={cmd}"
            );
            assert_eq!(
                cli.state_dir,
                Some(std::path::PathBuf::from("/s")),
                "cmd={cmd}"
            );
            assert_eq!(
                cli.lock_dir,
                Some(std::path::PathBuf::from("/l")),
                "cmd={cmd}"
            );
        }
    }
}
