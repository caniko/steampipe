use crate::config::ClusterConfig;

/// Apply network emulation (latency, packet loss, jitter, bandwidth limit) to a VM's TAP device.
pub fn apply<S>(
    config: &ClusterConfig<S>,
    target: &str,
    latency_ms: Option<u32>,
    jitter_ms: Option<u32>,
    loss_percent: Option<f32>,
    rate_kbit: Option<u32>,
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
    let targets = config.resolve_targets(Some(target), false)?;

    for vm in &targets {
        let tap = format!("tap-{}", vm.name);

        // Clear existing qdisc (ignore error if none set)
        let _ = std::process::Command::new("tc")
            .args(["qdisc", "del", "dev", &tap, "root"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        let mut args: Vec<String> = vec![
            "qdisc".into(),
            "add".into(),
            "dev".into(),
            tap.clone(),
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

        let has_netem_params =
            latency_ms.is_some() || loss_percent.is_some() || rate_kbit.is_some();
        if !has_netem_params {
            println!("  {}: no parameters specified, skipping", vm.name);
            continue;
        }

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
