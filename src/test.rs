//! End-to-end test orchestration: deploy, launch, monitor, collect logs, report.
//!
//! The test loop deploys the game binary to VMs, launches a local host process,
//! starts game instances on VMs, monitors for heartbeat stalls and timeouts,
//! and produces a pass/fail summary with failure code breakdown.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::backend::Backend;
use crate::cli::NetworkMode;
use crate::config::{ChaosProfile, ClusterConfig, IpAddr, VmName};
use crate::output::{OutputFormat, RunResult, RunStatus, TestSession};

/// Configuration for a test run.
///
/// Created from CLI arguments in `main.rs` and passed to [`run`].
pub struct TestConfig {
    pub network: NetworkMode,
    pub players: u8,
    pub vm_args: Option<String>,
    pub host_args: Option<String>,
    pub max_runs: u32,
    pub timeout: Duration,
    pub shutdown_timeout: Duration,
    pub stop_on_failure: bool,
    pub deploy: bool,
    pub build: bool,
    pub filter_pattern: Option<String>,
    pub output_file: Option<PathBuf>,
    pub capture_on_failure: bool,
    /// VM display mode: headless (game gets `--headless`), weston, or sway.
    pub display: crate::cli::DisplayMode,
    /// When false, suppress `println!` output (for MCP tools where stdout is JSON-RPC).
    pub verbose: bool,
    /// Chaos profile to apply during tests (netem params).
    pub chaos: Option<ChaosProfile>,
    /// Heartbeat stall threshold in seconds.
    pub heartbeat_stall_secs: u64,
    /// Output format (text, junit, jsonl).
    pub output_format: OutputFormat,
    /// Shell command to run when test session completes.
    pub on_complete: Option<String>,
    /// Shell command to run when any test fails.
    pub on_failure: Option<String>,
    /// Parsed exit code mapping from config.
    pub exit_codes: HashMap<i32, String>,
}

// ── Heartbeat trait: static dispatch for local vs. VM heartbeat sources ──

/// Reads the minimum `timestamp_ms` from a heartbeat source.
/// Monomorphized at each call site for zero-cost dispatch.
trait HeartbeatSource {
    fn min_timestamp(&self) -> Option<u128>;
}

struct LocalHeartbeat<'a> {
    dir: &'a Path,
}

struct VmHeartbeat<'a> {
    rt: &'a tokio::runtime::Handle,
    backend: &'a Backend,
    vms: &'a [(VmName, IpAddr)],
    remote_dir: &'a str,
}

impl HeartbeatSource for LocalHeartbeat<'_> {
    fn min_timestamp(&self) -> Option<u128> {
        let entries = std::fs::read_dir(self.dir).ok()?;
        let mut min_ts: Option<u128> = None;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("game_progress_") && name.ends_with(".json") {
                let content = std::fs::read_to_string(entry.path()).ok();
                if let Some(ts) = content.and_then(|c| parse_min_timestamp_ms(&c)) {
                    min_ts = Some(min_ts.map_or(ts, |prev| prev.min(ts)));
                }
            }
        }
        min_ts
    }
}

impl HeartbeatSource for VmHeartbeat<'_> {
    fn min_timestamp(&self) -> Option<u128> {
        let cmd = format!(
            "cat {}/game_progress_*.json 2>/dev/null || true",
            self.remote_dir
        );
        let mut min_ts: Option<u128> = None;
        for (_, ip) in self.vms {
            let result = self.rt.block_on(self.backend.run_cmd_timeout(
                ip,
                &cmd,
                std::time::Duration::from_secs(5),
            ));
            if let Some(ts) = parse_min_timestamp_ms(&result.stdout) {
                min_ts = Some(min_ts.map_or(ts, |prev| prev.min(ts)));
            }
        }
        min_ts
    }
}

/// Parse all `"timestamp_ms": <number>` values from a string, returning the minimum.
fn parse_min_timestamp_ms(content: &str) -> Option<u128> {
    const MARKER: &str = "\"timestamp_ms\":";
    let mut min_ts: Option<u128> = None;
    let mut search_from = 0;
    while let Some(idx) = content[search_from..].find(MARKER) {
        let abs_idx = search_from + idx + MARKER.len();
        let rest = content[abs_idx..].trim_start();
        if let Some(end) = rest.find(|c: char| !c.is_ascii_digit())
            && let Ok(ts) = rest[..end].parse::<u128>()
        {
            min_ts = Some(min_ts.map_or(ts, |prev| prev.min(ts)));
        }
        search_from = abs_idx;
    }
    min_ts
}

/// Check if a heartbeat source has stalled beyond `stall_secs`.
/// Returns `true` if stalled (caller should kill).
fn check_stall(source: &impl HeartbeatSource, last_ts: &mut Option<u128>, stall_secs: u64, now: u128) -> bool {
    if let Some(ts) = source.min_timestamp() {
        if let Some(prev) = *last_ts
            && ts <= prev
        {
            let stale = now.saturating_sub(ts) / 1000;
            if stale > stall_secs as u128 {
                return true;
            }
        }
        *last_ts = Some(ts);
    }
    false
}

// ── Main test orchestration ──

/// Run the end-to-end test orchestration loop.
pub async fn run<S>(
    config: &ClusterConfig<S>,
    project_root: &Path,
    test_config: TestConfig,
) -> anyhow::Result<String> {
    if test_config.players < 2 || test_config.players > 8 {
        anyhow::bail!("players must be 2-8, got {}", test_config.players);
    }

    let vm_count = (test_config.players as usize)
        .saturating_sub(1)
        .min(config.vms.len());
    let target_vms = &config.vms[..vm_count];

    let filter_re = Regex::new(
        test_config.filter_pattern.as_deref().unwrap_or(
            r"FAIL|PASS|panic|error|bug_report|violation|completed|timeout|killed|summary|Player|disconnected",
        ),
    )?;

    let backend = &config.backend;
    let verbose = test_config.verbose;
    let mut output = String::new();

    output.push_str(&format!(
        "Cluster test: network={}, players={}, vms={vm_count}, max_runs={}\n\n",
        test_config.network, test_config.players, test_config.max_runs,
    ));

    // Step 1: Deploy (reuse deploy module)
    if test_config.deploy {
        print_and_push(&mut output, "=== DEPLOYING ===", verbose);
        if test_config.build {
            crate::deploy::build_release(project_root, &config.cargo_package).await?;
            print_and_push(&mut output, "Build OK", verbose);
        }

        let deploy_results =
            crate::deploy::deploy_to_vms(backend, target_vms, config, project_root, false).await?;
        let failed_count = deploy_results.iter().filter(|r| r.error.is_some()).count();
        if failed_count > 0 {
            anyhow::bail!("Deploy failed for {} VMs", failed_count);
        }
        output.push('\n');
    }

    // Pre-flight: verify VMs have a GPU device if using a GPU display mode
    if test_config.display.requires_gpu() {
        print_and_push(&mut output, "=== GPU CHECK ===", verbose);
        crate::preflight::ensure_vm_gpu(backend, target_vms, test_config.display).await?;
        print_and_push(&mut output, "  All VMs have a DRM device", verbose);
        output.push('\n');
    }

    // Step 2a: Start compositor on VMs if display mode requires it
    if test_config.display != crate::cli::DisplayMode::Headless {
        print_and_push(
            &mut output,
            &format!("=== STARTING {} ON VMs ===", test_config.display),
            verbose,
        );
        let setup_script =
            crate::steam::compositor_setup(test_config.display, &config.vm_user);
        for vm in target_vms {
            let result = backend.run_cmd(&vm.ip, &setup_script).await;
            if result.success {
                print_and_push(
                    &mut output,
                    &format!("  {}: {} ready", vm.name, test_config.display),
                    verbose,
                );
            } else {
                print_and_push(
                    &mut output,
                    &format!("  {}: compositor FAILED — {}", vm.name, result.stderr),
                    verbose,
                );
                anyhow::bail!("Compositor setup failed on {}", vm.name);
            }
        }
        output.push('\n');
    }

    // Step 2b: Ensure Steam on VMs if steam network
    if test_config.network == NetworkMode::Steam {
        print_and_push(&mut output, "=== STARTING STEAM ON VMs ===", verbose);
        // If display is Headless, Steam still needs weston as a compositor
        let compositor = if test_config.display == crate::cli::DisplayMode::Headless {
            crate::steam::weston_setup(&config.vm_user)
        } else {
            // Compositor already running from step 2a — just export the env vars
            String::new()
        };
        for vm in target_vms {
            match crate::steam::ensure_steam(backend, &vm.ip, &config.vm_user, &compositor).await
            {
                Ok(()) => {
                    print_and_push(
                        &mut output,
                        &format!("  {}: Steam ready", vm.name),
                        verbose,
                    )
                }
                Err(e) => {
                    print_and_push(
                        &mut output,
                        &format!("  {}: FAILED — {e}", vm.name),
                        verbose,
                    );
                    anyhow::bail!("Steam setup failed on {}", vm.name);
                }
            }
        }
        output.push('\n');
    }

    // Pre-flight: verify VM→host connectivity (catches missing nftables rules after reboot)
    crate::preflight::ensure_vm_to_host_connectivity(config, backend, target_vms).await?;

    // Apply chaos profile if configured
    if let Some(ref chaos) = test_config.chaos {
        print_and_push(&mut output, "=== APPLYING CHAOS PROFILE ===", verbose);
        for vm in target_vms {
            crate::netem::apply(
                config,
                &vm.name,
                chaos.latency,
                chaos.jitter,
                chaos.loss,
                chaos.rate,
            )?;
        }
        output.push('\n');
    }

    // Step 3: Run loop
    let binary_name = &config.binary_name;
    let binary_path = project_root.join(format!("target/release/{binary_name}"));
    if !binary_path.exists() {
        anyhow::bail!(
            "target/release/{binary_name} not found — run with --build or `cargo build --release -p {binary_name}`"
        );
    }

    // Default args based on network mode when not explicitly provided.
    // Host never gets --headless; VMs get it only in headless display mode.
    let vm_headless = if test_config.display == crate::cli::DisplayMode::Headless {
        " --headless"
    } else {
        ""
    };
    let default_vm_args;
    let default_host_args;
    match test_config.network {
        NetworkMode::Lan => {
            default_host_args = "--auto-host-udp --auto-play".to_string();
            default_vm_args = format!(
                "--auto-join-udp --udp-addr {}:{} --auto-play{vm_headless}",
                config.host_ip, config.udp_port,
            );
        }
        NetworkMode::Steam => {
            default_host_args = "--auto-host-steam --auto-play".to_string();
            let target_flag = crate::accounts::local_steam_id()
                .map(|id| format!(" --steam-target-id {id}"))
                .unwrap_or_default();
            default_vm_args = format!(
                "--auto-join-steam --auto-play{vm_headless}{target_flag}"
            );
        }
    }
    let vm_args = test_config.vm_args.as_deref().unwrap_or(&default_vm_args);
    let host_args = test_config
        .host_args
        .as_deref()
        .unwrap_or(&default_host_args);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut timed_out_count = 0u32;
    let mut exit_codes: Vec<Option<i32>> = Vec::new();
    let mut consecutive_fail_code: Option<i32> = None;
    let mut consecutive_fail_count = 0u32;
    const REPEAT_FAILURE_LIMIT: u32 = 3;
    let session_start = Instant::now();
    let mut session = TestSession::new(
        &test_config.network.to_string(),
        test_config.players,
        vm_count,
    );

    for run_num in 1..=test_config.max_runs {
        let run_start = Instant::now();
        print_and_push(
            &mut output,
            &format!("=== RUN {run_num}/{} ===", test_config.max_runs),
            verbose,
        );

        // Kill existing games
        crate::run::kill_games(backend, target_vms, binary_name).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Launch local (host) process FIRST so it's listening when VMs connect
        eprintln!("[cluster-ctl] Launching local process...");
        let mut local_cmd = std::process::Command::new(&binary_path);
        for arg in host_args.split_whitespace() {
            local_cmd.arg(arg);
        }
        local_cmd.current_dir(project_root);
        local_cmd.env("BEVY_ASSET_ROOT", project_root);
        if let Some(lib_dir) = config.steam_api_lib.parent() {
            let ld_path = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
            let new_ld_path = if ld_path.is_empty() {
                lib_dir.to_string_lossy().into_owned()
            } else {
                format!("{}:{}", lib_dir.display(), ld_path)
            };
            local_cmd.env("LD_LIBRARY_PATH", new_ld_path);
        }
        if verbose {
            local_cmd.stdout(std::process::Stdio::inherit());
            local_cmd.stderr(std::process::Stdio::inherit());
        } else {
            // In MCP mode, stdout is the JSON-RPC transport — must pipe to avoid corruption.
            // Also enables monitor_local_process to capture output for the report.
            local_cmd.stdout(std::process::Stdio::piped());
            local_cmd.stderr(std::process::Stdio::piped());
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            local_cmd.process_group(0);
        }

        let mut child = local_cmd.spawn()?;
        eprintln!("[cluster-ctl] Host process spawned with PID {}", child.id());

        // Wait for host to start listening before launching VMs
        eprintln!("[cluster-ctl] Waiting 3s for host to initialize...");
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Launch game on VMs (joiners)
        eprintln!("[cluster-ctl] Launching game on VMs...");
        let mut vm_launch_ok = true;
        for vm in target_vms {
            eprintln!("[cluster-ctl]   launching on {}...", vm.name);
            if let Err(e) = launch_game(
                backend,
                &vm.ip,
                &config.remote_dir,
                binary_name,
                &config.vm_user,
                &config.log_file,
                vm_args,
            )
            .await
            {
                print_and_push(&mut output, &format!("  {}: launch FAILED: {e}", vm.name), verbose);
                vm_launch_ok = false;
            }
        }

        if !vm_launch_ok {
            failed += 1;
            print_and_push(&mut output, "--- FAIL (VM launch failed) ---", verbose);
            kill_process_group(&mut child, test_config.shutdown_timeout);
            crate::run::kill_games(backend, target_vms, binary_name).await;
            if test_config.stop_on_failure {
                break;
            }
            output.push('\n');
            continue;
        }
        eprintln!("[cluster-ctl] VMs launched OK");

        // Monitor with heartbeat detection
        cleanup_heartbeat_files(project_root);

        let timeout = test_config.timeout;
        let shutdown_timeout = test_config.shutdown_timeout;
        let project_root_owned = project_root.to_owned();
        let vm_ips: Vec<(VmName, IpAddr)> = target_vms
            .iter()
            .map(|vm| (vm.name.clone(), vm.ip.clone()))
            .collect();
        let backend_for_monitor = backend.clone();
        let remote_dir_owned = config.remote_dir.clone();

        let hb_stall = test_config.heartbeat_stall_secs;
        let (combined, exit_code, was_timeout) = tokio::task::spawn_blocking(move || {
            monitor_local_process(
                &mut child,
                timeout,
                shutdown_timeout,
                &project_root_owned,
                &vm_ips,
                &backend_for_monitor,
                &remote_dir_owned,
                hb_stall,
            )
        })
        .await??;

        cleanup_heartbeat_files(project_root);
        crate::run::kill_games(backend, target_vms, binary_name).await;

        // Filter and report
        for line in combined.lines().filter(|l| filter_re.is_match(l)) {
            output.push_str(line);
            output.push('\n');
        }

        // Classify result
        let failure_code: Option<i32>;
        if was_timeout {
            timed_out_count += 1;
            failed += 1;
            failure_code = Some(-1);
            print_and_push(
                &mut output,
                &format!("--- TIMEOUT ({}s limit) ---", timeout.as_secs()),
                verbose,
            );
        } else if exit_code == Some(0) {
            passed += 1;
            failure_code = None;
            print_and_push(&mut output, "--- PASS ---", verbose);
        } else {
            failed += 1;
            failure_code = exit_code;
            let code_str: Cow<'static, str> = exit_code
                .map(|c| Cow::Owned(format!("{c} ({})", exit_code_label(c, &test_config.exit_codes))))
                .unwrap_or(Cow::Borrowed("signal"));
            print_and_push(&mut output, &format!("--- FAIL (exit {code_str}) ---"), verbose);
        }

        // Collect VM logs on failure
        if was_timeout || exit_code != Some(0) {
            output.push_str("\n--- VM logs (last 30 lines each) ---\n");
            for vm in target_vms {
                output.push_str(&format!("  [{}]:\n", vm.name));
                let log =
                    collect_vm_log(backend, &vm.ip, &config.remote_dir, &config.log_file, 30).await;
                for line in log.lines() {
                    output.push_str(&format!("    {line}\n"));
                }
            }

            output.push_str("\n--- Local unfiltered tail (last 60 lines) ---\n");
            let tail_lines: Vec<&str> = combined.lines().rev().take(60).collect();
            for line in tail_lines.into_iter().rev() {
                output.push_str(line);
                output.push('\n');
            }

            // Capture screenshots on failure
            if test_config.capture_on_failure {
                let screenshot_dir = project_root.join("logs");
                crate::capture::capture_on_failure(config, &screenshot_dir, run_num).await;
            }

            if test_config.stop_on_failure {
                break;
            }
        }

        // Track exit codes for history
        exit_codes.push(exit_code);

        // Track run result for output formatting
        let run_status = if was_timeout {
            RunStatus::Timeout
        } else if exit_code == Some(0) {
            RunStatus::Pass
        } else {
            RunStatus::Fail
        };
        let run_label = exit_code
            .map(|c| exit_code_label(c, &test_config.exit_codes).into_owned())
            .unwrap_or_else(|| "SIGNAL".into());
        session.runs.push(RunResult {
            run_number: run_num,
            status: run_status,
            exit_code,
            exit_label: run_label,
            duration: run_start.elapsed(),
        });

        // Track consecutive identical failures
        if let Some(code) = failure_code {
            if consecutive_fail_code == Some(code) {
                consecutive_fail_count += 1;
            } else {
                consecutive_fail_code = Some(code);
                consecutive_fail_count = 1;
            }
            if !test_config.stop_on_failure && consecutive_fail_count >= REPEAT_FAILURE_LIMIT {
                let label: Cow<'static, str> = if code == -1 {
                    Cow::Borrowed("TIMEOUT")
                } else {
                    Cow::Owned(format!("{code} ({})", exit_code_label(code, &test_config.exit_codes)))
                };
                print_and_push(
                    &mut output,
                    &format!(
                        "\n--- EARLY STOP: {consecutive_fail_count} consecutive identical failures (exit {label}) ---"
                    ),
                    verbose,
                );
                break;
            }
        } else {
            consecutive_fail_code = None;
            consecutive_fail_count = 0;
        }

        output.push('\n');
    }

    // Reset chaos if it was applied
    if test_config.chaos.is_some() {
        let _ = crate::netem::reset(config, None);
    }

    session.total_duration = session_start.elapsed();

    // Summary
    let total = passed + failed;
    let timeout_info: Cow<'static, str> = if timed_out_count > 0 {
        Cow::Owned(format!(", {timed_out_count} timed out"))
    } else {
        Cow::Borrowed("")
    };
    let summary = format!(
        "\n=== SUMMARY ===\n{passed}/{total} passed, {failed} failed{timeout_info}\nConfig: network={}, players={}, vms={vm_count}, timeout={}s",
        test_config.network,
        test_config.players,
        test_config.timeout.as_secs(),
    );
    print_and_push(&mut output, &summary, verbose);

    // Inline flaky detection (when multiple runs)
    if test_config.max_runs > 1 {
        let flaky = crate::history::compute_flakiness(&exit_codes, &test_config.exit_codes);
        for entry in &flaky {
            if matches!(entry.pattern, crate::history::FlakPattern::Intermittent) {
                print_and_push(
                    &mut output,
                    &format!(
                        "  FLAKY: exit {} ({}) appeared in {}/{} runs (score: {:.0}%)",
                        entry.exit_code,
                        entry.label,
                        entry.occurrences,
                        entry.total_runs,
                        entry.flakiness_score * 100.0
                    ),
                    verbose,
                );
            }
        }
    }

    // Emit formatted output
    match test_config.output_format {
        OutputFormat::Text => {
            if let Some(path) = &test_config.output_file {
                std::fs::write(path, &output)?;
                if verbose {
                    println!("Output written to {}", path.display());
                }
            }
        }
        OutputFormat::Junit => {
            let xml = crate::output::emit_junit(&session);
            if let Some(path) = &test_config.output_file {
                std::fs::write(path, &xml)?;
                if verbose {
                    println!("JUnit XML written to {}", path.display());
                }
            } else if verbose {
                println!("{xml}");
            }
        }
        OutputFormat::Jsonl => {
            let jsonl = crate::output::emit_jsonl(&session);
            if let Some(path) = &test_config.output_file {
                std::fs::write(path, &jsonl)?;
                if verbose {
                    println!("JSON Lines written to {}", path.display());
                }
            } else if verbose {
                println!("{jsonl}");
            }
        }
    }

    // Capture git SHA for history
    let git_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    // Compute per-run durations
    let run_durations: Vec<f64> = session.runs.iter().map(|r| r.duration.as_secs_f64()).collect();

    // Save to test history
    let result = crate::history::make_result(
        &test_config.network.to_string(),
        test_config.players,
        vm_count,
        test_config.max_runs,
        test_config.max_runs,
        passed,
        failed,
        timed_out_count,
        test_config.timeout.as_secs(),
        exit_codes,
        git_sha,
        session.total_duration.as_secs_f64(),
        run_durations,
    );
    if let Err(e) = crate::history::save_result(&config.state_dir, result) {
        eprintln!("Warning: failed to save test history: {e}");
    }

    // Run notification hooks
    let hook_vars = crate::hooks::HookVars {
        pass_count: passed,
        fail_count: failed,
        total_count: total,
        timeout_count: timed_out_count,
        pass_rate: if total > 0 { passed * 100 / total } else { 0 },
        duration: session.total_duration,
        exit_label: session
            .runs
            .iter()
            .rev()
            .find(|r| r.status != RunStatus::Pass)
            .map(|r| r.exit_label.clone())
            .unwrap_or_default(),
        network: test_config.network.to_string(),
        players: test_config.players,
        cluster_name: config.cluster_name.clone(),
    };

    if let Some(ref cmd) = test_config.on_complete {
        crate::hooks::run_hook(cmd, &hook_vars).await;
    }
    if failed > 0 {
        if let Some(ref cmd) = test_config.on_failure {
            crate::hooks::run_hook(cmd, &hook_vars).await;
        }
    }

    if failed > 0 {
        anyhow::bail!("{output}\n{failed}/{total} runs failed");
    }
    Ok(output)
}

// ── Helpers ──

fn print_and_push(output: &mut String, msg: &str, verbose: bool) {
    if verbose {
        println!("{msg}");
    }
    output.push_str(msg);
    output.push('\n');
}

/// Launch game on an instance (detached via nohup).
async fn launch_game(
    backend: &Backend,
    ip: &str,
    remote_dir: &str,
    binary_name: &str,
    vm_user: &str,
    log_file: &str,
    args: &str,
) -> anyhow::Result<()> {
    let cmd = format!(
        "cd {remote_dir} && \
         export LD_LIBRARY_PATH=\"{remote_dir}:$LD_LIBRARY_PATH\" \
         XDG_RUNTIME_DIR=/tmp/runtime-{vm_user} \
         WAYLAND_DISPLAY=wayland-1 \
         DISPLAY=:0 && \
         nohup ./{binary_name} {args} > {log_file} 2>&1 < /dev/null & disown"
    );
    // Use run_cmd_timeout to avoid hanging if the channel doesn't close
    let result = backend
        .run_cmd_timeout(ip, &cmd, Duration::from_secs(5))
        .await;
    if !result.success {
        // Timeout is OK here — the command backgrounds successfully but SSH may not close the channel
        if result.stderr.contains("timed out") {
            return Ok(());
        }
        anyhow::bail!("{}", result.stderr);
    }
    Ok(())
}

async fn collect_vm_log(
    backend: &Backend,
    ip: &str,
    remote_dir: &str,
    log_file: &str,
    max_lines: usize,
) -> String {
    let cmd = format!("tail -n {max_lines} {remote_dir}/{log_file} 2>/dev/null || echo '(no log)'");
    backend.run_cmd(ip, &cmd).await.stdout
}

fn cleanup_heartbeat_files(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("game_progress_") && name.ends_with(".json") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ── Process monitoring ──

/// Monitor the local process with timeout and heartbeat detection.
/// Runs synchronously (called from spawn_blocking).
fn monitor_local_process(
    child: &mut std::process::Child,
    timeout: Duration,
    shutdown_timeout: Duration,
    project_root: &Path,
    vm_ips: &[(VmName, IpAddr)],
    backend: &Backend,
    remote_dir: &str,
    heartbeat_stall_secs: u64,
) -> anyhow::Result<(String, Option<i32>, bool)> {
    use std::io::Read as _;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = stdout_pipe {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = stderr_pipe {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });

    let start = Instant::now();
    let exit_code;
    let timed_out;
    let mut last_local_ts: Option<u128> = None;
    let mut last_vm_ts: Option<u128> = None;
    let mut last_heartbeat_check = Instant::now();
    let mut last_vm_check = Instant::now();

    let rt = tokio::runtime::Handle::current();
    let local_hb = LocalHeartbeat { dir: project_root };
    let vm_hb = VmHeartbeat {
        rt: &rt,
        backend,
        vms: vm_ips,
        remote_dir,
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                timed_out = false;
                break;
            }
            Ok(None) if start.elapsed() > timeout => {
                exit_code = kill_and_reap(child, shutdown_timeout);
                timed_out = true;
                break;
            }
            Ok(None) => {
                if last_heartbeat_check.elapsed() > Duration::from_secs(2) {
                    last_heartbeat_check = Instant::now();
                    if check_stall(&local_hb, &mut last_local_ts, heartbeat_stall_secs, now_ms()) {
                        eprintln!("Local heartbeat stall — killing");
                        exit_code = kill_and_reap(child, shutdown_timeout);
                        timed_out = true;
                        break;
                    }
                }

                if last_vm_check.elapsed() > Duration::from_secs(10) {
                    last_vm_check = Instant::now();
                    if check_stall(&vm_hb, &mut last_vm_ts, heartbeat_stall_secs, now_ms()) {
                        eprintln!("VM heartbeat stall — killing");
                        exit_code = kill_and_reap(child, shutdown_timeout);
                        timed_out = true;
                        break;
                    }
                }

                std::thread::sleep(Duration::from_millis(200));
            }
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok((format!("{stdout}{stderr}"), exit_code, timed_out))
}

/// Kill the process group and reap the exit code.
fn kill_and_reap(child: &mut std::process::Child, shutdown_timeout: Duration) -> Option<i32> {
    kill_process_group(child, shutdown_timeout);
    child.try_wait().ok().flatten().and_then(|s| s.code())
}

fn kill_process_group(child: &mut std::process::Child, shutdown_timeout: Duration) {
    let pid = child.id();

    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{pid}"))
            .output();
    }

    let term_start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if term_start.elapsed() > shutdown_timeout => {
                #[cfg(unix)]
                {
                    let _ = std::process::Command::new("kill")
                        .arg("-KILL")
                        .arg(format!("-{pid}"))
                        .output();
                }
                let _ = child.wait();
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Map a game process exit code to a human-readable failure label.
///
/// Checks the config-driven map first, then falls back to hardcoded defaults.
pub fn exit_code_label(code: i32, custom: &HashMap<i32, String>) -> Cow<'static, str> {
    if let Some(label) = custom.get(&code) {
        return Cow::Owned(label.clone());
    }
    Cow::Borrowed(match code {
        0 => "SUCCESS",
        1 => "ERROR",
        10 => "PHASE_WATCHDOG",
        11 => "DAG_VIOLATION",
        12 => "DESYNC",
        13 => "CONNECTION_LOST",
        14 => "INVARIANT_VIOLATION",
        15 => "COMMITMENT_FAILURE",
        16 => "FORMATION_WATCHDOG",
        17 => "MOVE_RESEND_WATCHDOG",
        18 => "PEER_READY_TIMEOUT",
        124 => "TIMEOUT",
        _ => "UNKNOWN",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_min_timestamp_ms_single() {
        let input = r#"{"timestamp_ms": 1000}"#;
        assert_eq!(parse_min_timestamp_ms(input), Some(1000));
    }

    #[test]
    fn parse_min_timestamp_ms_multiple_returns_min() {
        let input = r#"{"timestamp_ms": 3000}{"timestamp_ms": 1000}{"timestamp_ms": 2000}"#;
        assert_eq!(parse_min_timestamp_ms(input), Some(1000));
    }

    #[test]
    fn parse_min_timestamp_ms_empty() {
        assert_eq!(parse_min_timestamp_ms(""), None);
        assert_eq!(parse_min_timestamp_ms("no timestamps here"), None);
    }

    #[test]
    fn parse_min_timestamp_ms_no_digits() {
        assert_eq!(parse_min_timestamp_ms(r#""timestamp_ms": abc"#), None);
    }

    #[test]
    fn exit_code_labels_default() {
        let empty = HashMap::new();
        assert_eq!(&*exit_code_label(0, &empty), "SUCCESS");
        assert_eq!(&*exit_code_label(1, &empty), "ERROR");
        assert_eq!(&*exit_code_label(10, &empty), "PHASE_WATCHDOG");
        assert_eq!(&*exit_code_label(124, &empty), "TIMEOUT");
        assert_eq!(&*exit_code_label(999, &empty), "UNKNOWN");
    }

    #[test]
    fn exit_code_labels_custom() {
        let mut custom = HashMap::new();
        custom.insert(42, "MY_ERROR".to_string());
        custom.insert(0, "OK".to_string());
        assert_eq!(&*exit_code_label(42, &custom), "MY_ERROR");
        assert_eq!(&*exit_code_label(0, &custom), "OK");
        // Fallback for unconfigured code
        assert_eq!(&*exit_code_label(10, &custom), "PHASE_WATCHDOG");
    }

    struct FakeHeartbeat(Option<u128>);
    impl HeartbeatSource for FakeHeartbeat {
        fn min_timestamp(&self) -> Option<u128> { self.0 }
    }

    #[test]
    fn check_stall_no_stall_when_advancing() {
        let hb = FakeHeartbeat(Some(2000));
        let mut last = Some(1000);
        assert!(!check_stall(&hb, &mut last, 30, 2500));
        assert_eq!(last, Some(2000));
    }

    #[test]
    fn check_stall_detected_when_frozen() {
        let hb = FakeHeartbeat(Some(1000));
        let mut last = Some(1000);
        // now is 31 seconds after timestamp
        assert!(check_stall(&hb, &mut last, 30, 1000 + 31_000));
    }

    #[test]
    fn check_stall_no_false_positive_on_first_check() {
        let hb = FakeHeartbeat(Some(5000));
        let mut last = None;
        assert!(!check_stall(&hb, &mut last, 30, 50_000));
        assert_eq!(last, Some(5000));
    }

    #[test]
    fn check_stall_none_heartbeat_no_stall() {
        let hb = FakeHeartbeat(None);
        let mut last = Some(1000);
        assert!(!check_stall(&hb, &mut last, 30, 50_000));
        assert_eq!(last, Some(1000)); // unchanged
    }

    #[test]
    fn check_stall_exact_boundary_no_stall() {
        let hb = FakeHeartbeat(Some(1000));
        let mut last = Some(1000);
        // Exactly at boundary (30s) - should NOT stall (uses > not >=)
        assert!(!check_stall(&hb, &mut last, 30, 1000 + 30_000));
    }

    // ── Custom heartbeat threshold tests ────────────────────────────────

    #[test]
    fn check_stall_custom_threshold_short() {
        let hb = FakeHeartbeat(Some(1000));
        let mut last = Some(1000);
        // With 5s threshold, stall at 6s
        assert!(check_stall(&hb, &mut last, 5, 1000 + 6_000));
    }

    #[test]
    fn check_stall_custom_threshold_short_no_stall() {
        let hb = FakeHeartbeat(Some(1000));
        let mut last = Some(1000);
        // With 5s threshold, no stall at 4s
        assert!(!check_stall(&hb, &mut last, 5, 1000 + 4_000));
    }

    #[test]
    fn check_stall_custom_threshold_long() {
        let hb = FakeHeartbeat(Some(1000));
        let mut last = Some(1000);
        // With 120s threshold, no stall at 60s
        assert!(!check_stall(&hb, &mut last, 120, 1000 + 60_000));
    }

    #[test]
    fn check_stall_custom_threshold_long_stalls() {
        let hb = FakeHeartbeat(Some(1000));
        let mut last = Some(1000);
        // With 120s threshold, stall at 121s
        assert!(check_stall(&hb, &mut last, 120, 1000 + 121_000));
    }

    // ── exit_code_label edge cases ──────────────────────────────────────

    #[test]
    fn exit_code_label_all_hardcoded() {
        let empty = HashMap::new();
        // Verify all hardcoded codes return non-UNKNOWN labels
        for code in [0, 1, 10, 11, 12, 13, 14, 15, 16, 17, 18, 124] {
            let label = exit_code_label(code, &empty);
            assert_ne!(&*label, "UNKNOWN", "Code {code} should have a label");
        }
    }

    #[test]
    fn exit_code_label_negative_code() {
        let empty = HashMap::new();
        assert_eq!(&*exit_code_label(-1, &empty), "UNKNOWN");
    }

    #[test]
    fn exit_code_label_custom_overrides_hardcoded() {
        let mut custom = HashMap::new();
        custom.insert(11, "CUSTOM_DAG".to_string());
        // Custom map overrides the hardcoded "DAG_VIOLATION"
        assert_eq!(&*exit_code_label(11, &custom), "CUSTOM_DAG");
    }

    #[test]
    fn exit_code_label_custom_empty_string() {
        let mut custom = HashMap::new();
        custom.insert(42, String::new());
        assert_eq!(&*exit_code_label(42, &custom), "");
    }

    #[test]
    fn exit_code_label_returns_cow_borrowed_for_defaults() {
        let empty = HashMap::new();
        let label = exit_code_label(0, &empty);
        assert!(matches!(label, Cow::Borrowed(_)));
    }

    #[test]
    fn exit_code_label_returns_cow_owned_for_custom() {
        let mut custom = HashMap::new();
        custom.insert(99, "CUSTOM".to_string());
        let label = exit_code_label(99, &custom);
        assert!(matches!(label, Cow::Owned(_)));
    }

    // ── parse_min_timestamp_ms edge cases ───────────────────────────────

    #[test]
    fn parse_min_timestamp_ms_whitespace_around_value() {
        let input = r#"{"timestamp_ms":   1000  }"#;
        assert_eq!(parse_min_timestamp_ms(input), Some(1000));
    }

    #[test]
    fn parse_min_timestamp_ms_zero() {
        let input = r#"{"timestamp_ms": 0}"#;
        assert_eq!(parse_min_timestamp_ms(input), Some(0));
    }

    #[test]
    fn parse_min_timestamp_ms_large_value() {
        let input = r#"{"timestamp_ms": 1710000000000}"#;
        assert_eq!(parse_min_timestamp_ms(input), Some(1710000000000));
    }

    #[test]
    fn parse_min_timestamp_ms_concatenated_json() {
        // Simulates multiple heartbeat files concatenated
        let input = r#"{"timestamp_ms": 5000}
{"timestamp_ms": 3000}
{"timestamp_ms": 7000}"#;
        assert_eq!(parse_min_timestamp_ms(input), Some(3000));
    }
}
