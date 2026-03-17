use crate::config::{ClusterConfig, VmDef, par_each_vm};
use crate::ssh::SshClient;
use crate::steam;

/// Kill game process on a set of VMs in parallel.
pub async fn kill_games(ssh: &SshClient, vms: &[VmDef], binary_name: &str) {
    let results = par_each_vm(vms, |vm| {
        let ssh = ssh.clone();
        let binary_name = binary_name.to_owned();
        async move {
            ssh.run(&vm.ip, &format!("pkill -x {binary_name} 2>/dev/null || true"))
                .await;
        }
    })
    .await;
    let _ = results; // JoinErrors are non-fatal here
}

/// Run the `stop-game` subcommand: kill game on all VMs.
pub async fn stop_game<S>(config: &ClusterConfig<S>) -> anyhow::Result<()> {
    let ssh = config.ssh_client();
    println!("==> Stopping game on all VMs...");
    kill_games(&ssh, &config.vms, &config.binary_name).await;
    println!("==> Done");
    Ok(())
}

/// Run the `run` subcommand: start weston + Steam + game on all VMs.
pub async fn start_game<S>(config: &ClusterConfig<S>, extra_args: &[String]) -> anyhow::Result<()> {
    let ssh = config.ssh_client();
    let remote_dir = config.remote_dir.clone();
    let binary_name = config.binary_name.clone();
    let extra = extra_args.join(" ");

    println!("==> Starting game on {} VMs...", config.vms.len());

    let results = par_each_vm(&config.vms, |vm| {
        let ssh = ssh.clone();
        let remote_dir = remote_dir.clone();
        let binary_name = binary_name.clone();
        let extra = extra.clone();
        async move {
            let cmd = format!(
                "{weston}\
                 if ! pgrep -x steam >/dev/null; then\n\
                     steam -silent -cef-disable-gpu >/dev/null 2>&1 &\n\
                     sleep 5\n\
                 fi\n\
                 cd {remote_dir}\n\
                 if pgrep -x {binary_name} >/dev/null; then\n\
                     echo \"already-running\"\n\
                 else\n\
                     nohup ./{binary_name} {extra} > game.log 2>&1 &\n\
                     echo \"started\"\n\
                 fi",
                weston = steam::WESTON_SETUP,
            );
            let result = ssh.run(&vm.ip, &cmd).await;
            (vm.name, result.stdout.trim().to_string())
        }
    })
    .await?;

    for (name, status) in results {
        println!("  {name}: {status}");
    }
    println!("==> Done");
    Ok(())
}
