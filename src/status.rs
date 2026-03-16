use crate::config::ClusterConfig;
use crate::ssh::SshClient;
use crate::state;

/// Per-VM status result.
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

    // Probe all VMs in parallel
    let mut tasks = tokio::task::JoinSet::new();
    for vm in &config.vms {
        let ssh = ssh.clone();
        let name = vm.name.clone();
        let ip = vm.ip.clone();
        let state_dir = config.state_dir.clone();
        tasks.spawn(async move {
            let vm_running = state::read_pid(&state_dir, &name)
                .is_some_and(state::is_pid_alive);

            let ssh_ok = ssh.is_reachable(&ip).await;

            let (steam_running, game_running) = if ssh_ok {
                let steam = ssh.run(&ip, "pgrep -x steam >/dev/null 2>&1 && echo yes || echo no").await;
                let game = ssh.run(&ip, "pgrep -x chessbender >/dev/null 2>&1 && echo yes || echo no").await;
                (
                    steam.stdout.trim() == "yes",
                    game.stdout.trim() == "yes",
                )
            } else {
                (false, false)
            };

            VmStatus { name, ip, vm_running, ssh_ok, steam_running, game_running }
        });
    }

    let mut results = Vec::new();
    while let Some(result) = tasks.join_next().await {
        results.push(result?);
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));

    // Print table
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
