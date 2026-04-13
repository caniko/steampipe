use std::path::{Path, PathBuf};

use crate::core::config::{ClusterConfig, VmName, par_each_vm};

/// Run the `logs` subcommand: collect game.log from all instances to files.
pub async fn run<S>(
    config: &ClusterConfig<S>,
    project_root: &Path,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let output_dir =
        output.unwrap_or_else(|| project_root.join(format!("logs/cluster-{timestamp}")));
    std::fs::create_dir_all(&output_dir)?;

    let backend = config.backend.clone();
    let remote_dir = config.remote_dir.clone();
    let log_file = config.log_file.clone();

    println!("==> Collecting logs to {}", output_dir.display());

    par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        let remote_dir = remote_dir.clone();
        let log_file = log_file.clone();
        let dest = output_dir.join(format!("{}.log", vm.name));
        async move {
            let remote_log = format!("{remote_dir}/{log_file}");
            let sources = [Path::new(&remote_log)];
            match backend
                .upload(&sources, &vm.ip, dest.to_str().unwrap_or("."))
                .await
            {
                Ok(()) => println!("  {}: collected", vm.name),
                Err(_) => eprintln!("  {}: no log or unreachable", vm.name),
            }
        }
    })
    .await?;

    println!("==> Done");
    Ok(())
}

/// Collect logs from VM instances. Returns (vm_name, log_text) pairs.
pub async fn collect_logs<S>(
    config: &ClusterConfig<S>,
    target: Option<&str>,
    lines: u32,
    head: bool,
    pattern: Option<&str>,
) -> anyhow::Result<Vec<(VmName, String)>> {
    let vms = config.resolve_targets(target, false)?;
    let backend = config.backend.clone();
    let remote_dir = config.remote_dir.clone();
    let log_file = config.log_file.clone();

    let results = par_each_vm(&vms, |vm| {
        let backend = backend.clone();
        let remote_dir = remote_dir.clone();
        let log_file = log_file.clone();
        let pattern = pattern.map(|s| s.to_owned());
        async move {
            let mode = if head { "head" } else { "tail" };
            let remote_log = format!("{remote_dir}/{log_file}");
            let cmd = if let Some(ref pat) = pattern {
                // Use grep to filter, then head/tail for line count
                // Escape single quotes in pattern
                let escaped = pat.replace('\'', "'\\''");
                format!("grep -E '{escaped}' {remote_log} 2>/dev/null | {mode} -n {lines}")
            } else {
                format!("{mode} -n {lines} {remote_log} 2>/dev/null")
            };
            let result = backend.run_cmd(&vm.ip, &cmd).await;
            (vm.name.clone(), result.stdout)
        }
    })
    .await?;

    Ok(results)
}

/// Print logs to stdout (no file output). Supports target filtering, head/tail, line count, and regex.
pub async fn print_stdout<S>(
    config: &ClusterConfig<S>,
    target: Option<&str>,
    lines: u32,
    head: bool,
    pattern: Option<&str>,
) -> anyhow::Result<()> {
    let results = collect_logs(config, target, lines, head, pattern).await?;

    for (name, output) in &results {
        let trimmed = output.trim();
        if trimmed.is_empty() {
            println!("── {name} ── (no output)");
        } else {
            println!("── {name} ──");
            println!("{trimmed}");
        }
        println!();
    }

    Ok(())
}

/// Stream logs in real-time from instances.
pub async fn follow<S>(
    config: &ClusterConfig<S>,
    tail_lines: u32,
    target: Option<&str>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncBufReadExt;

    let vms = config.resolve_targets(target, false)?;
    let backend = &config.backend;

    println!(
        "==> Streaming logs from {} VM(s) (Ctrl+C to stop)...\n",
        vms.len()
    );

    let colors = [
        "\x1b[36m", "\x1b[33m", "\x1b[32m", "\x1b[35m", "\x1b[34m", "\x1b[31m", "\x1b[37m",
    ];
    let reset = "\x1b[0m";

    let remote_dir = &config.remote_dir;
    let log_file = &config.log_file;
    let mut handles = Vec::new();

    for (i, vm) in vms.iter().enumerate() {
        let color = colors[i % colors.len()];
        let vm_name = vm.name.to_string();
        let cmd = format!("tail -n {tail_lines} -f {remote_dir}/{log_file} 2>/dev/null");

        let extra_opts = ["-o", "ConnectTimeout=5", "-o", "ServerAliveInterval=10"];
        let mut child = match backend.spawn_streaming(&vm.ip, &cmd, &extra_opts) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{color}[{vm_name}]{reset} Failed to connect: {e}");
                continue;
            }
        };

        let handle = tokio::spawn(async move {
            if let Some(stdout) = child.stdout.take() {
                let reader = tokio::io::BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!("{color}[{vm_name}]{reset} {line}");
                }
            }
        });

        handles.push(handle);
    }

    tokio::signal::ctrl_c().await?;
    println!("\n==> Stopped streaming");
    Ok(())
}
