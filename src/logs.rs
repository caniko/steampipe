use std::path::{Path, PathBuf};

use crate::config::{ClusterConfig, par_each_vm};
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
    let remote_dir = config.remote_dir.clone();

    println!("==> Collecting logs to {}", output_dir.display());

    par_each_vm(&config.vms, |vm| {
        let ssh = ssh.clone();
        let remote_dir = remote_dir.clone();
        let dest = output_dir.join(format!("{}.log", vm.name));
        async move {
            let remote_log = format!("{remote_dir}/game.log");
            let sources = [Path::new(&remote_log)];
            match ssh.rsync(&sources, &vm.ip, dest.to_str().unwrap_or(".")).await {
                Ok(()) => println!("  {}: collected", vm.name),
                Err(_) => eprintln!("  {}: no log or unreachable", vm.name),
            }
        }
    })
    .await?;

    println!("==> Done");
    Ok(())
}
