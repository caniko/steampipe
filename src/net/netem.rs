use crate::core::config::{ClusterConfig, VmDef};
use crate::vm::lease;

pub(crate) fn validate_netem_params(
    latency_ms: Option<u32>,
    jitter_ms: Option<u32>,
    loss_percent: Option<f32>,
) -> anyhow::Result<()> {
    if let Some(loss) = loss_percent
        && !(0.0..=100.0).contains(&loss) {
            anyhow::bail!("loss percentage must be between 0 and 100, got {loss}");
        }
    if let Some(jit) = jitter_ms {
        if latency_ms.is_none() {
            anyhow::bail!("--jitter requires --latency");
        }
        if jit > latency_ms.unwrap_or(0) {
            eprintln!(
                "  Warning: jitter ({jit}ms) exceeds latency ({}ms)",
                latency_ms.unwrap_or(0)
            );
        }
    }
    Ok(())
}

pub(crate) fn build_netem_args(
    tap: &str,
    latency_ms: Option<u32>,
    jitter_ms: Option<u32>,
    loss_percent: Option<f32>,
    rate_kbit: Option<u32>,
) -> Option<Vec<String>> {
    let has_netem_params = latency_ms.is_some() || loss_percent.is_some() || rate_kbit.is_some();
    if !has_netem_params {
        return None;
    }
    let mut args: Vec<String> = vec![
        "qdisc".into(),
        "add".into(),
        "dev".into(),
        tap.into(),
        "root".into(),
        "netem".into(),
    ];
    if let Some(lat) = latency_ms {
        args.push("delay".into());
        args.push(format!("{lat}ms"));
        if let Some(jit) = jitter_ms {
            args.push(format!("{jit}ms"));
        }
    }
    if let Some(loss) = loss_percent {
        args.push("loss".into());
        args.push(format!("{loss}%"));
    }
    if let Some(rate) = rate_kbit {
        args.push("rate".into());
        args.push(format!("{rate}kbit"));
    }
    Some(args)
}

fn explicit_vm_id(target: &str) -> Option<u8> {
    if target.eq_ignore_ascii_case("all") {
        return None;
    }
    target
        .strip_prefix("vm-")
        .unwrap_or(target)
        .parse::<u8>()
        .ok()
}

fn resolve_netem_targets<S>(
    config: &ClusterConfig<S>,
    target: Option<&str>,
) -> anyhow::Result<Vec<VmDef>> {
    match target {
        None | Some("all") => config.resolve_targets(target, false),
        Some(target) => {
            if let Some(vm) = config.find_vm(target) {
                return Ok(vec![vm.clone()]);
            }

            if let Some(vm_id) = explicit_vm_id(target) {
                return match lease::probe_holder(vm_id, &config.lock_dir) {
                    Some(info) if info.cluster == config.cluster_name => config
                        .claimed_vms()
                        .into_iter()
                        .find(|vm| vm.index == vm_id)
                        .map(|vm| vec![vm])
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "vm-{vm_id} is owned by cluster '{}' but is missing from the active VM set",
                                config.cluster_name
                            )
                        }),
                    Some(info) => anyhow::bail!(
                        "vm-{vm_id} is not owned by cluster '{}' (owned by '{}')",
                        config.cluster_name,
                        info.cluster
                    ),
                    None => anyhow::bail!(
                        "vm-{vm_id} is not owned by cluster '{}' (slot is unowned)",
                        config.cluster_name
                    ),
                };
            }

            config.resolve_targets(Some(target), false)
        }
    }
}

/// Apply network emulation (latency, packet loss, jitter, bandwidth limit) to a VM's TAP device.
pub fn apply<S>(
    config: &ClusterConfig<S>,
    target: &str,
    latency_ms: Option<u32>,
    jitter_ms: Option<u32>,
    loss_percent: Option<f32>,
    rate_kbit: Option<u32>,
) -> anyhow::Result<()> {
    validate_netem_params(latency_ms, jitter_ms, loss_percent)?;

    let targets = resolve_netem_targets(config, Some(target))?;

    for vm in &targets {
        let tap = format!("tap-{}", vm.name);

        // Clear existing qdisc (ignore error if none set)
        let _ = std::process::Command::new("tc")
            .args(["qdisc", "del", "dev", &tap, "root"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        let Some(args) = build_netem_args(&tap, latency_ms, jitter_ms, loss_percent, rate_kbit)
        else {
            println!("  {}: no parameters specified, skipping", vm.name);
            continue;
        };

        let status = std::process::Command::new("tc")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()?;

        if !status.success() {
            anyhow::bail!("tc qdisc add failed for {} (requires sudo)", vm.name);
        }

        let mut desc = Vec::new();
        if let Some(lat) = latency_ms {
            let mut s = format!("latency={lat}ms");
            if let Some(jit) = jitter_ms {
                s.push_str(&format!(" jitter={jit}ms"));
            }
            desc.push(s);
        }
        if let Some(loss) = loss_percent {
            desc.push(format!("loss={loss}%"));
        }
        if let Some(rate) = rate_kbit {
            desc.push(format!("rate={rate}kbit"));
        }
        println!("  {}: {}", vm.name, desc.join(", "));
    }

    Ok(())
}

/// Show current netem qdisc settings on VM TAP devices.
pub fn show<S>(config: &ClusterConfig<S>) -> anyhow::Result<()> {
    println!("==> Network emulation status:\n");

    for vm in &config.vms {
        let tap = format!("tap-{}", vm.name);
        let output = std::process::Command::new("tc")
            .args(["qdisc", "show", "dev", &tap])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.trim();
        if line.is_empty() || !line.contains("netem") {
            println!("  {}: (no netem rules)", vm.name);
        } else {
            // Extract just the netem parameters
            for l in line.lines() {
                if l.contains("netem") {
                    println!("  {}: {}", vm.name, l.trim());
                }
            }
        }
    }
    Ok(())
}

/// Reset (remove) all netem rules from VM TAP devices.
pub fn reset<S>(config: &ClusterConfig<S>, target: Option<&str>) -> anyhow::Result<()> {
    let targets = resolve_netem_targets(config, target)?;
    for vm in &targets {
        let tap = format!("tap-{}", vm.name);
        let _ = std::process::Command::new("tc")
            .args(["qdisc", "del", "dev", &tap, "root"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        println!("  {}: netem rules cleared", vm.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ClusterConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_lock_dir(tag: &str) -> std::path::PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "steampipe-netem-test-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn netem_args_latency_only() {
        let args = build_netem_args("tap-vm0", Some(50), None, None, None).unwrap();
        assert!(args.contains(&"delay".to_string()));
        assert!(args.contains(&"50ms".to_string()));
    }

    #[test]
    fn netem_args_latency_and_jitter() {
        let args = build_netem_args("tap-vm0", Some(50), Some(10), None, None).unwrap();
        let delay_pos = args.iter().position(|a| a == "delay").unwrap();
        assert_eq!(args[delay_pos + 1], "50ms");
        assert_eq!(args[delay_pos + 2], "10ms");
    }

    #[test]
    fn netem_args_loss_only() {
        let args = build_netem_args("tap-vm0", None, None, Some(1.5), None).unwrap();
        assert!(args.contains(&"loss".to_string()));
        assert!(args.contains(&"1.5%".to_string()));
    }

    #[test]
    fn netem_args_rate_only() {
        let args = build_netem_args("tap-vm0", None, None, None, Some(1000)).unwrap();
        assert!(args.contains(&"rate".to_string()));
        assert!(args.contains(&"1000kbit".to_string()));
    }

    #[test]
    fn netem_args_combined() {
        let args = build_netem_args("tap-vm0", Some(50), Some(10), Some(1.5), Some(1000)).unwrap();
        // Verify all params present in correct order: delay 50ms 10ms loss 1.5% rate 1000kbit
        let delay_pos = args.iter().position(|a| a == "delay").unwrap();
        assert_eq!(args[delay_pos + 1], "50ms");
        assert_eq!(args[delay_pos + 2], "10ms");
        let loss_pos = args.iter().position(|a| a == "loss").unwrap();
        assert_eq!(args[loss_pos + 1], "1.5%");
        let rate_pos = args.iter().position(|a| a == "rate").unwrap();
        assert_eq!(args[rate_pos + 1], "1000kbit");
        assert!(delay_pos < loss_pos && loss_pos < rate_pos);
    }

    #[test]
    fn netem_args_no_params_returns_none() {
        let result = build_netem_args("tap-vm0", None, None, None, None);
        assert!(result.is_none());
    }

    #[test]
    fn validate_jitter_without_latency_fails() {
        let err = validate_netem_params(None, Some(10), None).unwrap_err();
        assert!(err.to_string().contains("--jitter requires --latency"));
    }

    #[test]
    fn validate_loss_out_of_range_fails() {
        let err = validate_netem_params(None, None, Some(101.0)).unwrap_err();
        assert!(
            err.to_string()
                .contains("loss percentage must be between 0 and 100")
        );
    }

    #[test]
    fn validate_valid_params_ok() {
        assert!(validate_netem_params(Some(50), Some(10), Some(1.0)).is_ok());
    }

    #[test]
    fn resolve_netem_target_accepts_non_contiguous_owned_vm() {
        let config = ClusterConfig::for_test_ids(&[2, 4]);
        let targets = resolve_netem_targets(&config, Some("vm-4")).unwrap();
        assert_eq!(
            targets.iter().map(|vm| vm.index).collect::<Vec<_>>(),
            vec![4]
        );
    }

    #[test]
    fn resolve_netem_target_rejects_slot_owned_by_other_cluster() {
        let lock_dir = temp_lock_dir("foreign-owner");
        lease::write_claim(3, &lock_dir, "regicide", std::process::id()).unwrap();

        let mut config = ClusterConfig::for_test_ids(&[2, 4]);
        config.cluster_name = "fixture".into();
        config.lock_dir = lock_dir.clone();

        let err = resolve_netem_targets(&config, Some("vm-3")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "vm-3 is not owned by cluster 'fixture' (owned by 'regicide')"
        );

        let _ = std::fs::remove_dir_all(lock_dir);
    }

    #[test]
    fn resolve_netem_target_rejects_unowned_slot() {
        let lock_dir = temp_lock_dir("unowned");
        let mut config = ClusterConfig::for_test_ids(&[2, 4]);
        config.cluster_name = "fixture".into();
        config.lock_dir = lock_dir.clone();

        let err = resolve_netem_targets(&config, Some("vm-3")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "vm-3 is not owned by cluster 'fixture' (slot is unowned)"
        );

        let _ = std::fs::remove_dir_all(lock_dir);
    }
}
