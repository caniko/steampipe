use std::time::Duration;

use crate::core::backend::Backend;
use crate::core::config::ClusterConfig;
use crate::core::state;
use crate::vm::lease;

/// The result of shutting down a single VM.
#[derive(Debug)]
pub enum ShutdownResult {
    /// VM shut down cleanly via SSH poweroff.
    Graceful { vm_name: String },
    /// VM was unreachable or graceful shutdown timed out; force-killed.
    ForceKilled {
        vm_name: String,
        reason: &'static str,
    },
    /// VM was not running.
    AlreadyDown { vm_name: String },
    /// An unexpected error occurred.
    Error { vm_name: String, error: String },
}

/// Attempt a graceful shutdown of every VM claimed by this cluster.
///
/// For each VM:
///   1. If the VM has a live PID and `force` is false, send an SSH
///      `sudo systemctl poweroff`  and wait up to `timeout` for the
///      VM to become unreachable.
///   2. If graceful shutdown succeeds the QEMU / runner process should
///      exit on its own; we clean up PID files, leases, and sockets.
///   3. If the VM was already unreachable (frozen) or graceful shutdown
///      timed out, fall back to a force-kill via the backend.
pub async fn shutdown_claimed<S>(
    config: &ClusterConfig<S>,
    backend: &Backend,
    force: bool,
    timeout: Duration,
) {
    let vms = config.claimed_vms();
    if vms.is_empty() {
        println!("  No claimed VMs.");
        return;
    }

    println!("==> Shutting down {} VM(s)...", vms.len());
    let mut results: Vec<ShutdownResult> = Vec::with_capacity(vms.len());

    for vm in &vms {
        let result = shutdown_vm(config, backend, vm, force, timeout).await;
        match &result {
            ShutdownResult::Graceful { vm_name } => {
                println!("  {vm_name}: graceful shutdown OK");
            }
            ShutdownResult::ForceKilled { vm_name, reason } => {
                println!("  {vm_name}: {reason}");
            }
            ShutdownResult::AlreadyDown { vm_name } => {
                println!("  {vm_name}: already stopped");
            }
            ShutdownResult::Error { vm_name, error } => {
                eprintln!("  {vm_name}: ERROR — {error}");
            }
        }
        results.push(result);
    }

    let graceful_count = results
        .iter()
        .filter(|r| matches!(r, ShutdownResult::Graceful { .. }))
        .count();
    let force_count = results
        .iter()
        .filter(|r| matches!(r, ShutdownResult::ForceKilled { .. }))
        .count();
    let down_count = results
        .iter()
        .filter(|r| matches!(r, ShutdownResult::AlreadyDown { .. }))
        .count();
    let error_count = results
        .iter()
        .filter(|r| matches!(r, ShutdownResult::Error { .. }))
        .count();

    println!(
        "==> Done ({graceful_count} graceful, {force_count} force-killed, {down_count} already stopped, {error_count} errors)"
    );
}

/// Shut down a single VM, returning the result.
async fn shutdown_vm<S>(
    config: &ClusterConfig<S>,
    backend: &Backend,
    vm: &crate::core::config::VmDef,
    force: bool,
    timeout: Duration,
) -> ShutdownResult {
    let vm_name = vm.name.to_string();

    // 1. Check if the VM has a live PID.
    let pid = match state::read_pid(&config.state_dir, &vm_name) {
        Some(pid) if state::is_pid_alive(pid) => pid,
        _ => {
            return ShutdownResult::AlreadyDown {
                vm_name: vm_name.clone(),
            };
        }
    };

    // 2. If --force was passed, skip graceful shutdown.
    if force {
        do_force_kill(config, vm, pid);
        return ShutdownResult::ForceKilled {
            vm_name: vm_name.clone(),
            reason: "force-killed (--force)",
        };
    }

    // 3. Attempt graceful shutdown via SSH.
    let reachable = backend.is_reachable(&vm.ip).await;
    if !reachable {
        // VM is frozen — SSH is already down.
        do_force_kill(config, vm, pid);
        return ShutdownResult::ForceKilled {
            vm_name: vm_name.clone(),
            reason: "force-killed (VM was frozen / unreachable)",
        };
    }

    // Send poweroff command.
    let poweroff_cmd = "sudo systemctl poweroff";
    let result = backend
        .run_cmd_timeout(&vm.ip, poweroff_cmd, Duration::from_secs(10))
        .await;
    if !result.success {
        eprintln!(
            "  {vm_name}: poweroff command returned non-zero (will wait and force-kill if needed): {}",
            result.stderr.trim()
        );
    }

    // 4. Wait for the VM to become unreachable (or the PID to die).
    let deadline = tokio::time::Instant::now() + timeout;
    let mut success = false;
    loop {
        if !state::is_pid_alive(pid) {
            success = true;
            break;
        }
        if !backend.is_reachable(&vm.ip).await {
            // SSH is down but PID may still be alive briefly during shutdown.
            // Give it a short grace period to exit cleanly.
            let pid_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if !state::is_pid_alive(pid) {
                    success = true;
                    break;
                }
                if tokio::time::Instant::now() >= pid_deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    if success {
        // Graceful shutdown succeeded.
        // Clean state: the PID died, but we still need to remove files / sockets.
        backend.stop_instance(config, vm);
        lease::remove_claim(vm.index, &config.lock_dir);
        ShutdownResult::Graceful {
            vm_name: vm_name.clone(),
        }
    } else {
        // Graceful shutdown timed out or PID did not die — force-kill.
        do_force_kill(config, vm, pid);
        ShutdownResult::ForceKilled {
            vm_name: vm_name.clone(),
            reason: "force-killed (graceful shutdown timed out)",
        }
    }
}

/// Force-kill a VM and clean up state.
fn do_force_kill<S>(config: &ClusterConfig<S>, vm: &crate::core::config::VmDef, _pid: u32) {
    config.backend.stop_instance(config, vm);
    lease::remove_claim(vm.index, &config.lock_dir);
}
