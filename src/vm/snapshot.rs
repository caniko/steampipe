use std::path::{Path, PathBuf};

use crate::core::config::ClusterConfig;
use crate::core::state;

fn snapshots_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("snapshots")
}

/// Validate that a snapshot name is safe (no path separators or special names).
fn validate_snapshot_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("snapshot name cannot be empty");
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        anyhow::bail!("snapshot name contains invalid characters: {name:?}");
    }
    Ok(())
}

/// Save a snapshot of VM state directories.
pub fn save<S>(config: &ClusterConfig<S>, name: &str) -> anyhow::Result<()> {
    validate_snapshot_name(name)?;
    let snap_dir = snapshots_dir(&config.state_dir).join(name);
    if snap_dir.exists() {
        anyhow::bail!(
            "Snapshot '{name}' already exists. Delete it first or choose a different name."
        );
    }
    std::fs::create_dir_all(&snap_dir)?;

    println!("==> Saving snapshot '{name}'...");

    for vm in &config.vms {
        // Check if VM is running — warn user
        if let Some(pid) = state::read_pid(&config.state_dir, &vm.name)
            && state::is_pid_alive(pid)
        {
            eprintln!(
                "  WARNING: {} is running (PID {pid}). Snapshot may be inconsistent.",
                vm.name
            );
        }

        let vm_state_dir = config.state_dir.join(&vm.name);
        if !vm_state_dir.exists() {
            println!("  {}: no state dir, skipping", vm.name);
            continue;
        }

        let vm_snap = snap_dir.join(&vm.name);
        std::fs::create_dir_all(&vm_snap)?;

        // Copy all files (except sockets) from vm state dir
        copy_dir_contents(&vm_state_dir, &vm_snap)?;
        println!("  {}: saved", vm.name);
    }

    // Save metadata
    let meta = SnapshotMeta {
        created: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        vm_count: config.vms.len(),
    };
    let meta_path = snap_dir.join("snapshot.json");
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;

    println!("==> Snapshot '{name}' saved");
    Ok(())
}

/// Restore a snapshot.
pub fn restore<S>(config: &ClusterConfig<S>, name: &str) -> anyhow::Result<()> {
    validate_snapshot_name(name)?;
    let snap_dir = snapshots_dir(&config.state_dir).join(name);
    if !snap_dir.exists() {
        anyhow::bail!("Snapshot '{name}' not found");
    }

    // Check no VMs are running
    for vm in &config.vms {
        if let Some(pid) = state::read_pid(&config.state_dir, &vm.name)
            && state::is_pid_alive(pid)
        {
            anyhow::bail!(
                "{} is still running (PID {pid}). Stop all VMs before restoring.",
                vm.name
            );
        }
    }

    println!("==> Restoring snapshot '{name}'...");

    for vm in &config.vms {
        let vm_snap = snap_dir.join(&vm.name);
        if !vm_snap.exists() {
            println!("  {}: not in snapshot, skipping", vm.name);
            continue;
        }

        let vm_state_dir = config.state_dir.join(&vm.name);
        // Clear existing state
        if vm_state_dir.exists() {
            std::fs::remove_dir_all(&vm_state_dir)?;
        }
        std::fs::create_dir_all(&vm_state_dir)?;

        copy_dir_contents(&vm_snap, &vm_state_dir)?;
        println!("  {}: restored", vm.name);
    }

    println!("==> Snapshot '{name}' restored");
    Ok(())
}

/// List available snapshots.
pub fn list<S>(config: &ClusterConfig<S>) -> anyhow::Result<()> {
    let dir = snapshots_dir(&config.state_dir);
    if !dir.exists() {
        println!("No snapshots found.");
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        println!("No snapshots found.");
        return Ok(());
    }

    println!("{:<20} {:<22} {:<6}", "Name", "Created", "VMs");
    println!("{}", "─".repeat(50));

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta_path = entry.path().join("snapshot.json");
        let (created, vms) = if let Ok(contents) = std::fs::read_to_string(&meta_path) {
            if let Ok(meta) = serde_json::from_str::<SnapshotMeta>(&contents) {
                (meta.created, meta.vm_count.to_string())
            } else {
                ("?".into(), "?".into())
            }
        } else {
            ("?".into(), "?".into())
        };
        println!("{:<20} {:<22} {:<6}", name, created, vms);
    }

    Ok(())
}

/// Delete a snapshot.
pub fn delete<S>(config: &ClusterConfig<S>, name: &str) -> anyhow::Result<()> {
    validate_snapshot_name(name)?;
    let snap_dir = snapshots_dir(&config.state_dir).join(name);
    if !snap_dir.exists() {
        anyhow::bail!("Snapshot '{name}' not found");
    }
    std::fs::remove_dir_all(&snap_dir)?;
    println!("Snapshot '{name}' deleted.");
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotMeta {
    created: String,
    vm_count: usize,
}

/// Copy directory contents (non-recursive for files, skipping sockets).
fn copy_dir_contents(src: &Path, dst: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(src)?.flatten() {
        let path = entry.path();
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();

        if state::is_socket_file(&fname_str) {
            continue;
        }

        let dest = dst.join(&fname);
        if path.is_dir() {
            std::fs::create_dir_all(&dest)?;
            copy_dir_contents(&path, &dest)?;
        } else {
            std::fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ClusterConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_snapshot_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "steampipe-snapshot-test-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn validate_snapshot_name_rejects_empty() {
        assert!(validate_snapshot_name("").is_err());
    }

    #[test]
    fn validate_snapshot_name_rejects_path_traversal() {
        assert!(validate_snapshot_name("..").is_err());
        assert!(validate_snapshot_name("../etc").is_err());
        assert!(validate_snapshot_name("foo/bar").is_err());
        assert!(validate_snapshot_name("foo\\bar").is_err());
    }

    #[test]
    fn validate_snapshot_name_accepts_valid() {
        assert!(validate_snapshot_name("my-snapshot").is_ok());
        assert!(validate_snapshot_name("before-update").is_ok());
        assert!(validate_snapshot_name("v1.0").is_ok());
    }

    #[test]
    fn snapshot_round_trip_with_non_sequential_vm() {
        let root = temp_snapshot_dir("round-trip");
        let state_dir = root.join("state");
        let vm_state_dir = state_dir.join("vm-3");
        std::fs::create_dir_all(&vm_state_dir).unwrap();
        std::fs::write(vm_state_dir.join("save.dat"), "before").unwrap();

        let mut config = ClusterConfig::for_test_ids(&[3]);
        config.state_dir = state_dir.clone();

        save(&config, "after-login").unwrap();

        assert!(
            root.join("state/snapshots/after-login/vm-3/save.dat")
                .exists()
        );
        assert!(!root.join("state/snapshots/after-login/vm-1").exists());

        std::fs::write(vm_state_dir.join("save.dat"), "after").unwrap();
        restore(&config, "after-login").unwrap();

        let restored = std::fs::read_to_string(vm_state_dir.join("save.dat")).unwrap();
        assert_eq!(restored, "before");

        let _ = std::fs::remove_dir_all(root);
    }
}
