use std::path::Path;

use crate::config::{ClusterConfig, VmDef, par_each_vm};
use crate::ssh::SshClient;
use crate::state;

/// Start a single VM by running its microvm-run binary.
pub fn start_vm(
    config: &ClusterConfig,
    vm: &VmDef,
    runners_dir: &Path,
) -> anyhow::Result<u32> {
    let runner = runners_dir
        .join(&vm.name)
        .join("bin/microvm-run");
    if !runner.exists() {
        anyhow::bail!("Runner not found: {}", runner.display());
    }

    let vm_dir = config.state_dir.join(&vm.name);
    std::fs::create_dir_all(&vm_dir)?;

    let log_file = std::fs::File::create(vm_dir.join("vm.log"))?;

    let child = std::process::Command::new(&runner)
        .current_dir(&vm_dir)
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()?;

    let pid = child.id();
    state::write_pid(&config.state_dir, &vm.name, pid)?;
    Ok(pid)
}

/// Stop a single VM: kill PID, clean up orphans and socket files.
pub fn stop_vm(config: &ClusterConfig, vm: &VmDef) {
    if let Some(pid) = state::read_pid(&config.state_dir, &vm.name)
        && state::is_pid_alive(pid) {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
    state::remove_pid(&config.state_dir, &vm.name);

    let _ = std::process::Command::new("pkill")
        .args(["-f", &format!("microvm@{}", vm.name)])
        .status();

    let vm_dir = config.state_dir.join(&vm.name);
    if vm_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&vm_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && (name.ends_with(".sock") || name.contains(".vsock")) {
                        let _ = std::fs::remove_file(path);
                    }
            }
        }
}

/// Run the `up` subcommand: start all VMs in config and wait for SSH.
pub async fn up(config: &ClusterConfig, runners_dir: &Path) -> anyhow::Result<()> {
    state::ensure_state_dir(&config.state_dir)?;
    config.validate_bridge()?;

    println!("==> Starting {} VM(s)...", config.vms.len());

    for vm in &config.vms {
        stop_vm(config, vm);
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    for vm in &config.vms {
        match start_vm(config, vm, runners_dir) {
            Ok(pid) => println!("  {} started (PID {pid})", vm.name),
            Err(e) => eprintln!("  {} FAILED: {e}", vm.name),
        }
    }

    println!("==> Waiting for SSH...");
    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);

    let results = par_each_vm(&config.vms, |vm| {
        let ssh = ssh.clone();
        async move {
            let ok = ssh.wait_ready(&vm.ip, 30).await;
            (vm.name, ok)
        }
    })
    .await?;

    let all_ok = results.iter().all(|(_, ok)| *ok);
    for (name, ok) in &results {
        if *ok {
            println!("  {name}: ready");
        } else {
            eprintln!("  {name}: SSH timeout");
        }
    }

    if all_ok {
        println!("==> All VMs ready");
    } else {
        eprintln!("==> Some VMs failed to become reachable");
    }
    Ok(())
}

/// Run the `down` subcommand: stop all VMs.
pub fn down(config: &ClusterConfig) {
    println!("==> Stopping all VMs...");
    for vm in &config.vms {
        let had_pid = state::read_pid(&config.state_dir, &vm.name).is_some();
        stop_vm(config, vm);
        if had_pid {
            println!("  {} stopped", vm.name);
        }
    }
    println!("==> Done");
}
