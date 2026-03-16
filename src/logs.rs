use std::path::{Path, PathBuf};

use crate::config::ClusterConfig;
use crate::ssh::SshClient;

/// Run the `logs` subcommand: collect game.log from all VMs.
pub async fn run(
    config: &ClusterConfig,
    project_root: &Path,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let output_dir = output.unwrap_or_else(|| project_root.join(format!("logs/cluster-{timestamp}")));
    std::fs::create_dir_all(&output_dir)?;

    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);

    println!("==> Collecting logs to {}", output_dir.display());
    let mut tasks = tokio::task::JoinSet::new();
    for vm in &config.vms {
        let ssh = ssh.clone();
        let name = vm.name.clone();
        let ip = vm.ip.clone();
        let remote_dir = config.remote_dir.clone();
        let dest = output_dir.join(format!("{name}.log"));

        tasks.spawn(async move {
            let remote_log = format!("{remote_dir}/game.log");
            let sources = [Path::new(&remote_log)];
            match ssh.rsync(&sources, &ip, dest.to_str().unwrap_or(".")).await {
                Ok(()) => println!("  {name}: collected"),
                Err(_) => eprintln!("  {name}: no log or unreachable"),
            }
        });
    }

    while let Some(result) = tasks.join_next().await {
        let _ = result;
    }
    println!("==> Done");
    Ok(())
}
