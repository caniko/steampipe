use std::path::Path;

use crate::config::{BridgeReady, ClusterConfig, VmDef, par_each_vm};
use crate::state;

/// Wait for connectivity on a set of instances in parallel and print results.
async fn wait_and_report(config: &ClusterConfig<BridgeReady>, vms: &[VmDef]) {
    println!("==> Waiting for SSH...");
    let backend = config.backend.clone();

    let results = par_each_vm(vms, |vm| {
        let backend = backend.clone();
        async move {
            let ok = backend.wait_ready(&vm.ip, 30).await;
            (vm.name, ok)
        }
    })
    .await
    .unwrap_or_default();

    for (name, ok) in &results {
        if *ok {
            println!("  {name}: ready");
        } else {
            eprintln!("  {name}: SSH timeout");
        }
    }

    if results.iter().all(|(_, ok)| *ok) {
        println!("==> All VMs ready");
    } else {
        eprintln!("==> Some VMs failed to become reachable");
    }
}

/// Run the `up` subcommand: start all instances and wait for connectivity.
pub async fn up(config: &ClusterConfig<BridgeReady>, runners_dir: &Path) -> anyhow::Result<()> {
    state::ensure_state_dir(&config.state_dir)?;

    println!("==> Starting {} VM(s)...", config.vms.len());

    for vm in &config.vms {
        config.backend.stop_instance(config, vm);
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    for vm in &config.vms {
        match config.backend.start_instance(config, vm, runners_dir) {
            Ok(pid) => println!("  {} started (PID {pid})", vm.name),
            Err(e) => eprintln!("  {} FAILED: {e}", vm.name),
        }
    }

    wait_and_report(config, &config.vms).await;
    Ok(())
}

/// Run the `down` subcommand: stop all instances.
pub fn down<S>(config: &ClusterConfig<S>) {
    println!("==> Stopping all VMs...");
    for vm in &config.vms {
        let had_pid = state::read_pid(&config.state_dir, &vm.name).is_some();
        config.backend.stop_instance(config, vm);
        if had_pid {
            println!("  {} stopped", vm.name);
        }
    }
    println!("==> Done");
}

/// Run the `restart` subcommand: stop then start target instances.
pub async fn restart(
    config: &ClusterConfig<BridgeReady>,
    target: Option<&str>,
    runners_dir: &Path,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, false)?;

    println!("==> Restarting {} VM(s)...", targets.len());

    for vm in &targets {
        let had_pid = state::read_pid(&config.state_dir, &vm.name).is_some();
        config.backend.stop_instance(config, vm);
        if had_pid {
            println!("  {} stopped", vm.name);
        }
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    for vm in &targets {
        match config.backend.start_instance(config, vm, runners_dir) {
            Ok(pid) => println!("  {} started (PID {pid})", vm.name),
            Err(e) => eprintln!("  {} FAILED: {e}", vm.name),
        }
    }

    wait_and_report(config, &targets).await;
    println!("==> Done");
    Ok(())
}
