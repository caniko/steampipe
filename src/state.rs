use std::path::{Path, PathBuf};

/// Ensure the state directory exists and return it.
pub fn ensure_state_dir(state_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(state_dir)?;
    Ok(state_dir.to_owned())
}

/// Read a PID from a PID file. Returns None if the file doesn't exist or is invalid.
pub fn read_pid(state_dir: &Path, vm_name: &str) -> Option<u32> {
    let pid_file = state_dir.join(format!("{vm_name}.pid"));
    std::fs::read_to_string(pid_file)
        .ok()?
        .trim()
        .parse()
        .ok()
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
