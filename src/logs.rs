use std::path::{Path, PathBuf};

use crate::config::{ClusterConfig, par_each_vm};

/// Run the `logs` subcommand: collect game.log from all instances.
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

/// Stream logs in real-time from all instances.
pub async fn follow<S>(config: &ClusterConfig<S>, tail_lines: u32) -> anyhow::Result<()> {
    use tokio::io::AsyncBufReadExt;

    let remote_dir = &config.remote_dir;
    let log_file = &config.log_file;
    let backend = &config.backend;

    println!(
        "==> Streaming logs from {} VMs (Ctrl+C to stop)...\n",
        config.vms.len()
    );

    let colors = [
        "\x1b[36m", "\x1b[33m", "\x1b[32m", "\x1b[35m", "\x1b[34m", "\x1b[31m", "\x1b[37m",
    ];
    let reset = "\x1b[0m";

    let mut handles = Vec::new();

    for (i, vm) in config.vms.iter().enumerate() {
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
