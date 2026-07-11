use std::path::{Path, PathBuf};

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

/// Check if a process is alive by reading /proc/<pid>/status.
pub fn is_pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Read a process command line from procfs.
pub fn process_cmdline(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let args = bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .filter_map(|arg| String::from_utf8(arg.to_vec()).ok())
        .collect::<Vec<_>>();
    (!args.is_empty()).then_some(args)
}

/// Read a process current working directory from procfs.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Read a process group id from procfs.
pub fn process_group_id(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    fields.get(2)?.parse().ok()
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
}
