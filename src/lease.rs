use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;

/// A held VM lease. The flock is released when this is dropped.
pub struct VmLease {
    _file: File,
    pub vm_id: u8,
}

/// Information about who holds a VM lock (read from lock file metadata).
#[allow(dead_code)]
pub struct LeaseInfo {
    pub pid: u32,
    pub cluster: String,
    pub since: String,
}

/// Try to exclusively lock a single VM. Returns `None` if already held by another process.
pub fn try_reserve(vm_id: u8, lock_dir: &Path, cluster: &str) -> anyhow::Result<Option<VmLease>> {
    fs::create_dir_all(lock_dir)?;

    let path = lock_dir.join(format!("vm-{vm_id}.lock"));
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;

    if file.try_lock_exclusive().is_err() {
        return Ok(None);
    }

    // Write metadata into the locked file
    let meta = serde_json::json!({
        "pid": std::process::id(),
        "cluster": cluster,
        "since": chrono::Local::now().to_rfc3339(),
    });
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(meta.to_string().as_bytes())?;
    file.flush()?;

    Ok(Some(VmLease { _file: file, vm_id }))
}

/// Reserve `count` VMs from the pool, skipping locked ones.
/// Returns an error if fewer than `count` are available.
pub fn reserve_n(
    count: u8,
    max_vms: u8,
    lock_dir: &Path,
    cluster: &str,
) -> anyhow::Result<Vec<VmLease>> {
    let mut leases = Vec::with_capacity(count as usize);
    let mut held_count = 0u8;

    for id in 1..=max_vms {
        if leases.len() == count as usize {
            break;
        }
        match try_reserve(id, lock_dir, cluster)? {
            Some(lease) => leases.push(lease),
            None => {
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
    }

    if leases.len() < count as usize {
        // Release what we got
        drop(leases);
        let available = max_vms - held_count;
        anyhow::bail!(
            "only {} of {} requested VMs available ({} held by other processes)",
            available,
            count,
            held_count
        );
    }

    Ok(leases)
}

/// Probe a lock file to see who holds it. Returns `None` if the VM is free.
pub fn probe_holder(vm_id: u8, lock_dir: &Path) -> Option<LeaseInfo> {
    let path = lock_dir.join(format!("vm-{vm_id}.lock"));
    let file = OpenOptions::new().read(true).write(true).open(&path).ok()?;

    if file.try_lock_exclusive().is_ok() {
        // We got the lock — VM is free. Unlock immediately.
        let _ = file.unlock();
        return None;
    }

    // Lock is held by someone else — read metadata
    let mut contents = String::new();
    let mut file = file;
    file.read_to_string(&mut contents).ok()?;

    let v: serde_json::Value = serde_json::from_str(&contents).ok()?;
    Some(LeaseInfo {
        pid: v.get("pid")?.as_u64()? as u32,
        cluster: v.get("cluster")?.as_str()?.to_string(),
        since: v.get("since")?.as_str()?.to_string(),
    })
}

/// List VM IDs currently leased by a given cluster name.
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
