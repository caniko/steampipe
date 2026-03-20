use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::config::{BridgeReady, ClusterConfig, VmDef};
use crate::ssh::SshClient;
use crate::state;

/// Backend-agnostic command output.
#[derive(Debug)]
pub struct CmdOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Execution backend for cluster instances.
#[derive(Debug, Clone)]
pub enum Backend {
    MicroVm(MicroVmBackend),
    Docker(DockerBackend),
    Local(LocalBackend),
}

// ── Backend structs ──

/// Wraps SSH/rsync + microvm-run for NixOS microVMs.
#[derive(Debug, Clone)]
pub struct MicroVmBackend {
    ssh: SshClient,
}

/// Docker containers via `docker exec` / `docker cp`.
#[derive(Debug, Clone)]
pub struct DockerBackend {
    pub image: String,
    pub network: String,
}

/// Local processes on the host (no isolation).
#[derive(Debug, Clone)]
pub struct LocalBackend {
    pub work_dir: PathBuf,
}

// ── Constructors ──

impl MicroVmBackend {
    pub fn new(ssh_key: &Path, user: &str) -> Self {
        Self { ssh: SshClient::new(ssh_key, user) }
    }
}

impl Backend {
    pub fn new_microvm(ssh_key: &Path, user: &str) -> Self {
        Self::MicroVm(MicroVmBackend::new(ssh_key, user))
    }

    pub fn new_docker(image: &str, network: &str) -> Self {
        Self::Docker(DockerBackend {
            image: image.to_owned(),
            network: network.to_owned(),
        })
    }

    pub fn new_local(work_dir: PathBuf) -> Self {
        Self::Local(LocalBackend { work_dir })
    }
}

// ── Dispatch ──

impl Backend {
    /// Run a command on a remote instance.
    pub async fn run_cmd(&self, ip: &str, cmd: &str) -> CmdOutput {
        match self {
            Self::MicroVm(b) => {
                let out = b.ssh.run(ip, cmd).await;
                CmdOutput { stdout: out.stdout, stderr: out.stderr, success: out.success }
            }
            Self::Docker(b) => b.docker_exec(ip, cmd).await,
            Self::Local(b) => b.local_exec(cmd).await,
        }
    }

    /// Run a command with a timeout.
    pub async fn run_cmd_timeout(&self, ip: &str, cmd: &str, timeout: Duration) -> CmdOutput {
        match self {
            Self::MicroVm(b) => {
                let out = b.ssh.run_with_timeout(ip, cmd, timeout).await;
                CmdOutput { stdout: out.stdout, stderr: out.stderr, success: out.success }
            }
            Self::Docker(b) => {
                match tokio::time::timeout(timeout, b.docker_exec(ip, cmd)).await {
                    Ok(output) => output,
                    Err(_) => CmdOutput {
                        stdout: String::new(),
                        stderr: "Command timed out".into(),
                        success: false,
                    },
                }
            }
            Self::Local(b) => {
                match tokio::time::timeout(timeout, b.local_exec(cmd)).await {
                    Ok(output) => output,
                    Err(_) => CmdOutput {
                        stdout: String::new(),
                        stderr: "Command timed out".into(),
                        success: false,
                    },
                }
            }
        }
    }

    /// Spawn a long-running command with streamed stdout.
    pub fn spawn_streaming(
        &self,
        ip: &str,
        cmd: &str,
        extra_opts: &[&str],
    ) -> std::io::Result<tokio::process::Child> {
        match self {
            Self::MicroVm(b) => b.ssh.spawn_streaming(ip, cmd, extra_opts),
            Self::Docker(_) => {
                // container name is derived from ip (vm name stored elsewhere)
                // For streaming, use docker exec with stdout piped
                tokio::process::Command::new("docker")
                    .args(["exec", ip, "sh", "-c", cmd])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
            }
            Self::Local(b) => {
                tokio::process::Command::new("sh")
                    .args(["-c", cmd])
                    .current_dir(&b.work_dir)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
            }
        }
    }

    /// Upload files to a remote instance.
    pub async fn upload(
        &self,
        sources: &[&Path],
        ip: &str,
        dest: &str,
    ) -> anyhow::Result<()> {
        match self {
            Self::MicroVm(b) => b.ssh.rsync(sources, ip, dest).await,
            Self::Docker(_) => docker_cp_to(sources, ip, dest).await,
            Self::Local(b) => local_copy(sources, &b.work_dir, dest),
        }
    }

    /// Check if the instance is reachable.
    pub async fn is_reachable(&self, ip: &str) -> bool {
        match self {
            Self::MicroVm(b) => b.ssh.is_reachable(ip).await,
            Self::Docker(_) => {
                let output = tokio::process::Command::new("docker")
                    .args(["exec", ip, "true"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
                output.is_ok_and(|s| s.success())
            }
            Self::Local(_) => true,
        }
    }

    /// Wait for the instance to become reachable, with exponential backoff.
    pub async fn wait_ready(&self, ip: &str, timeout_secs: u32) -> bool {
        match self {
            Self::MicroVm(b) => b.ssh.wait_ready(ip, timeout_secs).await,
            Self::Docker(_) => {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs as u64);
                let mut delay = Duration::from_millis(500);
                loop {
                    if self.is_reachable(ip).await { return true; }
                    if tokio::time::Instant::now() >= deadline { return false; }
                    let remaining = deadline - tokio::time::Instant::now();
                    tokio::time::sleep(delay.min(remaining)).await;
                    delay = (delay * 2).min(Duration::from_secs(4));
                }
            }
            Self::Local(_) => true,
        }
    }

    /// Start an instance. Returns the PID (for backends that have one).
    pub fn start_instance(
        &self,
        config: &ClusterConfig<BridgeReady>,
        vm: &VmDef,
        runners_dir: &Path,
    ) -> anyhow::Result<u32> {
        match self {
            Self::MicroVm(_) => microvm_start(config, vm, runners_dir),
            Self::Docker(b) => docker_start(config, vm, b),
            Self::Local(_) => Ok(0), // no-op: local processes are spawned by the test harness
        }
    }

    /// Stop an instance.
    pub fn stop_instance<S>(&self, config: &ClusterConfig<S>, vm: &VmDef) {
        match self {
            Self::MicroVm(_) => microvm_stop(config, vm),
            Self::Docker(_) => docker_stop(vm),
            Self::Local(_) => {} // no-op
        }
    }

    /// Check if an instance is running.
    #[allow(dead_code)]
    pub fn is_instance_running<S>(&self, config: &ClusterConfig<S>, vm: &VmDef) -> bool {
        match self {
            Self::MicroVm(_) => {
                state::read_pid(&config.state_dir, &vm.name).is_some_and(state::is_pid_alive)
            }
            Self::Docker(_) => {
                std::process::Command::new("docker")
                    .args(["inspect", "-f", "{{.State.Running}}", &vm.name.0])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            }
            Self::Local(_) => true,
        }
    }

    /// Whether this backend supports network emulation (tc/netem on TAP devices).
    #[allow(dead_code)]
    pub fn supports_netem(&self) -> bool {
        matches!(self, Self::MicroVm(_))
    }

    /// Whether this backend supports Steam client operations.
    #[allow(dead_code)]
    pub fn supports_steam(&self) -> bool {
        matches!(self, Self::MicroVm(_) | Self::Docker(_))
    }

    /// Whether this backend supports screenshot capture.
    #[allow(dead_code)]
    pub fn supports_screenshots(&self) -> bool {
        matches!(self, Self::MicroVm(_))
    }
}

// ── MicroVM lifecycle ──

fn microvm_start(
    config: &ClusterConfig<BridgeReady>,
    vm: &VmDef,
    runners_dir: &Path,
) -> anyhow::Result<u32> {
    let runner = runners_dir.join(&vm.name).join("bin/microvm-run");
    if !runner.exists() {
        anyhow::bail!("Runner not found: {}", runner.display());
    }

    let vm_dir = config.state_dir.join(&vm.name);
    std::fs::create_dir_all(&vm_dir)?;

    let log_file = std::fs::File::create(vm_dir.join("vm.log"))?;

    let child = std::process::Command::new(&runner)
        .current_dir(&vm_dir)
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()?;

    let pid = child.id();
    state::write_pid(&config.state_dir, &vm.name, pid)?;
    Ok(pid)
}

fn microvm_stop<S>(config: &ClusterConfig<S>, vm: &VmDef) {
    if let Some(pid) = state::read_pid(&config.state_dir, &vm.name)
        && state::is_pid_alive(pid)
    {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
    state::remove_pid(&config.state_dir, &vm.name);

    let _ = std::process::Command::new("pkill")
        .args(["-f", &format!("microvm@{}", vm.name)])
        .status();

    let vm_dir = config.state_dir.join(&vm.name);
    if vm_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&vm_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && state::is_socket_file(name)
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

// ── Docker lifecycle ──

fn docker_start(
    config: &ClusterConfig<BridgeReady>,
    vm: &VmDef,
    backend: &DockerBackend,
) -> anyhow::Result<u32> {
    // Stop any existing container with this name
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", &vm.name.0])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        vm.name.0.clone(),
        "--network".to_string(),
        backend.network.clone(),
        "--ip".to_string(),
        vm.ip.0.clone(),
    ];
    args.push(backend.image.clone());
    args.push("sleep".to_string());
    args.push("infinity".to_string());

    let output = std::process::Command::new("docker")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("docker run failed for {}: {stderr}", vm.name);
    }

    // Write container ID as "pid" for state tracking
    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    state::write_pid(&config.state_dir, &vm.name, 0)?; // placeholder PID
    eprintln!("  {} container started: {}...", vm.name, &container_id[..12.min(container_id.len())]);
    Ok(0)
}

fn docker_stop(vm: &VmDef) {
    let _ = std::process::Command::new("docker")
        .args(["stop", "-t", "5", &vm.name.0])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", &vm.name.0])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl DockerBackend {
    /// Execute a command inside a Docker container.
    /// The `ip` parameter is used as the container name.
    async fn docker_exec(&self, container: &str, cmd: &str) -> CmdOutput {
        let result = tokio::process::Command::new("docker")
            .args(["exec", container, "sh", "-c", cmd])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match result {
            Ok(output) => CmdOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                success: output.status.success(),
            },
            Err(e) => CmdOutput {
                stdout: String::new(),
                stderr: format!("docker exec failed: {e}"),
                success: false,
            },
        }
    }
}

async fn docker_cp_to(sources: &[&Path], container: &str, dest: &str) -> anyhow::Result<()> {
    for src in sources {
        let output = tokio::process::Command::new("docker")
            .args(["cp", &src.to_string_lossy(), &format!("{container}:{dest}")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("docker cp {} to {container}:{dest} failed: {stderr}", src.display());
        }
    }
    Ok(())
}

// ── Local backend ──

impl LocalBackend {
    async fn local_exec(&self, cmd: &str) -> CmdOutput {
        let result = tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&self.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match result {
            Ok(output) => CmdOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                success: output.status.success(),
            },
            Err(e) => CmdOutput {
                stdout: String::new(),
                stderr: format!("local exec failed: {e}"),
                success: false,
            },
        }
    }
}

fn local_copy(sources: &[&Path], _work_dir: &Path, dest: &str) -> anyhow::Result<()> {
    let dest_path = Path::new(dest);
    std::fs::create_dir_all(dest_path)?;
    for src in sources {
        if src.is_dir() {
            // Copy directory recursively
            copy_dir_all(src, &dest_path.join(src.file_name().unwrap_or_default()))?;
        } else if let Some(name) = src.file_name() {
            std::fs::copy(src, dest_path.join(name))?;
        }
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
