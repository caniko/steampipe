//! Pre-test connectivity check: verify VMs can reach the host, auto-fix if not.
//!
//! The NixOS host firewall (`inet nixos-fw`) blocks VM→host TCP unless an
//! explicit accept rule exists for the cluster bridge interface. This rule is
//! ephemeral — lost on reboot or `nixos-rebuild switch`. This module detects
//! the problem before the test loop starts and auto-inserts the rule via sudo.

use std::time::Duration;

use crate::backend::Backend;
use crate::config::{ClusterConfig, VmDef};

/// Verify that at least one VM can TCP-connect back to the host.
///
/// If connectivity fails, checks for the missing nftables rule and
/// auto-inserts it via `sudo nft`. Bails if the fix doesn't help.
pub async fn ensure_vm_to_host_connectivity<S>(
    config: &ClusterConfig<S>,
    backend: &Backend,
    target_vms: &[VmDef],
) -> anyhow::Result<()> {
    if !matches!(backend, Backend::MicroVm(_)) {
        return Ok(());
    }

    let Some(vm) = target_vms.first() else {
        return Ok(());
    };

    // Bind an ephemeral listener on the host bridge IP to probe against.
    let listener = std::net::TcpListener::bind(format!("{}:0", config.host_ip))
        .map_err(|e| anyhow::anyhow!(
            "Cannot bind to {} — is the bridge up? ({})\n  Run: just cluster-net-up",
            config.host_ip, e
        ))?;
    let port = listener.local_addr()?.port();

    if probe_vm_to_host(backend, &vm.ip, &config.host_ip, port).await {
        return Ok(());
    }

    // Connectivity failed — check nftables
    eprintln!(
        "[preflight] VM {} cannot reach host {}:{} — checking nftables...",
        vm.name, config.host_ip, port
    );

    if has_nixos_fw_bridge_rule(&config.bridge) {
        anyhow::bail!(
            "VM→host connectivity failed but nixos-fw rule exists.\n\
             The bridge {br} is up and the firewall rule is present, but TCP\n\
             from {vm} to {host} is blocked by something else.\n\
             Run: cluster-ctl doctor",
            br = config.bridge,
            vm = vm.name,
            host = config.host_ip,
        );
    }

    eprintln!(
        "[preflight] Missing nixos-fw accept rule for {} — inserting via sudo...",
        config.bridge
    );
    insert_nixos_fw_rule(&config.bridge)?;

    // Retry
    if probe_vm_to_host(backend, &vm.ip, &config.host_ip, port).await {
        eprintln!("[preflight] Auto-fix successful — VM→host connectivity OK");
        return Ok(());
    }

    anyhow::bail!(
        "VM→host connectivity still failing after inserting nixos-fw rule.\n\
         Run: cluster-ctl doctor"
    );
}

/// Probe TCP connectivity from a VM to the host.
///
/// SSHes into the VM and uses bash's `/dev/tcp` pseudo-device to connect
/// to the host's ephemeral listener. Returns true if the connection succeeds.
async fn probe_vm_to_host(
    backend: &Backend,
    vm_ip: &crate::config::IpAddr,
    host_ip: &str,
    port: u16,
) -> bool {
    let cmd = format!(
        "timeout 3 bash -c '</dev/tcp/{host_ip}/{port}' 2>/dev/null && echo REACH || echo NOREACH"
    );
    let result = backend
        .run_cmd_timeout(vm_ip, &cmd, Duration::from_secs(6))
        .await;
    result.stdout.trim() == "REACH"
}

/// Check if the nixos-fw input chain has an accept rule for the bridge interface.
fn has_nixos_fw_bridge_rule(bridge: &str) -> bool {
    let output = std::process::Command::new("nft")
        .args(["list", "chain", "inet", "nixos-fw", "input"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            // Look for: iifname "br-cluster" accept
            text.contains(&format!("iifname \"{bridge}\""))
                && text.contains("accept")
        }
        _ => false,
    }
}

/// Insert the nixos-fw accept rule for the bridge interface via sudo.
fn insert_nixos_fw_rule(bridge: &str) -> anyhow::Result<()> {
    let iifname = format!("iifname \"{bridge}\"");
    let status = std::process::Command::new("sudo")
        .args(["nft", "insert", "rule", "inet", "nixos-fw", "input", &iifname, "accept"])
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "Failed to insert nixos-fw rule (sudo nft failed).\n\
             Run manually: sudo nft insert rule inet nixos-fw input iifname \"{bridge}\" accept"
        );
    }
    Ok(())
}
