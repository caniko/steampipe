use std::path::Path;

/// Ensure the state directory exists.
pub fn ensure_state_dir(state_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    Ok(())
}

/// Read a PID from a PID file. Returns None if the file doesn't exist or is invalid.
pub fn read_pid(state_dir: &Path, vm_name: &str) -> Option<u32> {
    let pid_file = state_dir.join(format!("{vm_name}.pid"));
    std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}

/// Write a PID to a PID file.
pub fn write_pid(state_dir: &Path, vm_name: &str, pid: u32) -> anyhow::Result<()> {
    let pid_file = state_dir.join(format!("{vm_name}.pid"));
    std::fs::write(pid_file, pid.to_string())?;
    Ok(())
}

/// Remove a PID file.
pub fn remove_pid(state_dir: &Path, vm_name: &str) {
    let pid_file = state_dir.join(format!("{vm_name}.pid"));
    let _ = std::fs::remove_file(pid_file);
}

/// Read the virtiofsd helper PIDs for `vm_name`.
///
/// The file may contain one PID per line: the supervisord-bypass code in
/// `microvm_start` spawns each `[program:*]` from the per-VM supervisord
/// conf separately and appends every child PID here, so `microvm_stop`
/// can reap them all. Returns an empty Vec if the file is absent or
/// contains no parseable PIDs.
pub fn read_virtiofsd_pids(state_dir: &Path, vm_name: &str) -> Vec<u32> {
    let pid_file = state_dir.join(format!("{vm_name}.virtiofsd.pid"));
    let Ok(contents) = std::fs::read_to_string(pid_file) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// Write the virtiofsd helper PIDs for `vm_name`, one per line.
pub fn write_virtiofsd_pids(
    state_dir: &Path,
    vm_name: &str,
    pids: &[u32],
) -> anyhow::Result<()> {
    let pid_file = state_dir.join(format!("{vm_name}.virtiofsd.pid"));
    let body: String = pids
        .iter()
        .map(|pid| format!("{pid}\n"))
        .collect();
    std::fs::write(pid_file, body)?;
    Ok(())
}

/// Remove the virtiofsd helper PID file.
pub fn remove_virtiofsd_pids(state_dir: &Path, vm_name: &str) {
    let pid_file = state_dir.join(format!("{vm_name}.virtiofsd.pid"));
    let _ = std::fs::remove_file(pid_file);
}

/// Check if a process is alive by reading /proc/<pid>/status.
pub fn is_pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Check if a filename looks like a leftover socket file.
pub fn is_socket_file(name: &str) -> bool {
    name.ends_with(".sock") || name.contains(".vsock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_socket_file_matches() {
        assert!(is_socket_file("console.sock"));
        assert!(is_socket_file("vm-1.vsock.3"));
        assert!(is_socket_file("monitor.sock"));
    }

    #[test]
    fn is_socket_file_rejects() {
        assert!(!is_socket_file("vm.log"));
        assert!(!is_socket_file("vm.pid"));
        assert!(!is_socket_file(""));
    }

    #[test]
    fn pid_roundtrip() {
        let dir = std::env::temp_dir().join("steampipe-test-state");
        let _ = std::fs::create_dir_all(&dir);

        write_pid(&dir, "test-vm", 12345).unwrap();
        assert_eq!(read_pid(&dir, "test-vm"), Some(12345));

        remove_pid(&dir, "test-vm");
        assert_eq!(read_pid(&dir, "test-vm"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn virtiofsd_pids_roundtrip() {
        let dir = std::env::temp_dir().join("steampipe-test-state-virtiofsd");
        let _ = std::fs::create_dir_all(&dir);

        write_virtiofsd_pids(&dir, "test-vm", &[9999, 9998]).unwrap();
        assert_eq!(read_virtiofsd_pids(&dir, "test-vm"), vec![9999, 9998]);
        // Distinct slot from the microvm PID.
        write_pid(&dir, "test-vm", 1111).unwrap();
        assert_eq!(read_pid(&dir, "test-vm"), Some(1111));
        assert_eq!(read_virtiofsd_pids(&dir, "test-vm"), vec![9999, 9998]);

        remove_virtiofsd_pids(&dir, "test-vm");
        assert!(read_virtiofsd_pids(&dir, "test-vm").is_empty());
        assert_eq!(read_pid(&dir, "test-vm"), Some(1111));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn virtiofsd_pids_handles_single_line_legacy_format() {
        // Pre-bypass code wrote one PID per VM. The new reader must accept
        // that shape too so cluster-ctl can clean up after an upgrade.
        let dir = std::env::temp_dir().join("steampipe-test-state-virtiofsd-legacy");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("legacy-vm.virtiofsd.pid"), "4242").unwrap();
        assert_eq!(read_virtiofsd_pids(&dir, "legacy-vm"), vec![4242]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
