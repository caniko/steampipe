use crate::core::config::ClusterConfig;
use crate::core::state;
use crate::vm::lease;

/// Summary of what the cleanup pass did.
#[derive(Debug, Default)]
pub struct CleanupReport {
    pub stale_pid_files: Vec<String>,
    pub stale_virtiofsd_pid_files: Vec<String>,
    pub orphan_microvm: Vec<String>,
    pub orphan_virtiofsd: Vec<String>,
    pub stale_sockets: Vec<String>,
    pub stale_claims: Vec<String>,
    pub reset_homes: Vec<String>,
    pub errors: Vec<String>,
}

impl CleanupReport {
    pub fn is_empty(&self) -> bool {
        self.stale_pid_files.is_empty()
            && self.stale_virtiofsd_pid_files.is_empty()
            && self.orphan_microvm.is_empty()
            && self.orphan_virtiofsd.is_empty()
            && self.stale_sockets.is_empty()
            && self.stale_claims.is_empty()
            && self.reset_homes.is_empty()
            && self.errors.is_empty()
    }

    pub fn total_cleaned(&self) -> usize {
        self.stale_pid_files.len()
            + self.stale_virtiofsd_pid_files.len()
            + self.orphan_microvm.len()
            + self.orphan_virtiofsd.len()
            + self.stale_sockets.len()
            + self.stale_claims.len()
            + self.errors.len()
    }
}

/// Scan all VM slots in the configured pool and clean stale state.
///
/// This is more thorough than what `doctor --fix` covers:
/// - Stale claim files (PID dead but claim file lingers)
/// - Stale virtiofsd PID files
/// - Orphaned virtiofsd processes
/// - Optionally removes home images for unclaimed VMs
pub fn run_cleanup<S>(config: &ClusterConfig<S>, reset: bool, dry_run: bool) -> CleanupReport {
    let mut report = CleanupReport::default();

    for id in 1..=config.max_vms {
        let vm_name = format!("vm-{id}");

        check_stale_pid(config, &vm_name, dry_run, &mut report);
        check_stale_virtiofsd_pid(config, &vm_name, dry_run, &mut report);
        check_orphan_microvm(config, id, &vm_name, dry_run, &mut report);
        check_orphan_virtiofsd(config, id, &vm_name, dry_run, &mut report);
        check_stale_sockets(config, &vm_name, dry_run, &mut report);
        check_stale_claim(config, id, &vm_name, dry_run, &mut report);

        if reset {
            check_reset_home(config, id, &vm_name, dry_run, &mut report);
        }
    }

    report
}

fn check_stale_pid<S>(
    config: &ClusterConfig<S>,
    vm_name: &str,
    dry_run: bool,
    report: &mut CleanupReport,
) {
    let Some(pid) = state::read_pid(&config.state_dir, vm_name) else {
        return;
    };
    if state::is_pid_alive(pid) {
        return;
    }

    let detail = format!("removed stale PID file (was {pid})");
    if !dry_run {
        state::remove_pid(&config.state_dir, vm_name);
    }
    report.stale_pid_files.push(detail);
}

fn check_stale_virtiofsd_pid<S>(
    config: &ClusterConfig<S>,
    vm_name: &str,
    dry_run: bool,
    report: &mut CleanupReport,
) {
    let pids = state::read_virtiofsd_pids(&config.state_dir, vm_name);
    if pids.is_empty() {
        return;
    }
    if pids.iter().any(|p| state::is_pid_alive(*p)) {
        return;
    }

    let pid_list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let detail = format!("removed stale virtiofsd PID file (was {pid_list})");
    if !dry_run {
        state::remove_virtiofsd_pids(&config.state_dir, vm_name);
    }
    report.stale_virtiofsd_pid_files.push(detail);
}

fn check_orphan_microvm<S>(
    config: &ClusterConfig<S>,
    id: u8,
    vm_name: &str,
    dry_run: bool,
    report: &mut CleanupReport,
) {
    let pattern = format!("microvm@{vm_name}");
    let Ok(out) = std::process::Command::new("pgrep")
        .args(["-f", &pattern])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let pids = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if pids.is_empty() {
        return;
    }

    // Only clean if no active claim exists for this slot.
    if lease::probe_holder(id, &config.lock_dir).is_some() {
        return;
    }

    let detail = format!("killed orphaned microvm process(es): {pids}");
    if !dry_run {
        let _ = std::process::Command::new("pkill")
            .args(["-f", &pattern])
            .status();
    }
    report.orphan_microvm.push(detail);
}

fn check_orphan_virtiofsd<S>(
    config: &ClusterConfig<S>,
    id: u8,
    vm_name: &str,
    dry_run: bool,
    report: &mut CleanupReport,
) {
    let pattern = format!("virtiofsd-steam-state-{vm_name}");
    let Ok(out) = std::process::Command::new("pgrep")
        .args(["-f", &pattern])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let pids = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if pids.is_empty() {
        return;
    }

    if lease::probe_holder(id, &config.lock_dir).is_some() {
        return;
    }

    let detail = format!("killed orphaned virtiofsd process(es): {pids}");
    if !dry_run {
        let _ = std::process::Command::new("pkill")
            .args(["-f", &pattern])
            .status();
    }
    report.orphan_virtiofsd.push(detail);
}

fn check_stale_sockets<S>(
    config: &ClusterConfig<S>,
    vm_name: &str,
    dry_run: bool,
    report: &mut CleanupReport,
) {
    let vm_dir = config.state_dir.join(vm_name);
    if !vm_dir.exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&vm_dir) else {
        return;
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !state::is_socket_file(fname) {
            continue;
        }
        removed.push(fname.to_owned());
        if !dry_run {
            let _ = std::fs::remove_file(&path);
        }
    }
    if removed.is_empty() {
        return;
    }
    let detail = format!(
        "removed {} stale socket file(s): {}",
        removed.len(),
        removed.join(", ")
    );
    report.stale_sockets.push(detail);
}

fn check_stale_claim<S>(
    config: &ClusterConfig<S>,
    id: u8,
    vm_name: &str,
    dry_run: bool,
    report: &mut CleanupReport,
) {
    let claim_path = config.lock_dir.join(format!("{vm_name}.claim"));
    if !claim_path.exists() {
        return;
    }
    // probe_holder already removes stale claims internally, but we check
    // explicitly so we can report the action even in dry-run mode.
    let Some(info) = (|| {
        let contents = std::fs::read_to_string(&claim_path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&contents).ok()?;
        let pid = v.get("pid")?.as_u64()? as u32;
        let cluster = v.get("cluster")?.as_str()?.to_string();
        Some(lease::LeaseInfo { pid, cluster })
    })()
    else {
        return;
    };

    if state::is_pid_alive(info.pid) {
        return;
    }

    let detail = format!(
        "removed stale claim from cluster '{}' (PID {} is dead)",
        info.cluster, info.pid
    );
    if !dry_run {
        lease::remove_claim(id, &config.lock_dir);
    }
    report.stale_claims.push(detail);
}

fn check_reset_home<S>(
    config: &ClusterConfig<S>,
    id: u8,
    vm_name: &str,
    dry_run: bool,
    report: &mut CleanupReport,
) {
    // Only remove home image if no active claim.
    if lease::probe_holder(id, &config.lock_dir).is_some() {
        return;
    }

    let home = config
        .state_dir
        .join(vm_name)
        .join(format!("{vm_name}-home.img"));
    if !home.exists() {
        return;
    }

    let detail = format!("removed home image: {}", home.display());
    if !dry_run {
        match std::fs::remove_file(&home) {
            Ok(()) => {}
            Err(e) => {
                report
                    .errors
                    .push(format!("failed to remove {}: {e}", home.display()));
                return;
            }
        }
    }
    report.reset_homes.push(detail);
}
