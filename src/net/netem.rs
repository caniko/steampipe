use crate::core::config::ClusterConfig;

pub(crate) fn validate_netem_params(
    latency_ms: Option<u32>,
    jitter_ms: Option<u32>,
    loss_percent: Option<f32>,
) -> anyhow::Result<()> {
    if let Some(loss) = loss_percent {
        if !(0.0..=100.0).contains(&loss) {
            anyhow::bail!("loss percentage must be between 0 and 100, got {loss}");
        }
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

    let targets = config.resolve_targets(Some(target), false)?;

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
    let targets = config.resolve_targets(target, false)?;
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
        let args =
            build_netem_args("tap-vm0", Some(50), Some(10), Some(1.5), Some(1000)).unwrap();
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
        assert!(err.to_string().contains("loss percentage must be between 0 and 100"));
    }

    #[test]
    fn validate_valid_params_ok() {
        assert!(validate_netem_params(Some(50), Some(10), Some(1.0)).is_ok());
    }
}
