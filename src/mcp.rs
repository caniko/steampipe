use std::process::Command;
use std::time::Duration;

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};

use crate::cli::NetworkMode;
use crate::config::{ClusterConfig, Unchecked, detect_project_root};
use crate::state;

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Clone)]
pub struct SteampipeMcp {
    tool_router: ToolRouter<Self>,
}

impl SteampipeMcp {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

// ── Shared types ────────────────────────────────────────────────────────────

fn default_cluster() -> String {
    "1v1".into()
}

/// Common cluster parameters accepted by all cluster tools.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClusterParams {
    /// Cluster name: "1v1" or "tournament" (default: "1v1").
    #[serde(default = "default_cluster")]
    pub cluster: String,
    /// Number of VMs. Derived from cluster if omitted (1v1→1, tournament→7).
    #[serde(default)]
    pub vm_count: Option<u8>,
}

/// Build a ClusterConfig from MCP tool parameters.
/// Also runs pre-flight cleanup (stale PIDs, orphaned processes, leftover sockets).
/// Returns the config and an optional cleanup summary message.
fn build_config(params: &ClusterParams) -> Result<(ClusterConfig<Unchecked>, Option<String>), ErrorData> {
    let project_root = detect_project_root()
        .map_err(|e| ErrorData::internal_error(format!("Cannot find project root: {e}"), None))?;
    let vm_count = params.vm_count.unwrap_or(match params.cluster.as_str() {
        "1v1" => 1,
        _ => 7,
    });
    let mut config =
        ClusterConfig::new(&project_root, vm_count, Some(&params.cluster), None).map_err(|e| {
            ErrorData::internal_error(format!("Config error: {e}"), None)
        })?;

    state::ensure_state_dir(&config.state_dir)
        .map_err(|e| ErrorData::internal_error(format!("State dir error: {e}"), None))?;

    // Self-heal: clean up stale PIDs, orphaned processes, leftover sockets
    let cleanup_msg = crate::preflight::cleanup_stale_state(&config);

    // Prefer leased VMs (those actually running) over the static 1..N list
    // (must happen after cleanup so stale claims are already gone)
    config.vms = config.leased_vms();

    Ok((config, cleanup_msg))
}

/// Get project_root, needed by test and deploy tools.
fn project_root() -> Result<std::path::PathBuf, ErrorData> {
    detect_project_root()
        .map_err(|e| ErrorData::internal_error(format!("Cannot find project root: {e}"), None))
}

/// Fail fast if the network bridge is down — avoids SSH hangs when VMs are unreachable.
fn require_bridge(config: &ClusterConfig<Unchecked>) -> Result<(), ErrorData> {
    if !crate::config::bridge_exists(&config.bridge) {
        return Err(ErrorData::internal_error(
            format!(
                "Bridge '{}' is not up — VMs are unreachable.\n\
                 Run: sudo cluster-ctl net-up\n\
                 Or:  nix run .#cluster-net-up",
                config.bridge
            ),
            None,
        ));
    }
    Ok(())
}

// ── Input types ─────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NixRunInput {
    /// Flake output target (e.g. "cluster-1v1-up", "cluster-tournament-test").
    pub target: String,
    /// Extra arguments passed after `--` to the flake app.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Flake reference (default: ".").
    #[serde(default)]
    pub flake: Option<String>,
    /// Timeout in seconds (default: 300).
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClusterStatusInput {
    #[serde(flatten)]
    pub cluster: ClusterParams,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClusterLogsInput {
    #[serde(flatten)]
    pub cluster: ClusterParams,
    /// Specific VM target (e.g. "vm-1"). Omit for all VMs.
    #[serde(default)]
    pub vm: Option<String>,
    /// Max lines per VM (default: 100).
    #[serde(default)]
    pub max_lines: Option<u32>,
    /// Read from end (default). Set to false for head mode.
    #[serde(default)]
    pub tail: Option<bool>,
    /// Regex pattern to filter log lines.
    #[serde(default)]
    pub pattern: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClusterDeployInput {
    #[serde(flatten)]
    pub cluster: ClusterParams,
    /// Build the binary before deploying (default: true).
    #[serde(default)]
    pub build: Option<bool>,
    /// Verify deployment checksums after deploy (default: false).
    #[serde(default)]
    pub verify: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClusterStopInput {
    #[serde(flatten)]
    pub cluster: ClusterParams,
    /// Also kill Steam and Weston processes (default: false).
    #[serde(default)]
    pub kill_steam: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClusterTestInput {
    #[serde(flatten)]
    pub cluster: ClusterParams,
    /// Network transport: "lan" (default) or "steam".
    #[serde(default)]
    pub network: Option<String>,
    /// Number of players, 2-8 (default: 2 for 1v1, 8 for tournament).
    #[serde(default)]
    pub players: Option<u8>,
    /// Number of test runs (default: 1, max: 50).
    #[serde(default)]
    pub max_runs: Option<u32>,
    /// Timeout per run in seconds (default: 300).
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Grace period (seconds) between SIGTERM and SIGKILL (default: 5).
    #[serde(default)]
    pub shutdown_timeout: Option<u64>,
    /// Stop after first failure (default: true).
    #[serde(default)]
    pub stop_on_failure: Option<bool>,
    /// Deploy before testing (default: true).
    #[serde(default)]
    pub deploy: Option<bool>,
    /// Build before deploying (default: true).
    #[serde(default)]
    pub build: Option<bool>,
    /// VM display mode: "headless", "weston" (default), "sway", "weston-gpu", or "sway-gpu".
    #[serde(default)]
    pub display: Option<String>,
    /// Capture screenshots on failure (default: false).
    #[serde(default)]
    pub capture_on_failure: Option<bool>,
    /// Regex filter for output lines.
    #[serde(default)]
    pub filter_pattern: Option<String>,
    /// Write full output to this file.
    #[serde(default)]
    pub output_file: Option<String>,
    /// Extra args for the host game binary.
    #[serde(default)]
    pub host_args: Option<String>,
    /// Extra args for VM game binaries.
    #[serde(default)]
    pub vm_args: Option<String>,
    /// Launch the local host process under strace (default: false).
    #[serde(default)]
    pub strace: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClusterHistoryInput {
    #[serde(flatten)]
    pub cluster: ClusterParams,
    /// Show only the last N results.
    #[serde(default)]
    pub last: Option<usize>,
}

// ── Tool implementations ────────────────────────────────────────────────────

#[tool_router]
impl SteampipeMcp {
    #[tool(
        description = "Run a Nix flake app. Executes `nix run <flake>#<target> -- <args...>`. Use for any `nix run .#<target>` command."
    )]
    fn nix_run(
        &self,
        Parameters(input): Parameters<NixRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let flake = input.flake.as_deref().unwrap_or(".");
        let flake_ref = format!("{flake}#{}", input.target);
        let timeout_secs = input.timeout.unwrap_or(300);

        let mut cmd = Command::new("nix");
        cmd.arg("run").arg(&flake_ref);

        if let Some(ref args) = input.args {
            if !args.is_empty() {
                cmd.arg("--");
                cmd.args(args);
            }
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            ErrorData::internal_error(format!("Failed to spawn nix run: {e}"), None)
        })?;

        let output = wait_with_timeout(child, Duration::from_secs(timeout_secs)).map_err(|e| {
            ErrorData::internal_error(format!("nix run failed: {e}"), None)
        })?;

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let text = strip_ansi(&combined);

        if output.status.success() {
            Ok(CallToolResult::success(vec![Content::text(text)]))
        } else {
            let code = output
                .status
                .code()
                .map(|c| format!(" (exit code {c})"))
                .unwrap_or_default();
            Ok(CallToolResult::success(vec![Content::text(format!(
                "nix run {flake_ref} failed{code}:\n{text}"
            ))]))
        }
    }

    // ── Cluster tools ───────────────────────────────────────────────────

    #[tool(
        description = "Check status of all VMs in the cluster. Shows SSH reachability, whether the game/Steam/Weston processes are running, and which cluster holds the VM lease."
    )]
    async fn cluster_status(
        &self,
        Parameters(input): Parameters<ClusterStatusInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, cleanup_msg) = build_config(&input.cluster)?;
        require_bridge(&config)?;

        let statuses = crate::status::poll_vm_statuses(
            &config.vms,
            &config.backend,
            &config.state_dir,
            &config.binary_name,
        )
        .await;

        let mut text = String::new();
        if let Some(msg) = cleanup_msg {
            text.push_str(&msg);
            text.push_str("\n\n");
        }
        text.push_str(&format!(
            "{:<8} {:<14} {:<16} {:<6} {:<6} {:<8} {:<8}\n",
            "VM", "IP", "Held By", "SSH", "VM", "Steam", "Game"
        ));
        text.push_str(&"─".repeat(66));
        text.push('\n');

        for s in &statuses {
            let vm_index: u8 = s
                .name
                .strip_prefix("vm-")
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let held_by = match crate::lease::probe_holder(vm_index, &config.lock_dir) {
                Some(info) => info.cluster,
                None => "(free)".to_string(),
            };
            text.push_str(&format!(
                "{:<8} {:<14} {:<16} {:<6} {:<6} {:<8} {:<8}\n",
                s.name,
                s.ip,
                held_by,
                if s.ssh_ok { "OK" } else { "─" },
                if s.vm_running { "UP" } else { "─" },
                if s.steam_running { "OK" } else { "─" },
                if s.game_running { "OK" } else { "─" },
            ));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Read game logs from cluster VMs. Can target a specific VM or all VMs. Supports head/tail mode and regex filtering."
    )]
    async fn cluster_logs(
        &self,
        Parameters(input): Parameters<ClusterLogsInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, _cleanup_msg) = build_config(&input.cluster)?;
        require_bridge(&config)?;
        let lines = input.max_lines.unwrap_or(100);
        let head = !input.tail.unwrap_or(true);

        let results = crate::logs::collect_logs(
            &config,
            input.vm.as_deref(),
            lines,
            head,
            input.pattern.as_deref(),
        )
        .await
        .map_err(|e| ErrorData::internal_error(format!("Failed to collect logs: {e}"), None))?;

        let mut text = String::new();
        for (name, output) in &results {
            let trimmed = output.trim();
            if trimmed.is_empty() {
                text.push_str(&format!("── {name} ── (no output)\n\n"));
            } else {
                text.push_str(&format!("── {name} ──\n{trimmed}\n\n"));
            }
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Build and deploy the game binary + assets to cluster VMs. Uses content-addressable pool with SHA-256 verification to skip unchanged binaries."
    )]
    async fn cluster_deploy(
        &self,
        Parameters(input): Parameters<ClusterDeployInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, _cleanup_msg) = build_config(&input.cluster)?;
        require_bridge(&config)?;
        let root = project_root()?;
        let do_build = input.build.unwrap_or(true);
        let do_verify = input.verify.unwrap_or(false);

        // Build
        if do_build {
            crate::deploy::build_release(&root, &config.cargo_package)
                .await
                .map_err(|e| ErrorData::internal_error(format!("Build failed: {e}"), None))?;
        }

        // Deploy (verbose=false to avoid stdout corruption)
        let results = crate::deploy::deploy_to_vms(
            &config.backend,
            &config.vms,
            &config,
            &root,
            false,
        )
        .await
        .map_err(|e| ErrorData::internal_error(format!("Deploy failed: {e}"), None))?;

        let mut text = String::new();
        let mut failed = 0;
        for r in &results {
            if let Some(ref err) = r.error {
                text.push_str(&format!("  FAILED: {err}\n"));
                failed += 1;
            } else {
                let status = if r.cached {
                    "deployed (binary cached)"
                } else {
                    "deployed"
                };
                text.push_str(&format!("  {}: {status}\n", r.vm_name));
            }
        }

        if failed > 0 {
            text.push_str(&format!("\n{failed} VM(s) failed deployment"));
        } else {
            text.push_str(&format!("\nAll {} VM(s) deployed successfully", results.len()));
        }

        // Verify
        if do_verify && failed == 0 {
            // verify prints to stdout, so we skip it in MCP context
            // and instead do a basic hash check
            text.push_str("\n(Verification requested but not yet supported in MCP mode)");
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Kill the game process on all cluster VMs. Optionally also kill Steam and Weston processes."
    )]
    async fn cluster_stop(
        &self,
        Parameters(input): Parameters<ClusterStopInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, cleanup_msg) = build_config(&input.cluster)?;
        require_bridge(&config)?;
        let kill_steam = input.kill_steam.unwrap_or(false);

        crate::run::kill_games(&config.backend, &config.vms, &config.binary_name).await;

        let mut text = String::new();
        if let Some(msg) = cleanup_msg {
            text.push_str(&msg);
            text.push_str("\n\n");
        }
        text.push_str(&format!("Game stopped on {} VM(s)", config.vms.len()));

        if kill_steam {
            crate::config::par_each_vm(&config.vms, |vm| {
                let backend = config.backend.clone();
                async move {
                    backend
                        .run_cmd(
                            &vm.ip,
                            "pkill -x steam 2>/dev/null; pkill -x weston 2>/dev/null; pkill -x sway 2>/dev/null; true",
                        )
                        .await;
                }
            })
            .await
            .map_err(|e| ErrorData::internal_error(format!("Failed to stop Steam: {e}"), None))?;
            text.push_str("\nSteam and Weston stopped");
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Run end-to-end test on the VM cluster. Handles: deploy, Steam setup, game launch on VMs, heartbeat monitoring, log collection, and pass/fail reporting. Supports LAN (UDP) or Steam networking. Returns PASS/FAIL per run with failure codes and VM log excerpts."
    )]
    async fn cluster_test(
        &self,
        Parameters(input): Parameters<ClusterTestInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, _cleanup_msg) = build_config(&input.cluster)?;
        require_bridge(&config)?;
        let root = project_root()?;

        let network = match input.network.as_deref().unwrap_or("lan") {
            "lan" => NetworkMode::Lan,
            "steam" => NetworkMode::Steam,
            other => {
                return Err(ErrorData::invalid_params(
                    format!("network must be \"lan\" or \"steam\", got \"{other}\""),
                    None,
                ))
            }
        };

        let default_players = match input.cluster.cluster.as_str() {
            "1v1" => 2,
            _ => 8,
        };

        let display = match input.display.as_deref().unwrap_or("weston") {
            "headless" => crate::cli::DisplayMode::Headless,
            "weston" => crate::cli::DisplayMode::Weston,
            "sway" => crate::cli::DisplayMode::Sway,
            "weston-gpu" => crate::cli::DisplayMode::WestonGpu,
            "sway-gpu" => crate::cli::DisplayMode::SwayGpu,
            other => {
                return Err(ErrorData::invalid_params(
                    format!(
                        "display must be \"headless\", \"weston\", \"sway\", \"weston-gpu\", or \"sway-gpu\", got \"{other}\""
                    ),
                    None,
                ));
            }
        };

        let test_config = crate::test::TestConfig {
            network,
            players: input.players.unwrap_or(default_players),
            vm_args: input.vm_args,
            host_args: input.host_args,
            max_runs: input.max_runs.unwrap_or(1).min(50),
            timeout: Duration::from_secs(input.timeout.unwrap_or(300)),
            shutdown_timeout: Duration::from_secs(input.shutdown_timeout.unwrap_or(5)),
            stop_on_failure: input.stop_on_failure.unwrap_or(true),
            deploy: input.deploy.unwrap_or(true),
            build: input.build.unwrap_or(true),
            filter_pattern: input.filter_pattern,
            output_file: input.output_file.map(std::path::PathBuf::from),
            capture_on_failure: input.capture_on_failure.unwrap_or(false),
            display,
            verbose: false, // suppress println to avoid corrupting MCP transport
            chaos: None,
            heartbeat_stall_secs: config.heartbeat_stall_secs,
            output_format: crate::output::OutputFormat::Text,
            on_complete: None,
            on_failure: None,
            exit_codes: config.exit_codes.clone(),
            strace: input.strace.unwrap_or(false),
        };

        let result = crate::test::run(&config, &root, test_config).await;

        match result {
            Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
            Err(e) => {
                // test::run returns Err with the output embedded in the error message
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "{e}"
                ))]))
            }
        }
    }

    #[tool(
        description = "Show test result history for the cluster. Displays pass/fail rates, failure code breakdown, and trends across test sessions."
    )]
    fn cluster_history(
        &self,
        Parameters(input): Parameters<ClusterHistoryInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, _cleanup_msg) = build_config(&input.cluster)?;
        let history = crate::history::load(&config.state_dir);

        if history.results.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No test history found.",
            )]));
        }

        let results: &[crate::history::TestResult] = if let Some(n) = input.last {
            let start = history.results.len().saturating_sub(n);
            &history.results[start..]
        } else {
            &history.results
        };

        let mut text = format!(
            "{:<20} {:<7} {:<4} {:<4} {:<6} {:<6} {:<6} {:<10}\n",
            "Timestamp", "Net", "P", "VMs", "Pass", "Fail", "T/O", "Result"
        );
        text.push_str(&"─".repeat(70));
        text.push('\n');

        for r in results {
            let result_str = if r.failed == 0 { "PASS" } else { "FAIL" };
            text.push_str(&format!(
                "{:<20} {:<7} {:<4} {:<4} {:<6} {:<6} {:<6} {:<10}\n",
                r.timestamp,
                r.network,
                r.players,
                r.vm_count,
                r.passed,
                r.failed,
                r.timed_out,
                result_str,
            ));
        }

        // Summary statistics
        let total_runs: u32 = results.iter().map(|r| r.passed + r.failed).sum();
        let total_passed: u32 = results.iter().map(|r| r.passed).sum();
        let total_failed: u32 = results.iter().map(|r| r.failed).sum();
        let sessions = results.len();
        let pass_rate = if total_runs > 0 {
            total_passed as f64 / total_runs as f64 * 100.0
        } else {
            0.0
        };

        text.push_str(&format!(
            "\n{sessions} session(s), {total_runs} run(s): {total_passed} passed, {total_failed} failed ({pass_rate:.1}% pass rate)"
        ));

        // Failure code breakdown
        let mut code_counts = std::collections::HashMap::new();
        for r in results {
            for c in r.exit_codes.iter().flatten() {
                if *c != 0 {
                    *code_counts.entry(*c).or_insert(0u32) += 1;
                }
            }
        }
        if !code_counts.is_empty() {
            text.push_str("\n\nFailure breakdown:");
            let mut codes: Vec<_> = code_counts.into_iter().collect();
            codes.sort_by(|a, b| b.1.cmp(&a.1));
            for (code, count) in codes {
                text.push_str(&format!(
                    "\n  exit {code} ({}): {count}x",
                    crate::test::exit_code_label(code, &config.exit_codes)
                ));
            }
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

fn wait_with_timeout(
    child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    use std::sync::mpsc;
    use std::thread;

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result.map_err(|e| format!("IO error: {e}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "timed out after {}s",
            timeout.as_secs()
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("child process thread panicked".into()),
    }
}

#[tool_handler]
impl ServerHandler for SteampipeMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Steampipe: cluster orchestration tools for multiplayer game testing. \
                 Provides native cluster management (status, logs, deploy, test, stop, history) \
                 with self-healing pre-flight checks, and a generic Nix flake runner."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

pub async fn serve() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    // Log to stderr so stdout stays clean for MCP JSON-RPC protocol
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let service = SteampipeMcp::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
