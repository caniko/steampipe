use crate::backend::Backend;
use crate::config::{ClusterConfig, VmDef, par_each_vm};
use crate::state;

/// Status of a single instance: process, connectivity, Steam, game.
pub struct VmStatus {
    pub name: String,
    pub ip: String,
    pub vm_running: bool,
    pub ssh_ok: bool,
    pub steam_running: bool,
    pub game_running: bool,
}

/// Poll status of all instances in parallel. Shared by `status` and `watch`.
pub async fn poll_vm_statuses(
    vms: &[VmDef],
    backend: &Backend,
    state_dir: &std::path::Path,
    binary_name: &str,
) -> Vec<VmStatus> {
    let state_dir = state_dir.to_owned();
    let binary_name = binary_name.to_owned();

    let mut results = par_each_vm(vms, |vm| {
        let backend = backend.clone();
        let state_dir = state_dir.clone();
        let binary_name = binary_name.clone();
        async move {
            let vm_running = state::read_pid(&state_dir, &vm.name)
                .is_some_and(state::is_pid_alive);

            let ssh_ok = backend.is_reachable(&vm.ip).await;

            let (steam_running, game_running) = if ssh_ok {
                let steam = backend.run_cmd(&vm.ip, "pgrep -x steam >/dev/null 2>&1 && echo yes || echo no").await;
                let game = backend.run_cmd(&vm.ip, &format!("pgrep -x {binary_name} >/dev/null 2>&1 && echo yes || echo no")).await;
                (steam.stdout.trim() == "yes", game.stdout.trim() == "yes")
            } else {
                (false, false)
            };

            VmStatus {
                name: vm.name.to_string(),
                ip: vm.ip.to_string(),
                vm_running,
                ssh_ok,
                steam_running,
                game_running,
            }
        }
    })
    .await
    .unwrap_or_default();

    results.sort_by(|a, b| a.name.cmp(&b.name));
    results
}

/// Run the `status` subcommand: tabular status of all instances.
pub async fn run<S>(config: &ClusterConfig<S>) -> anyhow::Result<()> {
    let results = poll_vm_statuses(&config.vms, &config.backend, &config.state_dir, &config.binary_name).await;

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
