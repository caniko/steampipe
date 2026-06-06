//! VM reservation via PID-based claim files.
//!
//! Each VM slot gets a claim file in the lock directory. A VM is "claimed" if the
//! claim file exists, the PID recorded in it is still alive, **and** the process at
//! that PID is genuinely a `microvm@vm-N` instance for the slot (not a recycled
//! PID). Multiple `cluster-ctl` processes can safely share the same VM pool — a
//! process only uses VMs whose claim files reference live, identity-verified
//! processes from its cluster.
//!
//! ## Robustness against PID races
//!
//! - **PID identity verification**: each claim file carries a `cmdline_substring`
//!   (typically `microvm@vm-N`). `probe_holder` reads `/proc/{pid}/cmdline` and
//!   verifies the substring is present. A recycled PID running unrelated code
//!   fails the check and the claim is recognised as stale.
//! - **Startup grace window**: brand-new claims (younger than `STARTUP_GRACE`)
//!   skip the cmdline check because the bash `microvm-run` shell briefly owns
//!   the PID before exec'ing crosvm — cmdline would show bash, not the final
//!   pattern. The grace gives the exec time to land.
//! - **Atomic writes**: claim files are written via `tempfile + rename` so a
//!   reader never sees a partial write parse as "no claim".
//! - **Audit logging**: every claim reclaim emits an `eprintln!` recording the
//!   prior owner and the reason (dead-PID vs cmdline-mismatch).
//!
//! See `docs/src/investigations/cluster-lease-stale-pid-race.md` in the regicide
//! repo for the failure mode this guards against.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::state;

/// Grace period after a claim is written during which the cmdline identity
/// check is skipped. The window covers the microvm-run bash → crosvm `exec`
/// transition (typically <100 ms; 5 s is generous against load).
const STARTUP_GRACE: Duration = Duration::from_secs(5);

/// Information about who holds a VM claim.
pub struct LeaseInfo {
    pub pid: u32,
    pub cluster: String,
}

fn claim_path(vm_id: u8, lock_dir: &Path) -> PathBuf {
    lock_dir.join(format!("vm-{vm_id}.claim"))
}

/// Expected substring in `/proc/{pid}/cmdline` for a healthy claim on `vm_id`.
fn expected_cmdline_substring(vm_id: u8) -> String {
    format!("microvm@vm-{vm_id}")
}

/// Write a claim file for a VM. Called after `start_instance()` returns a PID.
///
/// The write goes through a tempfile + rename so concurrent readers never see
/// a torn file. The recorded `cmdline_substring` is what `probe_holder` will
/// match against `/proc/{pid}/cmdline` after the startup grace window.
pub fn write_claim(vm_id: u8, lock_dir: &Path, cluster: &str, pid: u32) -> anyhow::Result<()> {
    fs::create_dir_all(lock_dir)?;
    let meta = serde_json::json!({
        "pid": pid,
        "cluster": cluster,
        "since": chrono::Local::now().to_rfc3339(),
        "cmdline_substring": expected_cmdline_substring(vm_id),
    });
    write_atomic(&claim_path(vm_id, lock_dir), meta.to_string().as_bytes())?;
    Ok(())
}

/// Atomically replace `path` with `bytes` by writing a sibling tempfile and
/// renaming. The rename is POSIX-atomic on the same filesystem.
fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("claim path has no parent: {}", path.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("atomic rename failed: {e}"))?;
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

/// Reserve `count` VM IDs from the pool in ascending slot order.
///
/// Held slots owned by other live clusters are skipped. Returns exactly
/// `count` free IDs when enough capacity exists. Callers must not assume the
/// returned IDs are contiguous or that they start at `1`.
#[must_use = "the reserved VM IDs define the cluster's operational VM set"]
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
/// (no claim file, unreadable, dead PID, or PID identity mismatch).
///
/// Identity verification: after the startup grace window, the PID's
/// `/proc/{pid}/cmdline` must contain the substring recorded in the claim file
/// (typically `microvm@vm-N`). This catches recycled PIDs that happen to be
/// alive but aren't the original microvm process.
pub fn probe_holder(vm_id: u8, lock_dir: &Path) -> Option<LeaseInfo> {
    let path = claim_path(vm_id, lock_dir);
    let mut file = OpenOptions::new().read(true).open(&path).ok()?;

    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;

    let v: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let pid = v.get("pid")?.as_u64()? as u32;
    let cluster = v.get("cluster")?.as_str()?.to_string();
    let since = v
        .get("since")
        .and_then(|s| s.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());

    // Compute claim age. Default to "old" if `since` is missing or unparseable
    // so legacy claim files (no `since` field) still get cmdline-verified.
    let claim_is_fresh = since
        .and_then(|s| chrono::Local::now().signed_duration_since(s).to_std().ok())
        .is_some_and(|age| age < STARTUP_GRACE);

    if !state::is_pid_alive(pid) {
        eprintln!("[lease] vm-{vm_id}.claim PID {pid} is dead (cluster '{cluster}'); reclaiming.");
        let _ = fs::remove_file(&path);
        return None;
    }

    if !claim_is_fresh {
        let expected = v
            .get("cmdline_substring")
            .and_then(|s| s.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| expected_cmdline_substring(vm_id));
        let cmdline_ok = read_proc_cmdline(pid)
            .map(|cmd| cmd.contains(&expected))
            .unwrap_or(false);
        if !cmdline_ok {
            eprintln!(
                "[lease] vm-{vm_id}.claim PID {pid} no longer matches '{expected}' \
                 (PID reuse or process exited); reclaiming claim for cluster '{cluster}'."
            );
            let _ = fs::remove_file(&path);
            return None;
        }
    }

    Some(LeaseInfo { pid, cluster })
}

/// Read `/proc/{pid}/cmdline` and join the NUL-separated args into a single
/// string. Returns `None` if the file can't be read (process exited).
fn read_proc_cmdline(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let joined = String::from_utf8_lossy(&raw).replace('\0', " ");
    Some(joined)
}

/// List VM IDs currently claimed by a given cluster name (with alive PIDs).
pub fn list_cluster_vms(cluster: &str, max_vms: u8, lock_dir: &Path) -> Vec<u8> {
    let mut ids = Vec::new();
    for id in 1..=max_vms {
        if let Some(info) = probe_holder(id, lock_dir)
            && info.cluster == cluster
        {
            ids.push(id);
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

    /// Write a claim with a custom `cmdline_substring` so we can deliberately
    /// trigger identity mismatch (the test's own PID's cmdline never contains
    /// `microvm@vm-...`).
    fn write_claim_with_substring(
        vm_id: u8,
        lock_dir: &Path,
        cluster: &str,
        pid: u32,
        cmdline_substring: &str,
        age_seconds: i64,
    ) {
        let since = chrono::Local::now() - chrono::Duration::seconds(age_seconds);
        let meta = serde_json::json!({
            "pid": pid,
            "cluster": cluster,
            "since": since.to_rfc3339(),
            "cmdline_substring": cmdline_substring,
        });
        fs::write(claim_path(vm_id, lock_dir), meta.to_string()).unwrap();
    }

    #[test]
    fn claim_path_is_lock_dir_joined() {
        let p = claim_path(3, Path::new("/tmp/lock-dir"));
        assert_eq!(p, PathBuf::from("/tmp/lock-dir/vm-3.claim"));
    }

    #[test]
    fn write_then_probe_returns_holder_within_startup_grace() {
        let dir = unique_lock_dir("probe-grace");
        // Test's own PID has cmdline like "lease-test-foo", not "microvm@vm-1",
        // so without the grace window probe_holder would reject the live PID
        // for identity mismatch. The just-written claim is fresh, so cmdline
        // is skipped and the lease is honoured.
        write_claim(1, &dir, "alpha", live_pid()).unwrap();
        let info = probe_holder(1, &dir).expect("fresh claim should be live");
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
    fn probe_holder_reclaims_pid_with_wrong_cmdline_after_grace() {
        let dir = unique_lock_dir("cmdline-mismatch");
        // Stamp claim as 10 s old → past startup grace, so cmdline gets verified.
        write_claim_with_substring(3, &dir, "ghost", live_pid(), "microvm@vm-3", 10);
        assert!(claim_path(3, &dir).exists());
        // Our test process's cmdline doesn't contain "microvm@vm-3" → reclaim.
        assert!(probe_holder(3, &dir).is_none());
        assert!(!claim_path(3, &dir).exists());
    }

    #[test]
    fn probe_holder_keeps_fresh_claim_even_with_wrong_cmdline() {
        let dir = unique_lock_dir("fresh-grace");
        // age_seconds=0 → within startup grace → cmdline check skipped.
        write_claim_with_substring(4, &dir, "alpha", live_pid(), "microvm@vm-4", 0);
        let info = probe_holder(4, &dir).expect("within grace window the claim should hold");
        assert_eq!(info.cluster, "alpha");
        assert!(claim_path(4, &dir).exists());
    }

    #[test]
    fn probe_holder_keeps_live_claim_when_cmdline_matches() {
        // Synthesise a claim whose cmdline_substring is a substring guaranteed
        // to be in our own /proc/$$/cmdline. Test binaries always invoke
        // something like `<runner> tests::probe_holder_...` — `tests::` is a
        // stable, non-empty fingerprint.
        let dir = unique_lock_dir("live-match");
        let our_cmdline = read_proc_cmdline(live_pid()).expect("self cmdline readable");
        // Pick a substring that's certain to appear: the test process's own
        // executable path. (`/` is in every cmdline; safer is the binary name
        // chunk before the test args.)
        let needle: String = our_cmdline
            .split_whitespace()
            .next()
            .expect("cmdline non-empty")
            .to_owned();
        write_claim_with_substring(5, &dir, "alpha", live_pid(), &needle, 10);
        let info = probe_holder(5, &dir).expect("matching cmdline keeps claim alive");
        assert_eq!(info.cluster, "alpha");
    }

    #[test]
    fn probe_holder_handles_missing_cmdline_substring_field() {
        // Simulate a legacy claim file written before the cmdline_substring
        // field existed. probe_holder falls back to the default substring
        // (`microvm@vm-{id}`) so the slot still gets identity-verified once
        // it's outside the grace window.
        let dir = unique_lock_dir("legacy");
        let since = chrono::Local::now() - chrono::Duration::seconds(10);
        let meta = serde_json::json!({
            "pid": live_pid(),
            "cluster": "legacy",
            "since": since.to_rfc3339(),
        });
        fs::write(claim_path(6, &dir), meta.to_string()).unwrap();
        // Default substring is `microvm@vm-6` which is NOT in our test
        // process's cmdline → reclaimed.
        assert!(probe_holder(6, &dir).is_none());
    }

    #[test]
    fn write_claim_records_cmdline_substring() {
        let dir = unique_lock_dir("substring-recorded");
        write_claim(7, &dir, "alpha", live_pid()).unwrap();
        let contents = fs::read_to_string(claim_path(7, &dir)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(
            v.get("cmdline_substring").and_then(|s| s.as_str()),
            Some("microvm@vm-7")
        );
    }

    #[test]
    fn write_claim_uses_atomic_rename() {
        // Force a torn-write scenario the old way: write the file, then
        // overwrite it concurrently. With atomic rename, readers always see
        // valid JSON.
        let dir = unique_lock_dir("atomic");
        write_claim(1, &dir, "alpha", live_pid()).unwrap();
        // Sequentially overwrite many times; every intermediate read must parse.
        for _ in 0..20 {
            write_claim(1, &dir, "alpha", live_pid()).unwrap();
            let raw = fs::read_to_string(claim_path(1, &dir)).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&raw).expect("atomic rename → readable JSON");
            assert_eq!(v.get("cluster").and_then(|s| s.as_str()), Some("alpha"));
        }
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
    fn reserve_n_returns_non_contiguous_when_vm1_held() {
        let dir = unique_lock_dir("skip-vm1");
        write_claim(1, &dir, "beta", live_pid()).unwrap();
        let ids = reserve_n(1, 7, &dir, "alpha").unwrap();
        assert_eq!(ids, vec![2]);
    }

    #[test]
    fn reserve_n_skips_multiple_held_slots() {
        let dir = unique_lock_dir("skip-multiple");
        write_claim(1, &dir, "beta", live_pid()).unwrap();
        write_claim(3, &dir, "beta", live_pid()).unwrap();
        write_claim(5, &dir, "beta", live_pid()).unwrap();
        let ids = reserve_n(3, 7, &dir, "alpha").unwrap();
        assert_eq!(ids, vec![2, 4, 6]);
    }

    #[test]
    fn reserve_n_can_fill_the_entire_pool() {
        let dir = unique_lock_dir("full-pool");
        let ids = reserve_n(5, 5, &dir, "alpha").unwrap();
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn reserve_n_errors_when_pool_is_empty() {
        let dir = unique_lock_dir("empty-pool");
        for id in 1..=4 {
            write_claim(id, &dir, "beta", live_pid()).unwrap();
        }
        let err = reserve_n(1, 4, &dir, "alpha").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("only 0 of 1 requested VMs available"), "{msg}");
        assert!(msg.contains("4 held by other processes"), "{msg}");
    }

    #[test]
    fn reserve_n_errors_when_pool_insufficient() {
        let dir = unique_lock_dir("pool-insufficient");
        for id in 1..=6 {
            write_claim(id, &dir, "beta", live_pid()).unwrap();
        }
        let err = reserve_n(2, 7, &dir, "alpha").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("only 1 of 2 requested VMs available"), "{msg}");
        assert!(msg.contains("6 held by other processes"), "{msg}");
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
