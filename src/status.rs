use crate::config::{ClusterConfig, par_each_vm};
use crate::ssh::SshClient;
use crate::state;

struct VmStatus {
    name: String,
    ip: String,
    vm_running: bool,
    ssh_ok: bool,
    steam_running: bool,
    game_running: bool,
}

/// Run the `status` subcommand: tabular status of all VMs.
pub async fn run(config: &ClusterConfig) -> anyhow::Result<()> {
    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);
    let state_dir = config.state_dir.clone();

    let mut results = par_each_vm(&config.vms, |vm| {
        let ssh = ssh.clone();
        let state_dir = state_dir.clone();
        async move {
            let vm_running = state::read_pid(&state_dir, &vm.name)
                .is_some_and(state::is_pid_alive);

            let ssh_ok = ssh.is_reachable(&vm.ip).await;

            let (steam_running, game_running) = if ssh_ok {
                let steam = ssh.run(&vm.ip, "pgrep -x steam >/dev/null 2>&1 && echo yes || echo no").await;
                let game = ssh.run(&vm.ip, "pgrep -x chessbender >/dev/null 2>&1 && echo yes || echo no").await;
                (steam.stdout.trim() == "yes", game.stdout.trim() == "yes")
            } else {
                (false, false)
            };

            VmStatus { name: vm.name, ip: vm.ip, vm_running, ssh_ok, steam_running, game_running }
        }
    })
    .await?;

    results.sort_by(|a, b| a.name.cmp(&b.name));

    println!("{:<8} {:<14} {:<6} {:<6} {:<8} {:<8}", "VM", "IP", "VM", "SSH", "Steam", "Game");
    println!("{}", "─".repeat(56));
    for s in &results {
        println!(
            "{:<8} {:<14} {:<6} {:<6} {:<8} {:<8}",
            s.name,
            s.ip,
            if s.vm_running { "UP" } else { "─" },
            if s.ssh_ok { "OK" } else { "─" },
            if s.steam_running { "OK" } else { "─" },
            if s.game_running { "OK" } else { "─" },
        );
    }
    Ok(())
}
