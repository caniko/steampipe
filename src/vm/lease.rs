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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Per-test unique lock directory.
    fn unique_lock_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("steampipe-lease-test-{tag}-{pid}-{n}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn live_pid() -> u32 {
        std::process::id()
    }

    /// Find a PID that is guaranteed not to be alive on this system.
    fn dead_pid() -> u32 {
        for candidate in (u32::MAX - 1000..u32::MAX).rev() {
            if !crate::core::state::is_pid_alive(candidate) {
                return candidate;
            }
        }
        unreachable!("no dead pid found in scan range");
    }

    #[test]
    fn claim_path_is_lock_dir_joined() {
        let p = claim_path(3, Path::new("/tmp/lock-dir"));
        assert_eq!(p, PathBuf::from("/tmp/lock-dir/vm-3.claim"));
    }

    #[test]
    fn write_then_probe_returns_holder() {
        let dir = unique_lock_dir("probe");
        write_claim(1, &dir, "alpha", live_pid()).unwrap();
        let info = probe_holder(1, &dir).expect("claim should be live");
        assert_eq!(info.pid, live_pid());
        assert_eq!(info.cluster, "alpha");
    }

    #[test]
    fn probe_holder_none_when_no_claim() {
        let dir = unique_lock_dir("missing");
        assert!(probe_holder(7, &dir).is_none());
    }

    #[test]
    fn probe_holder_reclaims_stale_pid() {
        let dir = unique_lock_dir("stale");
        write_claim(2, &dir, "ghost", dead_pid()).unwrap();
        assert!(claim_path(2, &dir).exists());
        assert!(probe_holder(2, &dir).is_none());
        assert!(
            !claim_path(2, &dir).exists(),
            "stale claim file should be removed"
        );
    }

    #[test]
    fn remove_claim_deletes_file() {
        let dir = unique_lock_dir("remove");
        write_claim(4, &dir, "c", live_pid()).unwrap();
        assert!(claim_path(4, &dir).exists());
        remove_claim(4, &dir);
        assert!(!claim_path(4, &dir).exists());
    }

    #[test]
    fn remove_claim_silent_when_missing() {
        let dir = unique_lock_dir("remove-missing");
        // Should not panic.
        remove_claim(99, &dir);
    }

    #[test]
    fn try_reserve_free_slot() {
        let dir = unique_lock_dir("free");
        assert!(try_reserve(1, &dir, "alpha").unwrap());
    }

    #[test]
    fn try_reserve_same_cluster_is_available() {
        let dir = unique_lock_dir("same");
        write_claim(1, &dir, "alpha", live_pid()).unwrap();
        assert!(try_reserve(1, &dir, "alpha").unwrap());
    }

    #[test]
    fn try_reserve_other_cluster_is_blocked() {
        let dir = unique_lock_dir("other");
        write_claim(1, &dir, "alpha", live_pid()).unwrap();
        assert!(!try_reserve(1, &dir, "beta").unwrap());
    }

    #[test]
    fn reserve_n_picks_first_n_when_all_free() {
        let dir = unique_lock_dir("first-n");
        let ids = reserve_n(3, 5, &dir, "alpha").unwrap();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn reserve_n_skips_other_cluster_holds() {
        let dir = unique_lock_dir("skip");
        write_claim(1, &dir, "beta", live_pid()).unwrap();
        write_claim(3, &dir, "beta", live_pid()).unwrap();
        let ids = reserve_n(2, 5, &dir, "alpha").unwrap();
        assert_eq!(ids, vec![2, 4]);
    }

    #[test]
    fn reserve_n_fails_when_not_enough_available() {
        let dir = unique_lock_dir("not-enough");
        for id in 1..=4 {
            write_claim(id, &dir, "beta", live_pid()).unwrap();
        }
        let err = reserve_n(3, 5, &dir, "alpha").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("only 1 of 3 requested VMs available"), "{msg}");
        assert!(msg.contains("4 held by other processes"), "{msg}");
    }

    #[test]
    fn reserve_n_treats_same_cluster_holds_as_available() {
        let dir = unique_lock_dir("same-cluster-holds");
        write_claim(1, &dir, "alpha", live_pid()).unwrap();
        write_claim(2, &dir, "alpha", live_pid()).unwrap();
        let ids = reserve_n(3, 5, &dir, "alpha").unwrap();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn list_cluster_vms_filters_by_cluster() {
        let dir = unique_lock_dir("list");
        write_claim(1, &dir, "alpha", live_pid()).unwrap();
        write_claim(2, &dir, "beta", live_pid()).unwrap();
        write_claim(3, &dir, "alpha", live_pid()).unwrap();
        let alpha = list_cluster_vms("alpha", 5, &dir);
        assert_eq!(alpha, vec![1, 3]);
        let beta = list_cluster_vms("beta", 5, &dir);
        assert_eq!(beta, vec![2]);
        assert!(list_cluster_vms("gamma", 5, &dir).is_empty());
    }

    #[test]
    fn probe_holder_ignores_corrupt_claim() {
        let dir = unique_lock_dir("corrupt");
        fs::write(claim_path(1, &dir), "not json at all").unwrap();
        assert!(probe_holder(1, &dir).is_none());
    }

    #[test]
    fn default_lock_dir_ends_in_steampipe() {
        // Avoid env-mutating tests (they would race other tests in the same
        // binary); just assert the suffix that holds in either branch.
        assert!(default_lock_dir().ends_with("steampipe"));
    }
}
