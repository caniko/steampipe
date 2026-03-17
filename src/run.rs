use crate::config::{ClusterConfig, VmDef, par_each_vm};
use crate::ssh::SshClient;

/// Kill chessbender on a set of VMs in parallel.
pub async fn kill_games(ssh: &SshClient, vms: &[VmDef]) {
    let results = par_each_vm(vms, |vm| {
        let ssh = ssh.clone();
        async move {
            ssh.run(&vm.ip, "pkill -x chessbender 2>/dev/null || true")
                .await;
        }
    })
    .await;
    let _ = results; // JoinErrors are non-fatal here
}

/// Run the `stop-game` subcommand: kill chessbender on all VMs.
pub async fn stop_game(config: &ClusterConfig) -> anyhow::Result<()> {
    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);
    println!("==> Stopping chessbender on all VMs...");
    kill_games(&ssh, &config.vms).await;
    println!("==> Done");
    Ok(())
}
