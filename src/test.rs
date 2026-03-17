use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::config::ClusterConfig;
use crate::ssh::SshClient;

/// Configuration for a test run.
pub struct TestConfig {
    pub mode: String,
    pub network: String,
    pub players: u8,
    pub max_runs: u32,
    pub timeout: Duration,
    pub shutdown_timeout: Duration,
    pub stop_on_failure: bool,
    pub deploy: bool,
    pub build: bool,
    pub headless: bool,
    pub filter_pattern: Option<String>,
    pub output_file: Option<PathBuf>,
}

/// Run the end-to-end test orchestration loop.
pub async fn run(
    config: &ClusterConfig,
    project_root: &Path,
    test_config: TestConfig,
) -> anyhow::Result<()> {
    // Validate inputs
    if test_config.mode != "tournament" && test_config.mode != "1v1" {
        anyhow::bail!("mode must be 'tournament' or '1v1'");
    }
    if test_config.network != "lan" && test_config.network != "steam" {
        anyhow::bail!("network must be 'lan' or 'steam'");
    }
    if test_config.players < 2 || test_config.players > 8 {
        anyhow::bail!("players must be 2-8");
    }

    let vm_count = if test_config.mode == "tournament" {
        (test_config.players as usize).saturating_sub(1).min(config.vms.len())
    } else {
        1 // 1v1 uses only vm-1
    };
    let target_vms = &config.vms[..vm_count];

    let filter_re = Regex::new(
        test_config.filter_pattern.as_deref().unwrap_or(
            r"FAIL|PASS|panic|error|bug_report|violation|completed|timeout|killed|Tournament|summary|Player|disconnected",
        ),
    )?;

    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);
    let mut output = String::new();

    output.push_str(&format!(
        "Cluster test: mode={}, network={}, players={}, vms={vm_count}, max_runs={}\n\n",
        test_config.mode, test_config.network, test_config.players, test_config.max_runs,
    ));

    // Step 1: Deploy
    if test_config.deploy {
        print_and_push(&mut output, "=== DEPLOYING ===");

        if test_config.build {
            print_and_push(&mut output, "Building release binary...");
            let status = tokio::process::Command::new("cargo")
                .args(["build", "--release", "-p", "chessbender"])
                .current_dir(project_root)
                .status()
                .await?;
            if !status.success() {
                anyhow::bail!("cargo build failed");
            }
            print_and_push(&mut output, "Build OK");
        }

        let binary = project_root.join("target/release/chessbender");
        let assets = project_root.join("assets");
        if !binary.exists() {
            anyhow::bail!("Binary not found: {}", binary.display());
        }
        if !assets.exists() {
            anyhow::bail!("Assets dir not found: {}", assets.display());
        }

        // rsync to all target VMs in parallel
        let mut tasks = tokio::task::JoinSet::new();
        for vm in target_vms {
            let ssh = ssh.clone();
            let name = vm.name.clone();
            let ip = vm.ip.clone();
            let remote_dir = config.remote_dir.clone();
            let binary = binary.clone();
            let steam_lib = config.steam_api_lib.clone();
            let appid_file = project_root.join(&config.steam_appid_file);
            let assets = assets.clone();

            tasks.spawn(async move {
                ssh.run(&ip, &format!("mkdir -p {remote_dir}")).await;
                let sources: Vec<&Path> = vec![
                    binary.as_path(),
                    steam_lib.as_path(),
                    appid_file.as_path(),
                    assets.as_path(),
                ];
                ssh.rsync(&sources, &ip, &format!("{remote_dir}/")).await?;
                ssh.run(&ip, &format!("chmod +x {remote_dir}/chessbender"))
                    .await;
                anyhow::Ok(name)
            });
        }

        while let Some(result) = tasks.join_next().await {
            match result? {
                Ok(name) => print_and_push(&mut output, &format!("  {name}: deployed")),
                Err(e) => print_and_push(&mut output, &format!("  DEPLOY FAILED: {e}")),
            }
        }
        output.push('\n');
    }

    // Step 2: Ensure Steam on VMs if steam network
    if test_config.network == "steam" {
        print_and_push(&mut output, "=== STARTING STEAM ON VMs ===");
        for vm in target_vms {
            match ensure_steam(&ssh, &vm.ip).await {
                Ok(_) => {
                    print_and_push(&mut output, &format!("  {}: Steam ready", vm.name));
                }
                Err(e) => {
                    print_and_push(
                        &mut output,
                        &format!("  {}: FAILED — {e}", vm.name),
                    );
                    anyhow::bail!("Steam setup failed on {}", vm.name);
                }
            }
        }
        output.push('\n');
    }

    // Step 3: Run loop
    let binary_path = project_root.join("target/release/chessbender");
    if !binary_path.exists() {
        anyhow::bail!("target/release/chessbender not found");
    }

    let host_addr = format!("{}:{}", config.host_ip, config.udp_port);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut timed_out_count = 0u32;
    let mut consecutive_fail_code: Option<i32> = None;
    let mut consecutive_fail_count = 0u32;
    const REPEAT_FAILURE_LIMIT: u32 = 3;

    for run_num in 1..=test_config.max_runs {
        print_and_push(&mut output, &format!("=== RUN {run_num}/{} ===", test_config.max_runs));

        // 3a: Kill existing games on VMs
        kill_games(&ssh, target_vms).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // 3b: Launch chessbender on VMs
        let vm_args = match (test_config.mode.as_str(), test_config.network.as_str()) {
            ("tournament", "lan") => {
                format!("--headless --auto-join-tournament --udp-addr {host_addr} --auto-play")
            }
            ("tournament", "steam") => {
                "--headless --auto-join-steam-tournament --auto-play".to_string()
            }
            ("1v1", "lan") => {
                format!("--headless --auto-join-udp --udp-addr {host_addr} --auto-play")
            }
            ("1v1", "steam") => "--headless --auto-join-steam --auto-play".to_string(),
            _ => unreachable!(),
        };

        let mut vm_launch_ok = true;
        for vm in target_vms {
            if let Err(e) = launch_game(&ssh, &vm.ip, &config.remote_dir, &vm_args).await {
                print_and_push(
                    &mut output,
                    &format!("  {}: launch FAILED: {e}", vm.name),
                );
                vm_launch_ok = false;
            }
        }

        if !vm_launch_ok {
            failed += 1;
            print_and_push(&mut output, "--- FAIL (VM launch failed) ---");
            kill_games(&ssh, target_vms).await;
            if test_config.stop_on_failure {
                break;
            }
            output.push('\n');
            continue;
        }

        // Give VMs a moment to start
        tokio::time::sleep(Duration::from_secs(2)).await;

        // 3c: Launch local process
        let mut local_cmd = std::process::Command::new(&binary_path);
        match (test_config.mode.as_str(), test_config.network.as_str()) {
            ("tournament", "lan") => {
                local_cmd.arg("--auto-host-tournament").arg("--auto-play");
            }
            ("tournament", "steam") => {
                local_cmd
                    .arg("--auto-host-steam-tournament")
                    .arg("--auto-play");
            }
            ("1v1", "lan") => {
                local_cmd.arg("--auto-host-udp").arg("--auto-play");
            }
            ("1v1", "steam") => {
                local_cmd.arg("--auto-host-steam").arg("--auto-play");
            }
            _ => unreachable!(),
        }

        if test_config.headless {
            local_cmd.arg("--headless");
        }

        local_cmd.current_dir(project_root);
        local_cmd.stdout(std::process::Stdio::piped());
        local_cmd.stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            local_cmd.process_group(0);
        }

        let mut child = local_cmd.spawn()?;

        // 3d: Monitor with heartbeat detection
        cleanup_heartbeat_files(project_root);

        let timeout = test_config.timeout;
        let shutdown_timeout = test_config.shutdown_timeout;
        let project_root_owned = project_root.to_owned();
        let vm_ips: Vec<(String, String)> = target_vms
            .iter()
            .map(|vm| (vm.name.clone(), vm.ip.clone()))
            .collect();
        let ssh_for_monitor = ssh.clone();

        // Run monitoring in a blocking thread since child.try_wait() is sync
        let (combined, exit_code, was_timeout) = tokio::task::spawn_blocking(move || {
            monitor_local_process(
                &mut child,
                timeout,
                shutdown_timeout,
                &project_root_owned,
                &vm_ips,
                &ssh_for_monitor,
            )
        })
        .await??;

        cleanup_heartbeat_files(project_root);

        // 3e: Kill games on VMs
        kill_games(&ssh, target_vms).await;

        // 3f: Filter and report
        let filtered: Vec<&str> = combined
            .lines()
            .filter(|line| filter_re.is_match(line))
            .collect();
        for line in &filtered {
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
            );
        } else if exit_code == Some(0) {
            passed += 1;
            failure_code = None;
            print_and_push(&mut output, "--- PASS ---");
        } else {
            failed += 1;
            failure_code = exit_code;
            let code_str = exit_code
                .map(|c| format!("{c} ({})", exit_code_label(c)))
                .unwrap_or_else(|| "signal".into());
            print_and_push(&mut output, &format!("--- FAIL (exit {code_str}) ---"));
        }

        // Collect VM logs on failure
        let is_failure = was_timeout || exit_code != Some(0);
        if is_failure {
            output.push_str("\n--- VM logs (last 30 lines each) ---\n");
            for vm in target_vms {
                output.push_str(&format!("  [{}]:\n", vm.name));
                let log = collect_vm_log(&ssh, &vm.ip, &config.remote_dir, 30).await;
                for line in log.lines() {
                    output.push_str(&format!("    {line}\n"));
                }
            }

            // Unfiltered local tail
            output.push_str("\n--- Local unfiltered tail (last 60 lines) ---\n");
            let tail_lines: Vec<&str> = combined.lines().rev().take(60).collect();
            for line in tail_lines.into_iter().rev() {
                output.push_str(line);
                output.push('\n');
            }

            if test_config.stop_on_failure {
                break;
            }
        }

        // Track consecutive identical failures
        if let Some(code) = failure_code {
            if consecutive_fail_code == Some(code) {
                consecutive_fail_count += 1;
            } else {
                consecutive_fail_code = Some(code);
                consecutive_fail_count = 1;
            }
            if !test_config.stop_on_failure && consecutive_fail_count >= REPEAT_FAILURE_LIMIT {
                let label = if code == -1 {
                    "TIMEOUT".to_string()
                } else {
                    format!("{code} ({})", exit_code_label(code))
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
    let timeout_info = if timed_out_count > 0 {
        format!(", {timed_out_count} timed out")
    } else {
        String::new()
    };
    let summary = format!(
        "\n=== SUMMARY ===\n{passed}/{total} passed, {failed} failed{timeout_info}\nConfig: mode={}, network={}, players={}, vms={vm_count}, timeout={}s",
        test_config.mode,
        test_config.network,
        test_config.players,
        test_config.timeout.as_secs(),
    );
    print_and_push(&mut output, &summary);

    // Write output file
    if let Some(path) = &test_config.output_file {
        std::fs::write(path, &output)?;
        println!("Output written to {}", path.display());
    }

    if failed > 0 {
        anyhow::bail!("{failed}/{total} runs failed");
    }
    Ok(())
}

/// Print to stdout and append to output buffer.
fn print_and_push(output: &mut String, msg: &str) {
    println!("{msg}");
    output.push_str(msg);
    output.push('\n');
}

/// Kill chessbender on all target VMs.
async fn kill_games(ssh: &SshClient, vms: &[crate::config::VmDef]) {
    let mut tasks = tokio::task::JoinSet::new();
    for vm in vms {
        let ssh = ssh.clone();
        let ip = vm.ip.clone();
        tasks.spawn(async move {
            ssh.run(&ip, "pkill -x chessbender 2>/dev/null || true").await;
        });
    }
    while let Some(result) = tasks.join_next().await {
        let _ = result;
    }
}

/// Ensure weston + Steam are running on a VM.
async fn ensure_steam(ssh: &SshClient, ip: &str) -> anyhow::Result<()> {
    let log = "/home/chessbender/.local/share/Steam/logs/connection_log.txt";
    let cmd = format!(
        "export XDG_RUNTIME_DIR=/tmp/runtime-chessbender; \
         mkdir -p $XDG_RUNTIME_DIR; \
         if ! pgrep -x weston >/dev/null; then \
         weston --backend=headless --xwayland --no-config >/dev/null 2>&1 & \
         sleep 2; \
         fi; \
         export WAYLAND_DISPLAY=wayland-1; \
         export DISPLAY=:0; \
         if pgrep -x steam >/dev/null; then \
         echo STEAM_READY; exit 0; \
         fi; \
         : > {log} 2>/dev/null; \
         nohup steam -silent >/dev/null 2>&1 & \
         for i in $(seq 1 60); do \
         if grep -q 'Logged On.*processing complete' {log} 2>/dev/null; then \
         echo STEAM_READY; exit 0; \
         fi; sleep 1; done; \
         echo STEAM_TIMEOUT"
    );
    let result = ssh.run(ip, &cmd).await;
    if !result.success || !result.stdout.contains("STEAM_READY") {
        anyhow::bail!("Steam failed: {}", result.stderr);
    }
    Ok(())
}

/// Launch chessbender on a VM (detached via nohup).
async fn launch_game(
    ssh: &SshClient,
    ip: &str,
    remote_dir: &str,
    args: &str,
) -> anyhow::Result<()> {
    let cmd = format!(
        "cd {remote_dir} && \
         export LD_LIBRARY_PATH=\"{remote_dir}:$LD_LIBRARY_PATH\" \
         XDG_RUNTIME_DIR=/tmp/runtime-chessbender \
         WAYLAND_DISPLAY=wayland-1 \
         DISPLAY=:0 && \
         nohup ./chessbender {args} > game.log 2>&1 &"
    );
    let result = ssh.run(ip, &cmd).await;
    if !result.success {
        anyhow::bail!("{}", result.stderr);
    }
    Ok(())
}

/// Collect tail of game.log from a VM.
async fn collect_vm_log(ssh: &SshClient, ip: &str, remote_dir: &str, max_lines: usize) -> String {
    let cmd = format!("tail -n {max_lines} {remote_dir}/game.log 2>/dev/null || echo '(no log)'");
    let result = ssh.run(ip, &cmd).await;
    result.stdout
}

/// Monitor the local process with timeout and heartbeat detection.
/// Runs synchronously (called from spawn_blocking).
fn monitor_local_process(
    child: &mut std::process::Child,
    timeout: Duration,
    shutdown_timeout: Duration,
    project_root: &Path,
    vm_ips: &[(String, String)],
    ssh: &SshClient,
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

    // Build a tokio runtime for SSH calls within this blocking context
    let rt = tokio::runtime::Handle::current();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                timed_out = false;
                break;
            }
            Ok(None) if start.elapsed() > timeout => {
                kill_process_group(child, shutdown_timeout);
                exit_code = child.try_wait().ok().flatten().and_then(|s| s.code());
                timed_out = true;
                break;
            }
            Ok(None) => {
                // Local heartbeat check (every ~2s)
                if last_heartbeat_check.elapsed() > Duration::from_secs(2) {
                    last_heartbeat_check = Instant::now();
                    if let Some(ts) = read_min_heartbeat_timestamp(project_root) {
                        if let Some(prev_ts) = last_local_ts
                            && ts <= prev_ts
                        {
                            let stale_secs = now_ms().saturating_sub(ts) / 1000;
                            if stale_secs > HEARTBEAT_STALL_SECS as u128 {
                                eprintln!(
                                    "Local heartbeat stall: no update for {stale_secs}s — killing"
                                );
                                kill_process_group(child, shutdown_timeout);
                                exit_code =
                                    child.try_wait().ok().flatten().and_then(|s| s.code());
                                timed_out = true;
                                break;
                            }
                        }
                        last_local_ts = Some(ts);
                    }
                }

                // VM heartbeat check (every ~10s to limit SSH overhead)
                if last_vm_check.elapsed() > Duration::from_secs(10) {
                    last_vm_check = Instant::now();
                    if let Some(ts) = read_all_vm_heartbeats(&rt, ssh, vm_ips) {
                        if let Some(prev_ts) = last_vm_ts
                            && ts <= prev_ts
                        {
                            let stale_secs = now_ms().saturating_sub(ts) / 1000;
                            if stale_secs > HEARTBEAT_STALL_SECS as u128 {
                                eprintln!(
                                    "VM heartbeat stall: no update for {stale_secs}s — killing"
                                );
                                kill_process_group(child, shutdown_timeout);
                                exit_code =
                                    child.try_wait().ok().flatten().and_then(|s| s.code());
                                timed_out = true;
                                break;
                            }
                        }
                        last_vm_ts = Some(ts);
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

/// Send SIGTERM to process group, wait, then SIGKILL.
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

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Read the minimum heartbeat timestamp from local heartbeat files.
fn read_min_heartbeat_timestamp(dir: &Path) -> Option<u128> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut min_ts: Option<u128> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("game_progress_") && name.ends_with(".json")
            && let Some(ts) = read_heartbeat_from_file(&entry.path())
        {
            min_ts = Some(min_ts.map_or(ts, |prev: u128| prev.min(ts)));
        }
    }
    min_ts
}

fn read_heartbeat_from_file(path: &Path) -> Option<u128> {
    let content = std::fs::read_to_string(path).ok()?;
    let marker = "\"timestamp_ms\":";
    let idx = content.find(marker)?;
    let rest = &content[idx + marker.len()..];
    let rest = rest.trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

/// Remove all heartbeat files from a directory.
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

/// Read minimum heartbeat across all VMs via SSH.
fn read_all_vm_heartbeats(
    rt: &tokio::runtime::Handle,
    ssh: &SshClient,
    vms: &[(String, String)],
) -> Option<u128> {
    let mut min_ts: Option<u128> = None;
    for (_, ip) in vms {
        if let Some(ts) = read_vm_heartbeat(rt, ssh, ip) {
            min_ts = Some(min_ts.map_or(ts, |prev: u128| prev.min(ts)));
        }
    }
    min_ts
}

/// Read heartbeat from a single VM.
fn read_vm_heartbeat(rt: &tokio::runtime::Handle, ssh: &SshClient, ip: &str) -> Option<u128> {
    let cmd = "cat /home/chessbender/chessbender/game_progress_*.json 2>/dev/null || true";
    let result = rt.block_on(ssh.run(ip, cmd));
    let stdout = result.stdout;

    let marker = "\"timestamp_ms\":";
    let mut min_ts: Option<u128> = None;
    let mut search_from = 0;
    while let Some(idx) = stdout[search_from..].find(marker) {
        let abs_idx = search_from + idx + marker.len();
        let rest = stdout[abs_idx..].trim_start();
        if let Some(end) = rest.find(|c: char| !c.is_ascii_digit())
            && let Ok(ts) = rest[..end].parse::<u128>()
        {
            min_ts = Some(min_ts.map_or(ts, |prev: u128| prev.min(ts)));
        }
        search_from = abs_idx;
    }
    min_ts
}

fn exit_code_label(code: i32) -> &'static str {
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
