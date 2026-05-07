//! Pre-flight checks: self-healing cleanup and connectivity verification.
//!
//! Runs before every MCP tool invocation to ensure a clean state:
//! - Stale PID files from dead VM processes
//! - Orphaned microvm processes with no valid lease claim
//! - Leftover socket files in VM state directories
//!
//! Also verifies VM→host connectivity for tests. The NixOS host firewall
//! must already accept traffic on the cluster bridge — that rule is supplied
//! by the `services.steampipe-cluster` NixOS module via
//! `networking.firewall.extraInputRules`. We no longer auto-insert via sudo;
//! if the rule is missing, the operator must rebuild the host config.
//!
//! GPU checks: verifies that VMs have a DRM device when using a GPU display mode.

use std::time::Duration;

use crate::core::backend::Backend;
use crate::core::config::{ClusterConfig, VmDef};
use crate::core::state;

/// Clean up stale state: dead PIDs, orphaned processes, leftover sockets.
///
/// Scans all VM slots (1..=7) regardless of which VMs the current config targets,
/// since orphans may exist on slots outside the current cluster.
///
/// Returns a summary of what was cleaned, or `None` if everything was clean.
pub fn cleanup_stale_state<S>(config: &ClusterConfig<S>) -> Option<String> {
    let max_vms = 7u8;
    let mut cleaned = Vec::new();

    for id in 1..=max_vms {
        let vm_name = format!("vm-{id}");

        // 1. Stale PIDs — PID file exists but process is dead
        if let Some(pid) = state::read_pid(&config.state_dir, &vm_name) {
            if !state::is_pid_alive(pid) {
                state::remove_pid(&config.state_dir, &vm_name);
                cleaned.push(format!("{vm_name}: removed stale PID {pid}"));
            }
        }

        // 2. Orphaned processes — microvm pattern running but no valid claim
        let pattern = format!("microvm@{vm_name}");
        if let Ok(out) = std::process::Command::new("pgrep")
            .args(["-f", &pattern])
            .output()
        {
            if out.status.success() {
                let has_claim = crate::vm::lease::probe_holder(id, &config.lock_dir).is_some();
                if !has_claim {
                    let pids = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    let _ = std::process::Command::new("pkill")
                        .args(["-f", &pattern])
                        .status();
                    cleaned.push(format!("{vm_name}: killed orphan PIDs {pids}"));
                }
            }
        }

        // 3. Stale sockets — leftover .sock/.vsock files in VM state dir
        let vm_dir = config.state_dir.join(&vm_name);
        if vm_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&vm_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
                        if state::is_socket_file(fname) {
                            let _ = std::fs::remove_file(&path);
                            cleaned.push(format!("{vm_name}: removed {fname}"));
                        }
                    }
                }
            }
        }
    }

    if cleaned.is_empty() {
        None
    } else {
        Some(format!("Pre-flight cleanup:\n  {}", cleaned.join("\n  ")))
    }
}

/// Verify that at least one VM can TCP-connect back to the host.
///
/// The nixos-fw bridge accept rule must be present declaratively via the
/// `services.steampipe-cluster` NixOS module (the module sets both
/// `networking.firewall.trustedInterfaces` and a matching `extraInputRules`
/// entry). If connectivity fails, this bails with an actionable error
/// instead of mutating host firewall state at runtime via `sudo nft`.
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
    let listener = std::net::TcpListener::bind(format!("{}:0", config.host_ip)).map_err(|e| {
        anyhow::anyhow!(
            "Cannot bind to {} — is the bridge up? ({})\n  Run: just cluster-net-up",
            config.host_ip,
            e
        )
    })?;
    let port = listener.local_addr()?.port();

    if probe_vm_to_host(backend, &vm.ip, &config.host_ip, port).await {
        return Ok(());
    }

    // Connectivity failed. Surface a precise diagnostic but do NOT mutate
    // the host firewall — the rule is the operator's responsibility, set
    // declaratively via the NixOS module.
    eprintln!(
        "[preflight] VM {} cannot reach host {}:{} — checking nftables...",
        vm.name, config.host_ip, port
    );

    if has_nixos_fw_bridge_rule(&config.bridge) {
        anyhow::bail!(
            "VM→host connectivity failed but the nixos-fw accept rule for {br}\n\
             is present. TCP from {vm} to {host} is blocked by something else\n\
             (NAT, IP forwarding, conflicting rule, etc.). Run `cluster-ctl doctor`.",
            br = config.bridge,
            vm = vm.name,
            host = config.host_ip,
        );
    }

    anyhow::bail!(
        "VM→host connectivity failed and the nixos-fw accept rule for {br}\n\
         is missing. The `services.steampipe-cluster` NixOS module supplies\n\
         this rule via networking.firewall.extraInputRules — rebuild your\n\
         host configuration to apply it (e.g. `sudo nixos-rebuild switch`).\n\
         Steampipe no longer mutates host firewall state at runtime.",
        br = config.bridge,
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
            text.contains(&format!("iifname \"{bridge}\"")) && text.contains("accept")
        }
        _ => false,
    }
}

/// Shell script that checks for a DRM render device inside a VM.
///
/// Returns the script as a string; the caller SSHes it into each VM.
/// Exits 0 and prints the card path if a DRM device is found,
/// exits 1 with a diagnostic message otherwise.
pub fn gpu_check_script() -> &'static str {
    r#"
if ls /dev/dri/card* >/dev/null 2>&1; then
    echo "gpu-ok $(ls /dev/dri/card* | head -1)"
else
    echo "gpu-missing"
    echo "  /dev/dri/ contents:" >&2
    ls -la /dev/dri/ 2>&1 || echo "  /dev/dri/ does not exist" >&2
    exit 1
fi
"#
}

/// Verify that VMs have a DRM device available (required for GPU display modes).
///
/// SSHes into each target VM and checks for `/dev/dri/card*`. Bails with a
/// clear error message if any VM lacks a GPU device.
pub async fn ensure_vm_gpu(
    backend: &Backend,
    target_vms: &[VmDef],
    display: crate::cli::DisplayMode,
) -> anyhow::Result<()> {
    if !display.requires_gpu() {
        return Ok(());
    }

    let script = gpu_check_script();
    let mut failures = Vec::new();

    for vm in target_vms {
        let result = backend
            .run_cmd_timeout(&vm.ip, script, Duration::from_secs(10))
            .await;
        if !result.stdout.contains("gpu-ok") {
            failures.push(format!(
                "  {}: no DRM device found (is microvm.graphics.enable = true?)\n    {}",
                vm.name,
                result.stderr.trim(),
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "GPU check failed — display mode \"{display}\" requires virtio-gpu but \
             these VMs have no DRM device:\n{}",
            failures.join("\n"),
        );
    }
}

/// Parse the output of [`gpu_check_script`] to extract the card device path.
///
/// Returns `Some("/dev/dri/card0")` on success, `None` on failure.
#[cfg(test)]
pub fn parse_gpu_check(stdout: &str) -> Option<&str> {
    stdout.trim().strip_prefix("gpu-ok ").map(|s| s.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── gpu_check_script ────────────────────────────────────────────────

    #[test]
    fn gpu_check_script_checks_dev_dri() {
        let script = gpu_check_script();
        assert!(
            script.contains("/dev/dri/card*"),
            "must probe /dev/dri/card*"
        );
    }

    #[test]
    fn gpu_check_script_prints_gpu_ok_on_success() {
        let script = gpu_check_script();
        assert!(
            script.contains("gpu-ok"),
            "must print gpu-ok when device found"
        );
    }

    #[test]
    fn gpu_check_script_prints_gpu_missing_on_failure() {
        let script = gpu_check_script();
        assert!(
            script.contains("gpu-missing"),
            "must print gpu-missing when no device"
        );
    }

    #[test]
    fn gpu_check_script_exits_nonzero_on_failure() {
        let script = gpu_check_script();
        assert!(script.contains("exit 1"), "must exit 1 when no GPU found");
    }

    #[test]
    fn gpu_check_script_prints_diagnostics_on_failure() {
        let script = gpu_check_script();
        assert!(
            script.contains("/dev/dri/"),
            "must list /dev/dri/ contents for diagnostics"
        );
    }

    // ── parse_gpu_check ─────────────────────────────────────────────────

    #[test]
    fn parse_gpu_check_success() {
        assert_eq!(
            parse_gpu_check("gpu-ok /dev/dri/card0\n"),
            Some("/dev/dri/card0"),
        );
    }

    #[test]
    fn parse_gpu_check_success_with_whitespace() {
        assert_eq!(
            parse_gpu_check("  gpu-ok /dev/dri/card1  \n"),
            Some("/dev/dri/card1"),
        );
    }

    #[test]
    fn parse_gpu_check_failure() {
        assert_eq!(parse_gpu_check("gpu-missing\n"), None);
    }

    #[test]
    fn parse_gpu_check_empty() {
        assert_eq!(parse_gpu_check(""), None);
    }

    #[test]
    fn parse_gpu_check_random_output() {
        assert_eq!(parse_gpu_check("something unexpected"), None);
    }

    // ── DisplayMode::requires_gpu ───────────────────────────────────────

    #[test]
    fn requires_gpu_true_for_gpu_modes() {
        use crate::ui::cli::DisplayMode;
        assert!(DisplayMode::WestonGpu.requires_gpu());
        assert!(DisplayMode::SwayGpu.requires_gpu());
    }

    #[test]
    fn requires_gpu_false_for_non_gpu_modes() {
        use crate::ui::cli::DisplayMode;
        assert!(!DisplayMode::Headless.requires_gpu());
        assert!(!DisplayMode::Weston.requires_gpu());
        assert!(!DisplayMode::Sway.requires_gpu());
    }
}
