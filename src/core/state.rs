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

/// Read the virtiofsd helper PID for `vm_name`. None if no PID file or invalid.
pub fn read_virtiofsd_pid(state_dir: &Path, vm_name: &str) -> Option<u32> {
    let pid_file = state_dir.join(format!("{vm_name}.virtiofsd.pid"));
    std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}

/// Write the virtiofsd helper PID for `vm_name`.
pub fn write_virtiofsd_pid(state_dir: &Path, vm_name: &str, pid: u32) -> anyhow::Result<()> {
    let pid_file = state_dir.join(format!("{vm_name}.virtiofsd.pid"));
    std::fs::write(pid_file, pid.to_string())?;
    Ok(())
}

/// Remove the virtiofsd helper PID file.
pub fn remove_virtiofsd_pid(state_dir: &Path, vm_name: &str) {
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
    fn virtiofsd_pid_roundtrip() {
        let dir = std::env::temp_dir().join("steampipe-test-state-virtiofsd");
        let _ = std::fs::create_dir_all(&dir);

        write_virtiofsd_pid(&dir, "test-vm", 9999).unwrap();
        assert_eq!(read_virtiofsd_pid(&dir, "test-vm"), Some(9999));
        // Distinct slot from the microvm PID.
        write_pid(&dir, "test-vm", 1111).unwrap();
        assert_eq!(read_pid(&dir, "test-vm"), Some(1111));
        assert_eq!(read_virtiofsd_pid(&dir, "test-vm"), Some(9999));

        remove_virtiofsd_pid(&dir, "test-vm");
        assert_eq!(read_virtiofsd_pid(&dir, "test-vm"), None);
        assert_eq!(read_pid(&dir, "test-vm"), Some(1111));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
