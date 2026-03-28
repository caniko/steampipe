use crate::backend::Backend;
use crate::config::ClusterConfig;

/// Run the `net-up` subcommand: set up networking for the active backend.
pub fn up<S>(config: &ClusterConfig<S>, nft: &str) -> anyhow::Result<()> {
    match &config.backend {
        Backend::MicroVm(_) => microvm_net_up(config, nft),
        Backend::Docker(b) => docker_net_up(config, &b.network),
        Backend::Local(_) => {
            println!("==> Local backend: no network setup needed");
            Ok(())
        }
    }
}

/// Run the `net-down` subcommand: tear down networking for the active backend.
pub fn down<S>(config: &ClusterConfig<S>, nft: &str) -> anyhow::Result<()> {
    match &config.backend {
        Backend::MicroVm(_) => microvm_net_down(config, nft),
        Backend::Docker(b) => docker_net_down(&b.network),
        Backend::Local(_) => {
            println!("==> Local backend: no network teardown needed");
            Ok(())
        }
    }
}

// ── MicroVM networking (bridge + TAP + nftables) ──

/// A command to execute, produced by the pure planning functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetCmd {
    pub program: String,
    pub args: Vec<String>,
}

impl NetCmd {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|s| (*s).into()).collect(),
        }
    }
}

/// Parse the default network interface from `ip route show default` output.
pub(crate) fn parse_default_interface(ip_route_output: &str) -> Option<String> {
    ip_route_output
        .split_whitespace()
        .skip_while(|w| *w != "dev")
        .nth(1)
        .map(String::from)
}

/// Build the command plan for `net-up` (pure — no side effects).
pub(crate) fn plan_net_up(
    bridge: &str,
    host_ip: &str,
    prefix: u8,
    subnet: &str,
    vm_names: &[&str],
    default_if: &str,
    nft: &str,
) -> Vec<NetCmd> {
    let mut cmds = Vec::new();

    // Create bridge
    cmds.push(NetCmd::new("ip", &["link", "add", bridge, "type", "bridge"]));
    cmds.push(NetCmd::new(
        "ip",
        &["addr", "add", &format!("{host_ip}/{prefix}"), "dev", bridge],
    ));
    cmds.push(NetCmd::new("ip", &["link", "set", bridge, "up"]));

    // Create TAP devices
    for name in vm_names {
        let tap = format!("tap-{name}");
        cmds.push(NetCmd::new("ip", &["tuntap", "add", &tap, "mode", "tap"]));
        cmds.push(NetCmd::new("ip", &["link", "set", &tap, "master", bridge]));
        cmds.push(NetCmd::new("ip", &["link", "set", &tap, "up"]));
    }

    // Enable IP forwarding
    cmds.push(NetCmd::new("sysctl", &["-w", "net.ipv4.ip_forward=1"]));

    // nftables: cluster table + NAT
    let cidr = format!("{subnet}.0/{prefix}");
    let oifname = format!("oifname \"{default_if}\"");
    cmds.push(NetCmd::new(nft, &["add", "table", "ip", "cluster"]));
    cmds.push(NetCmd::new(
        nft,
        &[
            "add",
            "chain",
            "ip",
            "cluster",
            "nat_post",
            "{ type nat hook postrouting priority 100 ; }",
        ],
    ));
    cmds.push(NetCmd::new(
        nft,
        &[
            "add", "rule", "ip", "cluster", "nat_post", "ip", "saddr", &cidr, &oifname,
            "masquerade",
        ],
    ));

    // nftables: forward chain
    let iifname = format!("iifname \"{bridge}\"");
    let oifname_bridge = format!("oifname \"{bridge}\"");
    cmds.push(NetCmd::new(
        nft,
        &[
            "add",
            "chain",
            "ip",
            "cluster",
            "forward",
            "{ type filter hook forward priority 0 ; }",
        ],
    ));
    cmds.push(NetCmd::new(
        nft,
        &["add", "rule", "ip", "cluster", "forward", &iifname, "accept"],
    ));
    cmds.push(NetCmd::new(
        nft,
        &[
            "add",
            "rule",
            "ip",
            "cluster",
            "forward",
            &oifname_bridge,
            "accept",
        ],
    ));

    cmds
}

/// Build the command plan for `net-down` (pure — no side effects).
pub(crate) fn plan_net_down(bridge: &str, vm_names: &[&str], nft: &str) -> Vec<NetCmd> {
    let mut cmds = Vec::new();

    // Remove nftables table
    cmds.push(NetCmd::new(nft, &["delete", "table", "ip", "cluster"]));

    // Remove TAP devices
    for name in vm_names {
        let tap = format!("tap-{name}");
        cmds.push(NetCmd::new("ip", &["link", "delete", &tap]));
    }

    // Remove bridge
    cmds.push(NetCmd::new("ip", &["link", "set", bridge, "down"]));
    cmds.push(NetCmd::new("ip", &["link", "delete", bridge]));

    cmds
}

fn microvm_net_up<S>(config: &ClusterConfig<S>, nft: &str) -> anyhow::Result<()> {
    println!("==> Setting up cluster network...");

    let bridge = &config.bridge;

    // Detect default interface (requires running a command)
    let output = std::process::Command::new("ip")
        .args(["route", "show", "default"])
        .output()?;
    let route = String::from_utf8_lossy(&output.stdout);
    let default_if = parse_default_interface(&route)
        .ok_or_else(|| anyhow::anyhow!("Cannot detect default network interface"))?;

    let vm_names: Vec<&str> = config.vms.iter().map(|vm| vm.name.as_ref()).collect();
    let cmds = plan_net_up(
        bridge,
        &config.host_ip,
        config.prefix,
        &config.subnet,
        &vm_names,
        &default_if,
        nft,
    );

    // Execute, printing progress at milestones
    for cmd in &cmds {
        run_cmd(&cmd.program, &cmd.args.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    }

    println!("  Bridge {bridge} created ({}/{}", config.host_ip, config.prefix);
    println!("  {} TAP devices created", config.vms.len());
    println!("  NAT + forwarding rules added (via {default_if})");
    println!("==> Network ready");
    Ok(())
}

fn microvm_net_down<S>(config: &ClusterConfig<S>, nft: &str) -> anyhow::Result<()> {
    println!("==> Tearing down cluster network...");

    let vm_names: Vec<&str> = config.vms.iter().map(|vm| vm.name.as_ref()).collect();
    let cmds = plan_net_down(&config.bridge, &vm_names, nft);

    // Best-effort teardown — ignore individual failures
    for cmd in &cmds {
        let _ = run_cmd(&cmd.program, &cmd.args.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }

    println!("==> Network torn down");
    Ok(())
}

// ── Docker networking ──

fn docker_net_up<S>(config: &ClusterConfig<S>, network: &str) -> anyhow::Result<()> {
    println!("==> Setting up Docker network...");

    let subnet = format!("{}.0/{}", config.subnet, config.prefix);

    // Remove existing network if present
    let _ = std::process::Command::new("docker")
        .args(["network", "rm", network])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let status = std::process::Command::new("docker")
        .args(["network", "create", "--subnet", &subnet, network])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()?;

    if !status.success() {
        anyhow::bail!(
            "docker network create --subnet {subnet} {network} failed — is Docker running?"
        );
    }

    println!("  Docker network {network} created ({subnet})");
    println!("==> Network ready");
    Ok(())
}

fn docker_net_down(network: &str) -> anyhow::Result<()> {
    println!("==> Tearing down Docker network...");

    let status = std::process::Command::new("docker")
        .args(["network", "rm", network])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()?;

    if !status.success() {
        eprintln!("  Warning: docker network rm {network} failed (may not exist)");
    } else {
        println!("  Docker network {network} removed");
    }

    println!("==> Network torn down");
    Ok(())
}

// ── Helpers ──

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_interface_normal() {
        let output = "default via 192.168.1.1 dev enp5s0 proto dhcp src 192.168.1.100 metric 100";
        assert_eq!(parse_default_interface(output), Some("enp5s0".into()));
    }

    #[test]
    fn parse_default_interface_wlan() {
        let output = "default via 10.0.0.1 dev wlp3s0 proto dhcp metric 600";
        assert_eq!(parse_default_interface(output), Some("wlp3s0".into()));
    }

    #[test]
    fn parse_default_interface_empty() {
        assert_eq!(parse_default_interface(""), None);
    }

    #[test]
    fn parse_default_interface_no_dev() {
        assert_eq!(parse_default_interface("default via 10.0.0.1"), None);
    }

    #[test]
    fn plan_net_up_bridge_commands_first() {
        let cmds = plan_net_up("br-test", "10.0.100.254", 24, "10.0.100", &["vm-1"], "eth0", "nft");
        assert_eq!(cmds[0], NetCmd::new("ip", &["link", "add", "br-test", "type", "bridge"]));
        assert_eq!(
            cmds[1],
            NetCmd::new("ip", &["addr", "add", "10.0.100.254/24", "dev", "br-test"])
        );
        assert_eq!(cmds[2], NetCmd::new("ip", &["link", "set", "br-test", "up"]));
    }

    #[test]
    fn plan_net_up_tap_per_vm() {
        let cmds = plan_net_up(
            "br-test",
            "10.0.100.254",
            24,
            "10.0.100",
            &["vm-1", "vm-2", "vm-3"],
            "eth0",
            "nft",
        );
        // 3 bridge cmds + 3 TAP cmds per VM (9) + 1 sysctl + 6 nft = 19 total
        let tap_cmds: Vec<_> = cmds
            .iter()
            .filter(|c| c.args.iter().any(|a| a.starts_with("tap-")))
            .collect();
        assert_eq!(tap_cmds.len(), 9); // 3 commands × 3 VMs
        assert!(tap_cmds.iter().any(|c| c.args.contains(&"tap-vm-1".into())));
        assert!(tap_cmds.iter().any(|c| c.args.contains(&"tap-vm-2".into())));
        assert!(tap_cmds.iter().any(|c| c.args.contains(&"tap-vm-3".into())));
    }

    #[test]
    fn plan_net_up_nft_masquerade_uses_subnet() {
        let cmds = plan_net_up("br-test", "10.0.100.254", 24, "10.0.100", &["vm-1"], "eth0", "nft");
        let masq = cmds
            .iter()
            .find(|c| c.args.iter().any(|a| a == "masquerade"))
            .expect("masquerade rule missing");
        assert!(masq.args.contains(&"10.0.100.0/24".into()));
        assert!(masq.args.iter().any(|a| a.contains("eth0")));
    }

    #[test]
    fn plan_net_up_forward_chain_in_cluster_table() {
        let cmds = plan_net_up("br-test", "10.0.100.254", 24, "10.0.100", &["vm-1"], "eth0", "nft");
        let fwd_chain = cmds.iter().find(|c| {
            c.args.contains(&"chain".into())
                && c.args.contains(&"forward".into())
        });
        assert!(fwd_chain.is_some(), "forward chain creation missing");
        let fwd = fwd_chain.unwrap();
        assert!(fwd.args.contains(&"cluster".into()), "forward chain must be in cluster table");
    }

    #[test]
    fn plan_net_up_forward_rules_reference_bridge() {
        let cmds = plan_net_up("br-test", "10.0.100.254", 24, "10.0.100", &["vm-1"], "eth0", "nft");
        let fwd_rules: Vec<_> = cmds
            .iter()
            .filter(|c| {
                c.args.contains(&"rule".into())
                    && c.args.contains(&"forward".into())
            })
            .collect();
        assert_eq!(fwd_rules.len(), 2);
        assert!(fwd_rules.iter().any(|c| c.args.iter().any(|a| a.contains("iifname"))));
        assert!(fwd_rules.iter().any(|c| c.args.iter().any(|a| a.contains("oifname"))));
    }

    #[test]
    fn plan_net_up_uses_custom_nft_path() {
        let cmds = plan_net_up("br-test", "10.0.100.254", 24, "10.0.100", &["vm-1"], "eth0", "/usr/bin/nft");
        let nft_cmds: Vec<_> = cmds.iter().filter(|c| c.program == "/usr/bin/nft").collect();
        assert!(nft_cmds.len() >= 5); // table + nat_post chain + nat rule + fwd chain + 2 fwd rules
    }

    #[test]
    fn plan_net_down_removes_table_first() {
        let cmds = plan_net_down("br-test", &["vm-1", "vm-2"], "nft");
        assert_eq!(cmds[0], NetCmd::new("nft", &["delete", "table", "ip", "cluster"]));
    }

    #[test]
    fn plan_net_down_removes_taps_and_bridge() {
        let cmds = plan_net_down("br-test", &["vm-1", "vm-2"], "nft");
        assert!(cmds.iter().any(|c| c.args.contains(&"tap-vm-1".into())));
        assert!(cmds.iter().any(|c| c.args.contains(&"tap-vm-2".into())));
        // Bridge removal is last
        let last = cmds.last().unwrap();
        assert!(last.args.contains(&"br-test".into()));
        assert!(last.args.contains(&"delete".into()));
    }
}
