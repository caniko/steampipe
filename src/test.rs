//! End-to-end test orchestration: deploy, launch, monitor, collect logs, report.
//!
//! The test loop deploys the game binary to VMs, launches a local host process,
//! starts game instances on VMs, monitors for heartbeat stalls and timeouts,
//! and produces a pass/fail summary with failure code breakdown.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::backend::Backend;
use crate::cli::NetworkMode;
use crate::config::{ClusterConfig, IpAddr, VmName};

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
        let cmd = format!("cat {}/game_progress_*.json 2>/dev/null || true", self.remote_dir);
        let mut min_ts: Option<u128> = None;
        for (_, ip) in self.vms {
            let result = self.rt.block_on(self.backend.run_cmd(ip, &cmd));
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
fn check_stall(source: &impl HeartbeatSource, last_ts: &mut Option<u128>, stall_secs: u64) -> bool {
    if let Some(ts) = source.min_timestamp() {
        if let Some(prev) = *last_ts
            && ts <= prev
        {
            let stale = now_ms().saturating_sub(ts) / 1000;
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
) -> anyhow::Result<()> {
    if test_config.players < 2 || test_config.players > 8 {
        anyhow::bail!("players must be 2-8, got {}", test_config.players);
    }

    let vm_count = (test_config.players as usize).saturating_sub(1).min(config.vms.len());
    let target_vms = &config.vms[..vm_count];

    let filter_re = Regex::new(
        test_config.filter_pattern.as_deref().unwrap_or(
            r"FAIL|PASS|panic|error|bug_report|violation|completed|timeout|killed|summary|Player|disconnected",
        ),
    )?;

    let backend = &config.backend;
    let mut output = String::new();

    output.push_str(&format!(
        "Cluster test: network={}, players={}, vms={vm_count}, max_runs={}\n\n",
        test_config.network, test_config.players, test_config.max_runs,
    ));

    // Step 1: Deploy (reuse deploy module)
    if test_config.deploy {
        print_and_push(&mut output, "=== DEPLOYING ===");
        if test_config.build {
            crate::deploy::build_release(project_root, &config.cargo_package).await?;
            print_and_push(&mut output, "Build OK");
        }

        let failed = crate::deploy::deploy_to_vms(backend, target_vms, config, project_root).await?;
        if !failed.is_empty() {
            anyhow::bail!("Deploy failed for {} VMs", failed.len());
        }
        output.push('\n');
    }

    // Step 2: Ensure Steam on VMs if steam network
    if test_config.network == NetworkMode::Steam {
        print_and_push(&mut output, "=== STARTING STEAM ON VMs ===");
        for vm in target_vms {
            match crate::steam::ensure_steam(backend, &vm.ip).await {
                Ok(()) => print_and_push(&mut output, &format!("  {}: Steam ready", vm.name)),
                Err(e) => {
                    print_and_push(&mut output, &format!("  {}: FAILED — {e}", vm.name));
                    anyhow::bail!("Steam setup failed on {}", vm.name);
                }
            }
        }
        output.push('\n');
    }

    // Step 3: Run loop
    let binary_name = &config.binary_name;
    let binary_path = project_root.join(format!("target/release/{binary_name}"));
    if !binary_path.exists() {
        anyhow::bail!("target/release/{binary_name} not found — run with --build or `cargo build --release -p {binary_name}`");
    }

    // Default args based on network mode when not explicitly provided
    let default_vm_args;
    let default_host_args;
    match test_config.network {
        NetworkMode::Lan => {
            default_host_args = "--auto-host-udp --auto-play".to_string();
            default_vm_args = format!(
                "--auto-join-udp --udp-addr {}:{} --auto-play --headless",
                config.host_ip, config.udp_port,
            );
        }
        NetworkMode::Steam => {
            default_host_args = "--auto-host-steam --auto-play".to_string();
            default_vm_args = "--auto-join-steam --auto-play --headless".to_string();
        }
    }
    let vm_args = test_config.vm_args.as_deref().unwrap_or(&default_vm_args);
    let host_args = test_config.host_args.as_deref().unwrap_or(&default_host_args);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut timed_out_count = 0u32;
    let mut exit_codes: Vec<Option<i32>> = Vec::new();
    let mut consecutive_fail_code: Option<i32> = None;
    let mut consecutive_fail_count = 0u32;
    const REPEAT_FAILURE_LIMIT: u32 = 3;

    for run_num in 1..=test_config.max_runs {
        print_and_push(&mut output, &format!("=== RUN {run_num}/{} ===", test_config.max_runs));

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
        local_cmd.stdout(std::process::Stdio::inherit());
        local_cmd.stderr(std::process::Stdio::inherit());

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
            if let Err(e) = launch_game(backend, &vm.ip, &config.remote_dir, binary_name, &config.vm_user, &config.log_file, vm_args).await {
                print_and_push(&mut output, &format!("  {}: launch FAILED: {e}", vm.name));
                vm_launch_ok = false;
            }
        }

        if !vm_launch_ok {
            failed += 1;
            print_and_push(&mut output, "--- FAIL (VM launch failed) ---");
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

        let (combined, exit_code, was_timeout) = tokio::task::spawn_blocking(move || {
            monitor_local_process(
                &mut child,
                timeout,
                shutdown_timeout,
                &project_root_owned,
                &vm_ips,
                &backend_for_monitor,
                &remote_dir_owned,
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
            print_and_push(&mut output, &format!("--- TIMEOUT ({}s limit) ---", timeout.as_secs()));
        } else if exit_code == Some(0) {
            passed += 1;
            failure_code = None;
            print_and_push(&mut output, "--- PASS ---");
        } else {
            failed += 1;
            failure_code = exit_code;
            let code_str: Cow<'static, str> = exit_code
                .map(|c| Cow::Owned(format!("{c} ({})", exit_code_label(c))))
                .unwrap_or(Cow::Borrowed("signal"));
            print_and_push(&mut output, &format!("--- FAIL (exit {code_str}) ---"));
        }

        // Collect VM logs on failure
        if was_timeout || exit_code != Some(0) {
            output.push_str("\n--- VM logs (last 30 lines each) ---\n");
            for vm in target_vms {
                output.push_str(&format!("  [{}]:\n", vm.name));
                let log = collect_vm_log(backend, &vm.ip, &config.remote_dir, &config.log_file, 30).await;
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
                    Cow::Owned(format!("{code} ({})", exit_code_label(code)))
                };
                print_and_push(
                    &mut output,
                    &format!(
                        "\n--- EARLY STOP: {consecutive_fail_count} consecutive identical failures (exit {label}) ---"
                    ),
                );
                break;
            }
        } else {
            consecutive_fail_code = None;
            consecutive_fail_count = 0;
        }

        output.push('\n');
    }

    // Summary
    let total = passed + failed;
    let timeout_info: Cow<'static, str> = if timed_out_count > 0 {
        Cow::Owned(format!(", {timed_out_count} timed out"))
    } else {
        Cow::Borrowed("")
    };
    let summary = format!(
        "\n=== SUMMARY ===\n{passed}/{total} passed, {failed} failed{timeout_info}\nConfig: network={}, players={}, vms={vm_count}, timeout={}s",
        test_config.network, test_config.players, test_config.timeout.as_secs(),
    );
    print_and_push(&mut output, &summary);

    if let Some(path) = &test_config.output_file {
        std::fs::write(path, &output)?;
        println!("Output written to {}", path.display());
    }

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
    );
    if let Err(e) = crate::history::save_result(&config.state_dir, result) {
        eprintln!("Warning: failed to save test history: {e}");
    }

    if failed > 0 {
        anyhow::bail!("{failed}/{total} runs failed");
    }
    Ok(())
}

// ── Helpers ──

fn print_and_push(output: &mut String, msg: &str) {
    println!("{msg}");
    output.push_str(msg);
    output.push('\n');
}

/// Launch game on an instance (detached via nohup).
async fn launch_game(backend: &Backend, ip: &str, remote_dir: &str, binary_name: &str, vm_user: &str, log_file: &str, args: &str) -> anyhow::Result<()> {
    let cmd = format!(
        "cd {remote_dir} && \
         export LD_LIBRARY_PATH=\"{remote_dir}:$LD_LIBRARY_PATH\" \
         XDG_RUNTIME_DIR=/tmp/runtime-{vm_user} \
         WAYLAND_DISPLAY=wayland-1 \
         DISPLAY=:0 && \
         nohup ./{binary_name} {args} > {log_file} 2>&1 < /dev/null & disown"
    );
    // Use run_cmd_timeout to avoid hanging if the channel doesn't close
    let result = backend.run_cmd_timeout(ip, &cmd, Duration::from_secs(5)).await;
    if !result.success {
        // Timeout is OK here — the command backgrounds successfully but SSH may not close the channel
        if result.stderr.contains("timed out") {
            return Ok(());
        }
        anyhow::bail!("{}", result.stderr);
    }
    Ok(())
}

async fn collect_vm_log(backend: &Backend, ip: &str, remote_dir: &str, log_file: &str, max_lines: usize) -> String {
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

    const HEARTBEAT_STALL_SECS: u64 = 30;

    let rt = tokio::runtime::Handle::current();
    let local_hb = LocalHeartbeat { dir: project_root };
    let vm_hb = VmHeartbeat { rt: &rt, backend, vms: vm_ips, remote_dir };

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
                    if check_stall(&local_hb, &mut last_local_ts, HEARTBEAT_STALL_SECS) {
                        eprintln!("Local heartbeat stall — killing");
                        exit_code = kill_and_reap(child, shutdown_timeout);
                        timed_out = true;
                        break;
                    }
                }

                if last_vm_check.elapsed() > Duration::from_secs(10) {
                    last_vm_check = Instant::now();
                    if check_stall(&vm_hb, &mut last_vm_ts, HEARTBEAT_STALL_SECS) {
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
pub fn exit_code_label(code: i32) -> &'static str {
    match code {
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
    }
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
    fn exit_code_labels() {
        assert_eq!(exit_code_label(0), "SUCCESS");
        assert_eq!(exit_code_label(1), "ERROR");
        assert_eq!(exit_code_label(10), "PHASE_WATCHDOG");
        assert_eq!(exit_code_label(124), "TIMEOUT");
        assert_eq!(exit_code_label(999), "UNKNOWN");
    }
}
