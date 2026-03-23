//! VM lifecycle management: start, stop, restart cluster instances.

use std::path::Path;

use crate::config::{BridgeReady, ClusterConfig, IpAddr, VmDef, VmName, par_each_vm};
use crate::lease::{self, VmLease};
use crate::resources::{self, RamRequirements};
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

/// Result of a successful `up` operation. Holds leases for the process lifetime.
pub struct UpResult {
    pub vms: Vec<VmDef>,
    pub leases: Vec<VmLease>,
}

/// Run the `up` subcommand: reserve VMs, check RAM, start instances, wait for connectivity.
/// Returns held leases — dropping them releases the VM locks.
pub async fn up(
    config: &ClusterConfig<BridgeReady>,
    runners_dir: &Path,
) -> anyhow::Result<UpResult> {
    state::ensure_state_dir(&config.state_dir)?;

    let vm_count = config.vms.len() as u8;
    let max_vms = config
        .vms
        .iter()
        .map(|v| v.index)
        .max()
        .unwrap_or(7)
        .max(vm_count);

    // Reserve VMs via flock
    println!("==> Reserving {vm_count} VM(s)...");
    let leases = lease::reserve_n(vm_count, max_vms, &config.lock_dir, &config.cluster_name)?;

    // Build VmDefs from the reserved IDs (which may differ from 1..N)
    let vms: Vec<VmDef> = leases
        .iter()
        .map(|l| VmDef {
            name: VmName(format!("vm-{}", l.vm_id)),
            ip: IpAddr(format!("{}.{}", config.subnet, l.vm_id)),
            index: l.vm_id,
        })
        .collect();

    let reserved_names: Vec<&str> = vms.iter().map(|v| v.name.as_ref()).collect();
    println!("  Reserved: {}", reserved_names.join(", "));

    // Stop any existing instances on these VMs
    for vm in &vms {
        config.backend.stop_instance(config, vm);
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let ram_req = RamRequirements {
        per_vm: config.ram_per_vm,
        host_reserve: config.host_ram_reserve,
    };

    // Start instances with RAM check before each
    let mut started = Vec::new();
    for vm in &vms {
        match resources::check_ram(&ram_req) {
            Ok(avail) => match config.backend.start_instance(config, vm, runners_dir) {
                Ok(pid) => {
                    println!(
                        "  {} started (PID {pid}, {:.1} GB available)",
                        vm.name,
                        avail as f64 / 1e9
                    );
                    started.push(vm.clone());
                }
                Err(e) => eprintln!("  {} FAILED: {e}", vm.name),
            },
            Err(e) => {
                eprintln!("  {} REFUSED: {e}", vm.name);
                break; // No point trying more VMs if RAM is exhausted
            }
        }
    }

    if started.is_empty() {
        anyhow::bail!("no VMs were started — check RAM availability and runner paths");
    }

    if started.len() < vms.len() {
        eprintln!(
            "==> Started {} of {} requested VMs (insufficient RAM for the rest)",
            started.len(),
            vms.len()
        );
    }

    wait_and_report(config, &started).await;

    Ok(UpResult {
        vms: started,
        leases,
    })
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

/// Stop specific VMs by their definitions.
pub fn down_vms<S>(config: &ClusterConfig<S>, vms: &[VmDef]) {
    println!("==> Stopping {} VM(s)...", vms.len());
    for vm in vms {
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
