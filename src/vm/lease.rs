//! VM reservation via PID-based claim files.
//!
//! Each VM slot gets a claim file in the lock directory. A VM is "claimed" if the
//! claim file exists and the PID recorded in it is still alive. Multiple `cluster-ctl`
//! processes can safely share the same VM pool — a process only uses VMs whose
//! claim files reference live processes from its cluster.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::core::state;

/// Information about who holds a VM claim.
pub struct LeaseInfo {
    pub pid: u32,
    pub cluster: String,
}

fn claim_path(vm_id: u8, lock_dir: &Path) -> PathBuf {
    lock_dir.join(format!("vm-{vm_id}.claim"))
}

/// Write a claim file for a VM. Called after `start_instance()` returns a PID.
pub fn write_claim(vm_id: u8, lock_dir: &Path, cluster: &str, pid: u32) -> anyhow::Result<()> {
    fs::create_dir_all(lock_dir)?;
    let meta = serde_json::json!({
        "pid": pid,
        "cluster": cluster,
        "since": chrono::Local::now().to_rfc3339(),
    });
    fs::write(claim_path(vm_id, lock_dir), meta.to_string())?;
    Ok(())
}

/// Remove a claim file for a VM. Called during `down`.
pub fn remove_claim(vm_id: u8, lock_dir: &Path) {
    let _ = fs::remove_file(claim_path(vm_id, lock_dir));
}

/// Check if a VM slot is available. Returns `true` if the slot is free
/// (no claim file, dead PID, or already owned by the same cluster).
pub fn try_reserve(vm_id: u8, lock_dir: &Path, cluster: &str) -> anyhow::Result<bool> {
    match probe_holder(vm_id, lock_dir) {
        None => Ok(true),
        Some(info) if info.cluster == cluster => Ok(true),
        Some(_) => Ok(false),
    }
}

/// Reserve `count` VMs from the pool, skipping ones held by other clusters.
/// Returns the VM IDs that are available.
pub fn reserve_n(
    count: u8,
    max_vms: u8,
    lock_dir: &Path,
    cluster: &str,
) -> anyhow::Result<Vec<u8>> {
    let mut ids = Vec::with_capacity(count as usize);
    let mut held_count = 0u8;

    for id in 1..=max_vms {
        if ids.len() == count as usize {
            break;
        }
        if try_reserve(id, lock_dir, cluster)? {
            ids.push(id);
        } else {
            if let Some(info) = probe_holder(id, lock_dir) {
                eprintln!(
                    "  vm-{id} held by '{}' (pid {}), skipping",
                    info.cluster, info.pid
                );
            } else {
                eprintln!("  vm-{id} locked (unknown holder), skipping");
            }
            held_count += 1;
        }
    }

    if ids.len() < count as usize {
        let available = max_vms - held_count;
        anyhow::bail!(
            "only {} of {} requested VMs available ({} held by other processes)",
            available,
            count,
            held_count
        );
    }

    Ok(ids)
}

/// Probe a claim file to see who holds a VM. Returns `None` if the VM is free
/// (no claim file, unreadable, or the PID is dead).
pub fn probe_holder(vm_id: u8, lock_dir: &Path) -> Option<LeaseInfo> {
    let path = claim_path(vm_id, lock_dir);
    let mut file = OpenOptions::new().read(true).open(&path).ok()?;

    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;

    let v: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let pid = v.get("pid")?.as_u64()? as u32;
    let cluster = v.get("cluster")?.as_str()?.to_string();

    if !state::is_pid_alive(pid) {
        // Stale claim — reclaim by removing it
        let _ = fs::remove_file(&path);
        return None;
    }

    Some(LeaseInfo { pid, cluster })
}

/// List VM IDs currently claimed by a given cluster name (with alive PIDs).
pub fn list_cluster_vms(cluster: &str, max_vms: u8, lock_dir: &Path) -> Vec<u8> {
    let mut ids = Vec::new();
    for id in 1..=max_vms {
        if let Some(info) = probe_holder(id, lock_dir) {
            if info.cluster == cluster {
                ids.push(id);
            }
        }
    }
    ids
}

/// Resolve the lock directory, defaulting to `$XDG_RUNTIME_DIR/steampipe/`.
pub fn default_lock_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("steampipe")
}
