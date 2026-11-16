use std::path::{Path, PathBuf};

use mcp_layer::{Layered, Routed, ToolSet, job_tools::JobTools};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::*,
    tool, tool_router,
};

use crate::core::config::{ClusterConfig, Unchecked, detect_project_root};
use crate::core::state;
pub mod runners;

pub const MCP_NON_INTERACTIVE_ENV: &str = "STEAMPIPE_MCP_NON_INTERACTIVE";

#[derive(Clone, Debug)]
pub struct SteampipeCtx {
    pub project_root: PathBuf,
    pub cluster_ctl: PathBuf,
}

pub struct SteampipeTools {
    ctx: SteampipeCtx,
}

pub fn tool_set(ctx: SteampipeCtx) -> impl ToolSet {
    Routed { state: SteampipeTools { ctx }, router: SteampipeTools::tool_router() }
}

pub fn layered(ctx: SteampipeCtx, mut jobs: JobTools) -> Layered {
    jobs.profile_paths.insert(0, ctx.project_root.join("steampipe.toml"));
    runners::register(std::sync::Arc::make_mut(&mut jobs.registry), &ctx)
        .expect("duplicate steampipe runner");
    let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
    info.instructions = Some(
        "Steampipe: cluster orchestration tools for multiplayer game testing. \
         Provides native cluster management (status, logs, deploy, test, stop, history) \
         with self-healing pre-flight checks, and a generic Nix flake runner.".into(),
    );
    Layered::new(info).with(tool_set(ctx)).with(jobs.tool_set())
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
fn build_config(
    project_root: &Path,
    params: &ClusterParams,
) -> Result<(ClusterConfig<Unchecked>, Option<String>), ErrorData> {
    let vm_count = params.vm_count.unwrap_or(match params.cluster.as_str() {
        "1v1" => 1,
        _ => 7,
    });
    let config = ClusterConfig::from_vm_count(project_root, vm_count, Some(&params.cluster), None)
        .map_err(|e| ErrorData::internal_error(format!("Config error: {e}"), None))?;

    state::ensure_state_dir(&config.state_dir)
        .map_err(|e| ErrorData::internal_error(format!("State dir error: {e}"), None))?;

    // Self-heal: clean up stale PIDs, orphaned processes, leftover sockets
    let cleanup_msg = crate::vm::preflight::cleanup_stale_state(&config);

    // Use the active claimed set (must happen after cleanup so stale claims are gone).
    let claimed_vm_ids =
        crate::vm::lease::list_cluster_vms(&config.cluster_name, config.max_vms, &config.lock_dir);
    let config = ClusterConfig::new(project_root, &claimed_vm_ids, Some(&params.cluster), None)
        .map_err(|e| ErrorData::internal_error(format!("Config error: {e}"), None))?;

    Ok((config, cleanup_msg))
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

fn require_claimed_vms(config: &ClusterConfig<Unchecked>, action: &str) -> Result<(), ErrorData> {
    if config.vms.is_empty() {
        return Err(ErrorData::internal_error(
            format!(
                "Cannot {action}: cluster '{}' has no claimed VMs.\n\
                 Start the cluster first: nix run .#cluster-1v1-up\n\
                 Or for tournaments: nix run .#cluster-tournament-up",
                config.cluster_name
            ),
            None,
        ));
    }
    Ok(())
}

// ── Input types ─────────────────────────────────────────────────────────────

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
pub struct ClusterStopInput {
    #[serde(flatten)]
    pub cluster: ClusterParams,
    /// Also kill Steam and Weston processes (default: false).
    #[serde(default)]
    pub kill_steam: Option<bool>,
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
impl SteampipeTools {
    // ── Cluster tools ───────────────────────────────────────────────────

    #[tool(
        description = "Check status of the VMs currently leased by this cluster. Shows SSH reachability, whether the game/Steam/Weston processes are running, and which cluster holds the VM lease."
    )]
    async fn cluster_status(
        &self,
        Parameters(input): Parameters<ClusterStatusInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, cleanup_msg) = build_config(&self.ctx.project_root, &input.cluster)?;
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
            let held_by = match crate::vm::lease::probe_holder(vm_index, &config.lock_dir) {
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

        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        description = "Read game logs from cluster VMs. Can target a specific VM or all VMs. Supports head/tail mode and regex filtering."
    )]
    async fn cluster_logs(
        &self,
        Parameters(input): Parameters<ClusterLogsInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, _cleanup_msg) = build_config(&self.ctx.project_root, &input.cluster)?;
        require_bridge(&config)?;
        require_claimed_vms(&config, "read logs")?;
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

        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        description = "Kill the game process on all cluster VMs. Optionally also kill Steam and Weston processes."
    )]
    async fn cluster_stop(
        &self,
        Parameters(input): Parameters<ClusterStopInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, cleanup_msg) = build_config(&self.ctx.project_root, &input.cluster)?;
        require_bridge(&config)?;
        require_claimed_vms(&config, "stop games")?;
        let kill_steam = input.kill_steam.unwrap_or(false);

        crate::run::kill_games(&config.backend, &config.vms, &config.binary_name).await;

        let mut text = String::new();
        if let Some(msg) = cleanup_msg {
            text.push_str(&msg);
            text.push_str("\n\n");
        }
        text.push_str(&format!("Game stopped on {} VM(s)", config.vms.len()));

        if kill_steam {
            let vm_user = config.vm_user.clone();
            crate::config::par_each_vm(&config.vms, |vm| {
                let backend = config.backend.clone();
                let vm_user = vm_user.clone();
                async move {
                    let _ =
                        crate::game::steam::graceful_stop_steam(&backend, &vm.ip, &vm_user).await;
                    backend
                        .run_cmd(
                            &vm.ip,
                            "pkill -x weston 2>/dev/null; pkill -x sway 2>/dev/null; true",
                        )
                        .await;
                }
            })
            .await
            .map_err(|e| ErrorData::internal_error(format!("Failed to stop Steam: {e}"), None))?;
            text.push_str("\nSteam and Weston stopped");
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        description = "Show test result history for the cluster. Displays pass/fail rates, failure code breakdown, and trends across test sessions."
    )]
    fn cluster_history(
        &self,
        Parameters(input): Parameters<ClusterHistoryInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let (config, _cleanup_msg) = build_config(&self.ctx.project_root, &input.cluster)?;
        let history = crate::history::load(&config.state_dir);

        if history.results.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
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
            "{:<20} {:<7} {:<4} {:<4} {:<6} {:<6} {:<6} {:<12} {:<10}\n",
            "Timestamp", "Net", "P", "VMs", "Pass", "Fail", "T/O", "Visual", "Result"
        );
        text.push_str(&"─".repeat(86));
        text.push('\n');

        for r in results {
            let result_str = if r.failed == 0 { "PASS" } else { "FAIL" };
            let visual = crate::history::summarize_visual_results(&r.visual_results)
                .map(|summary| {
                    if summary.failed == 0 {
                        format!("PASS ({}/{})", summary.passed, summary.passed)
                    } else {
                        format!(
                            "FAIL ({}/{})",
                            summary.failed,
                            summary.passed + summary.failed
                        )
                    }
                })
                .unwrap_or_else(|| "─".to_string());
            text.push_str(&format!(
                "{:<20} {:<7} {:<4} {:<4} {:<6} {:<6} {:<6} {:<12} {:<10}\n",
                r.timestamp,
                r.network,
                r.players,
                r.vm_count,
                r.passed,
                r.failed,
                r.timed_out,
                visual,
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
            codes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            for (code, count) in codes {
                text.push_str(&format!(
                    "\n  exit {code} ({}): {count}x",
                    crate::test::exit_code_label(code, &config.exit_codes)
                ));
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

pub async fn serve() -> anyhow::Result<()> {
    let project_root = detect_project_root()?;
    let jobs = JobTools {
        jobs: mcp_layer::jobs::Jobs::new(
            project_root.join("logs/mcp-jobs"),
            std::env::current_exe()?,
        ),
        registry: std::sync::Arc::default(),
        profile_paths: vec![],
    };
    layered(SteampipeCtx { cluster_ctl: std::env::current_exe()?, project_root }, jobs).serve_stdio().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn layered_tools_and_history_use_context_root() {
        use rmcp::{ServiceExt, model::CallToolRequestParams};

        let dir = tempfile::tempdir().unwrap();
        assert_ne!(std::env::current_dir().unwrap(), dir.path());
        std::fs::write(
            dir.path().join("steampipe.toml"),
            format!(
                "binary_name='fixture'\ncargo_package='fixture'\nvm_user='fixture'\n\
                 remote_dir='/home/fixture/work'\nlock_dir='{}'\n[cluster]\nstate_dir='{}'\n",
                dir.path().join("locks").display(), dir.path().join("state").display(),
            ),
        ).unwrap();
        let jobs = JobTools {
            jobs: mcp_layer::jobs::Jobs::new(dir.path().join("jobs"), "unused"),
            registry: std::sync::Arc::default(),
            profile_paths: vec![],
        };
        let ctx = SteampipeCtx { project_root: dir.path().to_owned(), cluster_ctl: "cluster-ctl".into() };
        let (server_io, client_io) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            layered(ctx, jobs).serve(server_io).await.unwrap().waiting().await.unwrap();
        });
        let client = ().serve(client_io).await.unwrap();
        let names: Vec<_> = client.list_all_tools().await.unwrap().into_iter()
            .map(|tool| tool.name.to_string()).collect();
        assert_eq!(names, [
            "cluster_history", "cluster_logs", "cluster_status", "cluster_stop",
            "job_cancel", "job_list", "job_logs", "job_profiles", "job_start", "job_watch",
        ]);
        let response = client.call_tool(CallToolRequestParams::new("cluster_history")
            .with_arguments(serde_json::Map::new())).await.unwrap();
        assert_eq!(response.content[0].as_text().unwrap().text, "No test history found.");
        client.cancel().await.unwrap();
        server.await.unwrap();
    }

    // ── default_cluster ─────────────────────────────────────────────────

    #[test]
    fn default_cluster_is_1v1() {
        assert_eq!(default_cluster(), "1v1");
    }

    // ── require_claimed_vms ─────────────────────────────────────────────

    #[test]
    fn require_claimed_vms_ok_when_present() {
        let config = ClusterConfig::for_test(2);
        assert!(require_claimed_vms(&config, "test").is_ok());
    }

    #[test]
    fn require_claimed_vms_errors_when_empty() {
        let config = ClusterConfig::for_test(0);
        let err = require_claimed_vms(&config, "deploy").unwrap_err();
        assert!(err.message.contains("deploy"), "{}", err.message);
    }

    // ── ClusterParams ───────────────────────────────────────────────────

    #[test]
    fn cluster_params_default_vm_count_1v1() {
        let params = ClusterParams {
            cluster: "1v1".into(),
            vm_count: None,
        };
        assert_eq!(params.cluster, "1v1");
    }

    #[test]
    fn cluster_params_default_vm_count_tournament() {
        let params: ClusterParams = serde_json::from_str(r#"{"cluster": "tournament"}"#).unwrap();
        assert_eq!(params.cluster, "tournament");
        assert_eq!(params.vm_count, None);
    }
}
