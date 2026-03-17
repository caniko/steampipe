use crate::config::ClusterConfig;

/// Run the `net-up` subcommand: create bridge, TAP devices, NAT rules.
pub fn up<S>(config: &ClusterConfig<S>, nft: &str) -> anyhow::Result<()> {
    println!("==> Setting up cluster network...");

    let bridge = &config.bridge;
    let host_ip = &config.host_ip;
    let prefix = config.prefix;

    // Create bridge
    run_cmd("ip", &["link", "add", bridge, "type", "bridge"])?;
    run_cmd("ip", &["addr", "add", &format!("{host_ip}/{prefix}"), "dev", bridge])?;
    run_cmd("ip", &["link", "set", bridge, "up"])?;
    println!("  Bridge {bridge} created ({host_ip}/{prefix})");

    // Create TAP devices for each VM
    for vm in &config.vms {
        let tap = format!("tap-{}", vm.name);
        run_cmd("ip", &["tuntap", "add", &tap, "mode", "tap"])?;
        run_cmd("ip", &["link", "set", &tap, "master", bridge])?;
        run_cmd("ip", &["link", "set", &tap, "up"])?;
    }
    println!("  {} TAP devices created", config.vms.len());

    // Enable IP forwarding
    run_cmd("sysctl", &["-w", "net.ipv4.ip_forward=1"])?;

    // Detect default interface
    let output = std::process::Command::new("ip")
        .args(["route", "show", "default"])
        .output()?;
    let route = String::from_utf8_lossy(&output.stdout);
    let default_if = route
        .split_whitespace()
        .skip_while(|w| *w != "dev")
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Cannot detect default network interface"))?
        .to_string();

    // nftables rules
    let subnet = format!("{}.0/{}", config.subnet, prefix);
    run_cmd(nft, &[
        "add", "table", "ip", "cluster",
    ])?;
    run_cmd(nft, &[
        "add", "chain", "ip", "cluster", "nat_post",
        "{ type nat hook postrouting priority 100 ; }",
    ])?;
    run_cmd(nft, &[
        "add", "rule", "ip", "cluster", "nat_post",
        "ip", "saddr", &subnet, &format!("oifname \"{default_if}\""), "masquerade",
    ])?;
    // Allow forwarding through NixOS firewall
    run_cmd(nft, &[
        "add", "rule", "ip", "nixos-fw", "forward",
        &format!("iifname \"{bridge}\""), "accept",
    ])?;
    run_cmd(nft, &[
        "add", "rule", "ip", "nixos-fw", "forward",
        &format!("oifname \"{bridge}\""), "accept",
    ])?;
    println!("  NAT + forwarding rules added (via {default_if})");

    println!("==> Network ready");
    Ok(())
}

/// Run the `net-down` subcommand: tear down bridge, TAP devices, NAT rules.
pub fn down<S>(config: &ClusterConfig<S>, nft: &str) -> anyhow::Result<()> {
    println!("==> Tearing down cluster network...");

    // Remove nftables table (removes all rules in it)
    let _ = run_cmd(nft, &["delete", "table", "ip", "cluster"]);

    // Remove TAP devices
    for vm in &config.vms {
        let tap = format!("tap-{}", vm.name);
        let _ = run_cmd("ip", &["link", "delete", &tap]);
    }

    // Remove bridge
    let _ = run_cmd("ip", &["link", "set", &config.bridge, "down"]);
    let _ = run_cmd("ip", &["link", "delete", &config.bridge]);

    println!("==> Network torn down");
    Ok(())
}

fn run_cmd(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()?;
    if !status.success() {
        anyhow::bail!("{} {} failed (exit {})", program, args.join(" "), status);
    }
    Ok(())
}
