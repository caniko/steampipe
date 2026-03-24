use std::path::Path;

use crate::config::ClusterConfig;
use crate::state;

/// Diagnostic check result.
struct Check {
    name: &'static str,
    status: CheckStatus,
    detail: String,
}

enum CheckStatus {
    Ok,
    Warning,
    Error,
    Fixed,
}

impl Check {
    fn icon(&self) -> &'static str {
        match self.status {
            CheckStatus::Ok => "OK",
            CheckStatus::Warning => "WARN",
            CheckStatus::Error => "ERR",
            CheckStatus::Fixed => "FIXED",
        }
    }
}

/// Run the `doctor` subcommand: diagnose and optionally fix cluster health issues.
pub fn run<S>(config: &ClusterConfig<S>, fix: bool) -> anyhow::Result<()> {
    println!("==> Cluster health check...\n");

    let mut checks = Vec::new();

    // 1. Check state directory exists
    checks.push(check_state_dir(&config.state_dir));

    // 2. Check for stale PID files
    for vm in &config.vms {
        checks.push(check_stale_pid(config, &vm.name, fix));
    }

    // 3. Check for orphaned processes
    for vm in &config.vms {
        checks.push(check_orphaned_process(&vm.name, fix));
    }

    // 4. Check for leftover socket files
    for vm in &config.vms {
        checks.push(check_stale_sockets(&config.state_dir, &vm.name, fix));
    }

    // 5. Check bridge exists
    checks.push(check_bridge(&config.bridge));

    // 6. Check TAP devices
    for vm in &config.vms {
        checks.push(check_tap_device(&vm.name));
    }

    // 7. Check SSH key exists
    checks.push(check_ssh_key(&config.ssh_key));

    // 8. Check nftables cluster table
    checks.push(check_nftables());

    // Print results
    let mut warnings = 0;
    let mut errors = 0;
    let mut fixed = 0;
    for check in &checks {
        let icon = check.icon();
        println!("  [{icon:<5}] {}: {}", check.name, check.detail);
        match check.status {
            CheckStatus::Warning => warnings += 1,
            CheckStatus::Error => errors += 1,
            CheckStatus::Fixed => fixed += 1,
            CheckStatus::Ok => {}
        }
    }

    println!();
    if errors > 0 {
        println!("==> {errors} error(s), {warnings} warning(s), {fixed} fixed");
        if !fix {
            println!("    Run with --fix to auto-repair where possible");
        }
    } else if warnings > 0 {
        println!("==> {warnings} warning(s), {fixed} fixed — no errors");
    } else {
        println!("==> All checks passed");
    }

    Ok(())
}

fn check_state_dir(state_dir: &Path) -> Check {
    if state_dir.exists() {
        Check {
            name: "state directory",
            status: CheckStatus::Ok,
            detail: format!("{}", state_dir.display()),
        }
    } else {
        Check {
            name: "state directory",
            status: CheckStatus::Warning,
            detail: format!("missing: {}", state_dir.display()),
        }
    }
}

fn check_stale_pid<S>(config: &ClusterConfig<S>, vm_name: &str, fix: bool) -> Check {
    let name_str = format!("pid:{vm_name}");
    // Leak is fine for a CLI diagnostic tool's static str lifetime
    let name: &'static str = name_str.leak();
    match state::read_pid(&config.state_dir, vm_name) {
        Some(pid) if !state::is_pid_alive(pid) => {
            if fix {
                state::remove_pid(&config.state_dir, vm_name);
                Check {
                    name,
                    status: CheckStatus::Fixed,
                    detail: format!("removed stale PID file (was {pid})"),
                }
            } else {
                Check {
                    name,
                    status: CheckStatus::Warning,
                    detail: format!("stale PID {pid} (process dead)"),
                }
            }
        }
        Some(pid) => Check {
            name,
            status: CheckStatus::Ok,
            detail: format!("alive (PID {pid})"),
        },
        None => Check {
            name,
            status: CheckStatus::Ok,
            detail: "no PID file".into(),
        },
    }
}

fn check_orphaned_process(vm_name: &str, fix: bool) -> Check {
    let name_str = format!("orphan:{vm_name}");
    let name: &'static str = name_str.leak();
    let pattern = format!("microvm@{vm_name}");
    let output = std::process::Command::new("pgrep")
        .args(["-f", &pattern])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let pids = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if fix {
                let _ = std::process::Command::new("pkill")
                    .args(["-f", &pattern])
                    .status();
                Check {
                    name,
                    status: CheckStatus::Fixed,
                    detail: format!("killed orphaned process(es): {pids}"),
                }
            } else {
                Check {
                    name,
                    status: CheckStatus::Warning,
                    detail: format!("orphaned process(es): {pids}"),
                }
            }
        }
        _ => Check {
            name,
            status: CheckStatus::Ok,
            detail: "no orphans".into(),
        },
    }
}

fn check_stale_sockets(state_dir: &Path, vm_name: &str, fix: bool) -> Check {
    let name_str = format!("sockets:{vm_name}");
    let name: &'static str = name_str.leak();
    let vm_dir = state_dir.join(vm_name);
    if !vm_dir.exists() {
        return Check {
            name,
            status: CheckStatus::Ok,
            detail: "no state dir".into(),
        };
    }

    let mut stale = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&vm_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
                if state::is_socket_file(fname) {
                    stale.push(fname.to_string());
                }
            }
        }
    }

    if stale.is_empty() {
        Check {
            name,
            status: CheckStatus::Ok,
            detail: "clean".into(),
        }
    } else if fix {
        for s in &stale {
            let _ = std::fs::remove_file(vm_dir.join(s));
        }
        Check {
            name,
            status: CheckStatus::Fixed,
            detail: format!("removed {} socket file(s)", stale.len()),
        }
    } else {
        Check {
            name,
            status: CheckStatus::Warning,
            detail: format!("{} stale socket file(s)", stale.len()),
        }
    }
}

fn check_bridge(bridge: &str) -> Check {
    let ok = std::process::Command::new("ip")
        .args(["link", "show", bridge])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Check {
            name: "bridge",
            status: CheckStatus::Ok,
            detail: format!("{bridge} exists"),
        }
    } else {
        Check {
            name: "bridge",
            status: CheckStatus::Error,
            detail: format!("{bridge} not found — run `cluster-ctl net-up`"),
        }
    }
}

fn check_tap_device(vm_name: &str) -> Check {
    let tap = format!("tap-{vm_name}");
    let name_str = format!("tap:{vm_name}");
    let name: &'static str = name_str.leak();
    let ok = std::process::Command::new("ip")
        .args(["link", "show", &tap])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Check {
            name,
            status: CheckStatus::Ok,
            detail: format!("{tap} exists"),
        }
    } else {
        Check {
            name,
            status: CheckStatus::Warning,
            detail: format!("{tap} missing"),
        }
    }
}

fn check_ssh_key(key: &Path) -> Check {
    if key.exists() {
        Check {
            name: "ssh key",
            status: CheckStatus::Ok,
            detail: format!("{}", key.display()),
        }
    } else {
        Check {
            name: "ssh key",
            status: CheckStatus::Error,
            detail: format!("not found: {}", key.display()),
        }
    }
}

fn check_nftables() -> Check {
    let output = std::process::Command::new("nft")
        .args(["list", "table", "ip", "cluster"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match output {
        Ok(s) if s.success() => Check {
            name: "nftables",
            status: CheckStatus::Ok,
            detail: "cluster table exists".into(),
        },
        _ => Check {
            name: "nftables",
            status: CheckStatus::Warning,
            detail: "no cluster table (NAT may not be configured)".into(),
        },
    }
}
