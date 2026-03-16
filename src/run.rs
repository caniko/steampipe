use crate::config::ClusterConfig;
use crate::ssh::SshClient;

/// Run the `stop-game` subcommand: kill chessbender on all VMs.
pub async fn stop_game(config: &ClusterConfig) -> anyhow::Result<()> {
    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);

    println!("==> Stopping chessbender on all VMs...");
    let mut tasks = tokio::task::JoinSet::new();
    for vm in &config.vms {
        let ssh = ssh.clone();
        let name = vm.name.clone();
        let ip = vm.ip.clone();
        tasks.spawn(async move {
            let result = ssh.run(&ip, "pkill -x chessbender 2>/dev/null; echo ok").await;
            (name, result.success)
        });
    }

    while let Some(result) = tasks.join_next().await {
        let (name, ok) = result?;
        if ok {
            println!("  {name}: stopped");
        } else {
            eprintln!("  {name}: unreachable");
        }
    }
    println!("==> Done");
    Ok(())
}
