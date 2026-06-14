//! VM lifecycle management: start, stop, restart cluster instances.

use std::path::Path;
use std::time::Duration;

use crate::core::backend::Backend;
use crate::core::config::{BridgeReady, ClusterConfig, VmDef, par_each_vm};
use crate::core::state;
use crate::vm::lease;
use crate::vm::resources::{self, RamRequirements};
use crate::vm::shutdown;

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

/// Run the `up` subcommand: start the configured VM set, wait for connectivity,
/// and persist lease claims for the started instances.
pub async fn up(
    config: &ClusterConfig<BridgeReady>,
    runners_dir: &Path,
) -> anyhow::Result<Vec<VmDef>> {
    state::ensure_state_dir(&config.state_dir)?;

    let reserved_names: Vec<&str> = config.vms.iter().map(|v| v.name.as_ref()).collect();
    println!("  Reserved: {}", reserved_names.join(", "));

    // Stop any existing instances on these VMs.
    //
    // Defensive: even though reserve_n already enforced ownership, double-check
    // that the slot's current claim (if any) belongs to us. If a foreign claim
    // slipped through for any reason (race, manual edit, lease bug), refuse to
    // stomp on a still-running VM owned by another cluster.
    for vm in &config.vms {
        if let Some(holder) = lease::probe_holder(vm.index, &config.lock_dir)
            && holder.cluster != config.cluster_name
        {
            anyhow::bail!(
                "vm-{} was reserved for cluster '{}' but its claim now belongs to cluster '{}' (pid {}). \
                 Refusing to stop a VM owned by another cluster. \
                 Run `cluster-ctl --cluster {} down` to release first, or wait for it to finish.",
                vm.index,
                config.cluster_name,
                holder.cluster,
                holder.pid,
                holder.cluster,
            );
        }
        config.backend.stop_instance(config, vm);
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let ram_req = RamRequirements {
        per_vm: config.ram_per_vm,
        host_reserve: config.host_ram_reserve,
    };

    // Start instances with RAM check before each
    let mut started = Vec::new();
    for vm in &config.vms {
        match resources::check_ram(&ram_req) {
            Ok(avail) => {
                crate::core::admission::admit().await;
                match config.backend.start_instance(config, vm, runners_dir) {
                    Ok(pid) => {
                        // Write claim file so other commands can find this VM
                        lease::write_claim(vm.index, &config.lock_dir, &config.cluster_name, pid)?;
                        println!(
                            "  {} started (PID {pid}, {:.1} GB available)",
                            vm.name,
                            avail as f64 / 1e9
                        );
                        started.push(vm.clone());
                    }
                    Err(e) => eprintln!("  {} FAILED: {e}", vm.name),
                }
            }
            Err(e) => {
                eprintln!("  {} REFUSED: {e}", vm.name);
                break; // No point trying more VMs if RAM is exhausted
            }
        }
    }

    if started.is_empty() {
        anyhow::bail!("no VMs were started — check RAM availability and runner paths");
    }

    if started.len() < config.vms.len() {
        eprintln!(
            "==> Started {} of {} requested VMs (insufficient RAM for the rest)",
            started.len(),
            config.vms.len()
        );
    }

    wait_and_report(config, &started).await;

    Ok(started)
}

/// Run the `down` subcommand: gracefully shut down (or force-kill) claimed VMs.
///
/// By default, attempts SSH-based `systemctl poweroff` before falling back to
/// a force-kill. Pass `force: true` to skip the graceful path and kill immediately.
pub async fn down<S>(config: &ClusterConfig<S>, backend: &Backend, force: bool, timeout: Duration) {
    shutdown::shutdown_claimed(config, backend, force, timeout).await;
}

/// Explicitly reclaim one VM from a verified foreign live holder.
///
/// This is intentionally separate from `up`: default reservation remains
/// non-destructive, while reclaim requires the operator to name the expected
/// owning cluster.
pub async fn reclaim<S>(
    config: &ClusterConfig<S>,
    target: &str,
    from_cluster: &str,
) -> anyhow::Result<()> {
    let vm = config
        .find_vm(target)
        .ok_or_else(|| anyhow::anyhow!("Unknown VM: {target}"))?;
    let diagnosis = lease::diagnose_holder(vm.index, &config.lock_dir, &config.cluster_name)
        .ok_or_else(|| anyhow::anyhow!("{} has no live verified lease holder", vm.name))?;

    if diagnosis.owner_cluster != from_cluster {
        anyhow::bail!(
            "{} is held by cluster '{}' (pid {}), not requested --from-cluster '{}'. Refusing reclaim.",
            vm.name,
            diagnosis.owner_cluster,
            diagnosis.owner_pid,
            from_cluster
        );
    }

    println!(
        "==> Reclaiming {} for cluster '{}' from cluster '{}' (pid {}, cmdline_match={}, startup_grace={})",
        vm.name,
        config.cluster_name,
        diagnosis.owner_cluster,
        diagnosis.owner_pid,
        diagnosis.cmdline_matches,
        diagnosis.within_startup_grace
    );
    let removed_heartbeats_before = clear_remote_heartbeat_files(config, vm).await;
    config.backend.stop_instance(config, vm);
    lease::remove_claim(vm.index, &config.lock_dir);
    let removed_heartbeats_after = clear_remote_heartbeat_files(config, vm).await;
    println!(
        "[AUDIT] reclaimed {} from cluster '{}' pid {} for cluster '{}' (removed_heartbeats_before={}, removed_heartbeats_after={})",
        vm.name,
        diagnosis.owner_cluster,
        diagnosis.owner_pid,
        config.cluster_name,
        removed_heartbeats_before.unwrap_or(0),
        removed_heartbeats_after.unwrap_or(0)
    );
    Ok(())
}

async fn clear_remote_heartbeat_files<S>(config: &ClusterConfig<S>, vm: &VmDef) -> Option<u32> {
    let cmd = build_remote_heartbeat_cleanup_cmd(&config.remote_dir, &config.cluster_name);
    let result = config
        .backend
        .run_cmd_timeout(&vm.ip, &cmd, std::time::Duration::from_secs(3))
        .await;
    if !result.success {
        return None;
    }
    result.stdout.trim().parse().ok()
}

fn build_remote_heartbeat_cleanup_cmd(remote_dir: &str, cluster_name: &str) -> String {
    let scoped_dir = format!(
        "{remote_dir}/.steampipe-runtime/{}/heartbeats",
        cluster_name
    );
    format!(
        "files=$(find {scoped} {root} -maxdepth 1 -type f \\( -name 'game_progress_*.json' -o -name 'game_progress_*.tmp' \\) -print 2>/dev/null) && \
         if [ -n \"$files\" ]; then printf '%s\\n' \"$files\" | xargs -r rm -f; fi && \
         if [ -n \"$files\" ]; then printf '%s\\n' \"$files\" | sed '/^$/d' | wc -l; else printf '0\\n'; fi",
        scoped = shell_quote(&scoped_dir),
        root = shell_quote(remote_dir),
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Delete a VM's persistent home image after confirming no live lease owns it.
pub fn reset_vm_home<S>(config: &ClusterConfig<S>, target: &str) -> anyhow::Result<()> {
    let vm = config
        .find_vm(target)
        .ok_or_else(|| anyhow::anyhow!("Unknown VM: {target}"))?;

    if let Some(claim) = lease::probe_holder(vm.index, &config.lock_dir) {
        anyhow::bail!(
            "{}: VM is running (PID {}, cluster '{}'). Run `cluster-ctl --cluster {} down` first.",
            vm.name,
            claim.pid,
            claim.cluster,
            claim.cluster
        );
    }

    let home = config
        .state_dir
        .join(&*vm.name)
        .join(format!("{}-home.img", vm.name));
    if !home.exists() {
        println!("{}: no home image at {}", vm.name, home.display());
        return Ok(());
    }

    std::fs::remove_file(&home)
        .map_err(|error| anyhow::anyhow!("remove {}: {error}", home.display()))?;
    println!("{}: removed {}", vm.name, home.display());
    Ok(())
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
        lease::remove_claim(vm.index, &config.lock_dir);
        if had_pid {
            println!("  {} stopped", vm.name);
        }
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    for vm in &targets {
        crate::core::admission::admit().await;
        match config.backend.start_instance(config, vm, runners_dir) {
            Ok(pid) => {
                lease::write_claim(vm.index, &config.lock_dir, &config.cluster_name, pid)?;
                println!("  {} started (PID {pid})", vm.name);
            }
            Err(e) => eprintln!("  {} FAILED: {e}", vm.name),
        }
    }

    wait_and_report(config, &targets).await;
    println!("==> Done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{BackendKind, ClusterConfig, ProjectConfig};
    use tempfile::tempdir;

    fn test_config(state_dir: &Path, lock_dir: &Path) -> ClusterConfig {
        let project_root = tempdir().unwrap();
        let proj = ProjectConfig {
            backend: Some(BackendKind::Local),
            vm_user: Some("cluster".into()),
            remote_dir: Some("/tmp/steampipe".into()),
            binary_name: Some("game".into()),
            cargo_package: Some("game".into()),
            cluster: Some(crate::core::config::ClusterSection {
                name: Some("default".into()),
                state_dir: Some(state_dir.display().to_string()),
            }),
            ..Default::default()
        };
        ClusterConfig::from_parts(
            project_root.path(),
            Some(proj),
            1,
            Some("default"),
            Some(BackendKind::Local),
            Some(lock_dir),
        )
        .unwrap()
    }

    #[test]
    fn reset_home_removes_file_when_no_claim() {
        let tmp = tempdir().unwrap();
        let vm_dir = tmp.path().join("vm-1");
        std::fs::create_dir(&vm_dir).unwrap();
        let home = vm_dir.join("vm-1-home.img");
        std::fs::write(&home, b"fake").unwrap();
        let lock = tempdir().unwrap();
        let config = test_config(tmp.path(), lock.path());

        reset_vm_home(&config, "vm-1").unwrap();

        assert!(!home.exists());
    }

    #[tokio::test]
    async fn reclaim_refuses_mismatched_from_cluster() {
        let tmp = tempdir().unwrap();
        let lock = tempdir().unwrap();
        let config = test_config(tmp.path(), lock.path());
        lease::write_claim(1, lock.path(), "one-v-one", std::process::id()).unwrap();

        let err = reclaim(&config, "vm-1", "tournament").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not requested --from-cluster 'tournament'"),
            "{msg}"
        );
        assert_eq!(
            lease::probe_holder(1, lock.path()).map(|holder| holder.cluster),
            Some("one-v-one".into())
        );
    }

    #[tokio::test]
    async fn reclaim_removes_intended_claim() {
        let tmp = tempdir().unwrap();
        let lock = tempdir().unwrap();
        let config = test_config(tmp.path(), lock.path());
        lease::write_claim(1, lock.path(), "one-v-one", std::process::id()).unwrap();

        reclaim(&config, "vm-1", "one-v-one").await.unwrap();

        assert!(lease::probe_holder(1, lock.path()).is_none());
    }
}
