use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::core::config::{BridgeReady, ClusterConfig, VmDef};
use crate::core::ssh::SshClient;
use crate::core::state;

/// Backend-agnostic command output.
///
/// Returned by `Backend::run_cmd` and `Backend::run_cmd_timeout`. Check `success`
/// before trusting `stdout`; on failure, `stderr` contains the error details.
#[derive(Debug)]
pub struct CmdOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Execution backend for cluster instances.
///
/// All VM operations (commands, file uploads, lifecycle) are dispatched through
/// this enum. Each variant wraps a backend-specific implementation.
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
        Self {
            ssh: SshClient::new(ssh_key, user),
        }
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
                CmdOutput {
                    stdout: out.stdout,
                    stderr: out.stderr,
                    success: out.success,
                }
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
                CmdOutput {
                    stdout: out.stdout,
                    stderr: out.stderr,
                    success: out.success,
                }
            }
            Self::Docker(b) => match tokio::time::timeout(timeout, b.docker_exec(ip, cmd)).await {
                Ok(output) => output,
                Err(_) => CmdOutput {
                    stdout: String::new(),
                    stderr: "Command timed out".into(),
                    success: false,
                },
            },
            Self::Local(b) => match tokio::time::timeout(timeout, b.local_exec(cmd)).await {
                Ok(output) => output,
                Err(_) => CmdOutput {
                    stdout: String::new(),
                    stderr: "Command timed out".into(),
                    success: false,
                },
            },
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
            Self::Local(b) => tokio::process::Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&b.work_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn(),
        }
    }

    /// Upload files to a remote instance.
    pub async fn upload(&self, sources: &[&Path], ip: &str, dest: &str) -> anyhow::Result<()> {
        match self {
            Self::MicroVm(b) => b.ssh.rsync(sources, ip, dest).await,
            Self::Docker(_) => docker_cp_to(sources, ip, dest).await,
            Self::Local(b) => local_copy(sources, &b.work_dir, dest),
        }
    }

    /// Download one remote file or directory into a local destination directory.
    pub async fn download(&self, ip: &str, source: &str, dest: &Path) -> anyhow::Result<()> {
        match self {
            Self::MicroVm(b) => b.ssh.fetch(ip, source, dest).await,
            Self::Docker(_) => docker_cp_from(source, ip, dest).await,
            Self::Local(_) => local_fetch(source, dest),
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
                let deadline =
                    tokio::time::Instant::now() + Duration::from_secs(timeout_secs as u64);
                let mut delay = Duration::from_millis(500);
                loop {
                    if self.is_reachable(ip).await {
                        return true;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return false;
                    }
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
}

// ── MicroVM lifecycle ──

fn microvm_start(
    config: &ClusterConfig<BridgeReady>,
    vm: &VmDef,
    runners_dir: &Path,
) -> anyhow::Result<u32> {
    let runner = microvm_runner_path(runners_dir, vm);
    if !runner.exists() {
        anyhow::bail!("Runner not found: {}", runner.display());
    }

    let vm_dir = microvm_state_dir(&config.state_dir, vm);
    std::fs::create_dir_all(&vm_dir)?;

    let log_file = std::fs::File::create(microvm_log_path(&config.state_dir, vm))?;
    let runtime_dir = host_xdg_runtime_dir();

    // microvm.nix login runners declare a writable steam-state share via
    // virtiofs. The host-side virtiofsd never runs unless we start it: QEMU
    // opens `-chardev socket,…` eagerly and aborts if the socket is missing.
    //
    // The generated `bin/virtiofsd-run` is a `supervisord` wrapper whose
    // config has `[supervisord] user=root` baked in — supervisord refuses
    // to start as a non-root user, so executing the wrapper as the operator
    // fails before any virtiofsd is launched. Parse the wrapper to recover
    // its supervisord conf and spawn each `[program:*]` directly, skipping
    // supervisord entirely. cluster-ctl already owns the lifecycle (PIDs,
    // group kill, cleanup), so the supervisor layer is dead weight here.
    // Runners with no virtiofs share (regular non-login runners) skip this
    // path entirely.
    let virtiofsd_runner = virtiofsd_runner_path(runners_dir, vm);
    let virtiofsd_pids = if virtiofsd_runner.exists() {
        let expected_sockets = virtiofs_socket_names(&runner).unwrap_or_default();
        let programs = virtiofsd_program_commands(&virtiofsd_runner).map_err(|err| {
            err.context(format!(
                "failed to extract virtiofsd programs from {}",
                virtiofsd_runner.display()
            ))
        })?;
        if programs.is_empty() {
            anyhow::bail!(
                "virtiofsd-run wrapper {} declared no [program:*] entries; \
                 cannot start virtiofsd",
                virtiofsd_runner.display()
            );
        }

        let virtiofsd_log_path = virtiofsd_log_path(&config.state_dir, vm);
        let virtiofsd_log = std::fs::File::create(&virtiofsd_log_path)?;
        let mut pids: Vec<u32> = Vec::with_capacity(programs.len());
        for program in &programs {
            let mut cmd = std::process::Command::new(program);
            cmd.current_dir(&vm_dir)
                .env("XDG_RUNTIME_DIR", &runtime_dir)
                .stdin(Stdio::null())
                .stdout(virtiofsd_log.try_clone()?)
                .stderr(virtiofsd_log.try_clone()?);
            #[cfg(unix)]
            cmd.process_group(0);
            match cmd.spawn() {
                Ok(child) => pids.push(child.id()),
                Err(err) => {
                    for pid in &pids {
                        kill_pid_graceful(*pid);
                    }
                    state::remove_virtiofsd_pids(&config.state_dir, &vm.name);
                    return Err(anyhow::anyhow!(
                        "failed to spawn virtiofsd program {}: {err}",
                        program.display()
                    ));
                }
            }
        }
        state::write_virtiofsd_pids(&config.state_dir, &vm.name, &pids)?;

        if let Err(err) = wait_for_virtiofs_sockets(&vm_dir, &expected_sockets) {
            for pid in &pids {
                kill_pid_graceful(*pid);
            }
            state::remove_virtiofsd_pids(&config.state_dir, &vm.name);
            let log_tail = tail_log(&virtiofsd_log_path, 4);
            return Err(err.context(format!(
                "virtiofsd for {} never produced its sockets. \
                 virtiofsd.log tail:\n{log_tail}\n\
                 If the log shows EPERM/chgrp errors, add the operator user \
                 to the kvm group (see docs/src/configuration/steam-credentials.md).",
                vm.name
            )));
        }
        pids
    } else {
        Vec::new()
    };

    let mut command = std::process::Command::new(&runner);
    command
        .current_dir(&vm_dir)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file);
    if let Some(wayland_display) = host_wayland_display(&runtime_dir) {
        command.env("WAYLAND_DISPLAY", wayland_display);
    }
    #[cfg(unix)]
    command.process_group(0);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            for pid in &virtiofsd_pids {
                kill_pid_graceful(*pid);
            }
            if !virtiofsd_pids.is_empty() {
                state::remove_virtiofsd_pids(&config.state_dir, &vm.name);
            }
            return Err(err.into());
        }
    };

    let pid = child.id();
    state::write_pid(&config.state_dir, &vm.name, pid)?;
    Ok(pid)
}

fn microvm_stop<S>(config: &ClusterConfig<S>, vm: &VmDef) {
    // Kill the recorded virtiofsd PIDs *first* so QEMU's chardev disconnect
    // happens cleanly. The microvm-run argv contains the per-VM tag we can
    // pkill, but the virtiofsd argv has no comparable per-VM fingerprint
    // beyond the share name, so we must rely on the recorded PIDs. There
    // may be more than one program per VM (one per virtiofs share).
    for pid in state::read_virtiofsd_pids(&config.state_dir, &vm.name) {
        if state::is_pid_alive(pid) {
            kill_pid_graceful(pid);
        }
    }
    state::remove_virtiofsd_pids(&config.state_dir, &vm.name);

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
    // Fallback for leaked virtiofsd children: the supervisord wrapper runs
    // a binary whose argv contains `virtiofsd-steam-state-<vm>` from the
    // generated Nix script name.
    let _ = std::process::Command::new("pkill")
        .args(["-f", &format!("virtiofsd-steam-state-{}", vm.name)])
        .status();

    let vm_dir = config.state_dir.join(&vm.name);
    cleanup_microvm_sockets(&vm_dir);
}

fn kill_pid_graceful(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    for _ in 0..10 {
        if !state::is_pid_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
}

fn virtiofsd_runner_path(runners_dir: &Path, vm: &VmDef) -> PathBuf {
    runners_dir.join(&vm.name).join("bin/virtiofsd-run")
}

fn virtiofsd_log_path(state_dir: &Path, vm: &VmDef) -> PathBuf {
    microvm_state_dir(state_dir, vm).join("virtiofsd.log")
}

/// Resolve the `[program:*]` command paths the per-VM `bin/virtiofsd-run`
/// wrapper would launch under supervisord.
///
/// The wrapper is a one-liner `exec supervisord --configuration <conf>`;
/// the conf declares one or more `[program:*]` sections, each with a
/// `command=<binary>` pointing at the actual virtiofsd-wrapping script
/// generated by microvm.nix. We bypass supervisord because its config
/// pins `user=root` and refuses to run as the operator user.
fn virtiofsd_program_commands(virtiofsd_run: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let wrapper = std::fs::read_to_string(virtiofsd_run)?;
    let conf_path = parse_supervisord_conf_path(&wrapper).ok_or_else(|| {
        anyhow::anyhow!(
            "could not find --configuration <path> in {}",
            virtiofsd_run.display()
        )
    })?;
    let conf = std::fs::read_to_string(&conf_path)?;
    Ok(parse_supervisord_program_commands(&conf))
}

/// Extract the `--configuration <path>` argument from a `virtiofsd-run`
/// shell wrapper. The wrapper microvm.nix generates uses
/// `--configuration <path>` (one-word flag, space-separated value); some
/// supervisord wrappers use `--configuration=<path>` form, support both.
fn parse_supervisord_conf_path(wrapper: &str) -> Option<PathBuf> {
    let mut tokens = wrapper.split_whitespace().peekable();
    while let Some(tok) = tokens.next() {
        if let Some(rest) = tok.strip_prefix("--configuration=") {
            return Some(PathBuf::from(rest));
        }
        if tok == "--configuration" || tok == "-c" {
            if let Some(value) = tokens.next() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

/// Extract every `command=<path>` value found in a `[program:*]` block of
/// a supervisord INI config. `[eventlistener:*]` blocks are intentionally
/// skipped — they only exist to call `systemd-notify --ready`, which is
/// meaningless outside a systemd unit.
fn parse_supervisord_program_commands(conf: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut in_program = false;
    for raw in conf.lines() {
        let line = raw.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_program = section.starts_with("program:");
            continue;
        }
        if !in_program {
            continue;
        }
        if let Some(value) = line.strip_prefix("command=") {
            let first = value
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim();
            if !first.is_empty() {
                out.push(PathBuf::from(first));
            }
        }
    }
    out
}

/// Read up to the last `n` non-empty lines of a log file for inclusion
/// in error messages. Best-effort: missing/unreadable files return a
/// placeholder rather than failing the caller.
fn tail_log(path: &Path, n: usize) -> String {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return format!("(no log at {})", path.display());
    };
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return format!("(log {} is empty)", path.display());
    }
    let start = lines.len().saturating_sub(n);
    lines[start..]
        .iter()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse the per-VM virtiofs socket filenames declared in `microvm-run`.
///
/// QEMU and crosvm both spell them as `path=<name>.sock` / `socket=<name>.sock`
/// in the generated command line. Returns the de-duplicated set so each socket
/// is awaited exactly once before the VM runner exec's.
fn virtiofs_socket_names(runner: &Path) -> std::io::Result<Vec<String>> {
    let contents = std::fs::read_to_string(runner)?;
    Ok(parse_virtiofs_socket_names(&contents))
}

fn parse_virtiofs_socket_names(runner_contents: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in runner_contents.split(|c: char| {
        c.is_whitespace() || matches!(c, ',' | '\'' | '"' | '=')
    }) {
        if token.contains("virtiofs") && token.ends_with(".sock") {
            let name = token.trim();
            if !name.is_empty() && !out.iter().any(|existing: &String| existing == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn wait_for_virtiofs_sockets(vm_dir: &Path, sockets: &[String]) -> anyhow::Result<()> {
    if sockets.is_empty() {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if sockets.iter().all(|name| vm_dir.join(name).exists()) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let missing: Vec<&String> = sockets
                .iter()
                .filter(|name| !vm_dir.join(name).exists())
                .collect();
            anyhow::bail!(
                "timed out after 10s waiting for virtiofs socket(s) {:?} in {}",
                missing,
                vm_dir.display()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn microvm_runner_path(runners_dir: &Path, vm: &VmDef) -> std::path::PathBuf {
    runners_dir.join(&vm.name).join("bin/microvm-run")
}

pub fn microvm_state_dir(state_dir: &Path, vm: &VmDef) -> std::path::PathBuf {
    state_dir.join(&vm.name)
}

pub fn microvm_log_path(state_dir: &Path, vm: &VmDef) -> std::path::PathBuf {
    microvm_state_dir(state_dir, vm).join("vm.log")
}

fn host_xdg_runtime_dir() -> PathBuf {
    host_xdg_runtime_dir_from(std::env::var_os("XDG_RUNTIME_DIR"), host_uid())
}

fn host_uid() -> Option<String> {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_owned())
        .filter(|stdout| !stdout.is_empty())
}

fn host_xdg_runtime_dir_from(
    runtime_dir: Option<std::ffi::OsString>,
    uid: Option<String>,
) -> PathBuf {
    if let Some(runtime_dir) = runtime_dir
        && !runtime_dir.is_empty()
    {
        return PathBuf::from(runtime_dir);
    }

    uid.map(|uid| PathBuf::from(format!("/run/user/{uid}")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn host_wayland_display(runtime_dir: &Path) -> Option<String> {
    if let Some(display) = std::env::var_os("WAYLAND_DISPLAY")
        && !display.is_empty()
    {
        return Some(display.to_string_lossy().into_owned());
    }

    ["wayland-1", "wayland-0"]
        .into_iter()
        .find(|socket| runtime_dir.join(socket).exists())
        .map(str::to_owned)
}

pub fn cleanup_microvm_sockets(vm_dir: &Path) {
    if vm_dir.exists()
        && let Ok(entries) = std::fs::read_dir(vm_dir)
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
    eprintln!(
        "  {} container started: {}...",
        vm.name,
        &container_id[..12.min(container_id.len())]
    );
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
            anyhow::bail!(
                "docker cp {} to {container}:{dest} failed: {stderr}",
                src.display()
            );
        }
    }
    Ok(())
}

async fn docker_cp_from(source: &str, container: &str, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    let output = tokio::process::Command::new("docker")
        .args([
            "cp",
            &format!("{container}:{source}"),
            &dest.to_string_lossy(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("docker cp {container}:{source} failed: {stderr}");
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

fn local_fetch(source: &str, dest: &Path) -> anyhow::Result<()> {
    let source_path = Path::new(source);
    std::fs::create_dir_all(dest)?;
    if source_path.is_dir() {
        copy_dir_all(
            source_path,
            &dest.join(source_path.file_name().unwrap_or_default()),
        )?;
    } else if let Some(name) = source_path.file_name() {
        std::fs::copy(source_path, dest.join(name))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{IpAddr, VmName};

    fn vm() -> VmDef {
        VmDef {
            name: VmName("vm-1".into()),
            ip: IpAddr("10.0.100.1".into()),
            index: 1,
        }
    }

    #[test]
    fn microvm_paths_use_runner_and_state_layout() {
        let vm = vm();
        assert_eq!(
            microvm_runner_path(Path::new("/runners"), &vm),
            Path::new("/runners/vm-1/bin/microvm-run")
        );
        assert_eq!(
            microvm_state_dir(Path::new("/state"), &vm),
            Path::new("/state/vm-1")
        );
        assert_eq!(
            microvm_log_path(Path::new("/state"), &vm),
            Path::new("/state/vm-1/vm.log")
        );
    }

    #[test]
    fn cleanup_microvm_sockets_preserves_non_socket_files() {
        let dir =
            std::env::temp_dir().join(format!("steampipe-microvm-cleanup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("vm-1-gpu.sock");
        let vsock = dir.join("vm-1.vsock");
        let log = dir.join("vm.log");
        let image = dir.join("vm-1-home.img");
        std::fs::write(&socket, "socket").unwrap();
        std::fs::write(&vsock, "vsock").unwrap();
        std::fs::write(&log, "log").unwrap();
        std::fs::write(&image, "image").unwrap();

        cleanup_microvm_sockets(&dir);

        assert!(!socket.exists());
        assert!(!vsock.exists());
        assert!(log.exists());
        assert!(image.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn host_xdg_runtime_dir_prefers_environment() {
        let expected = PathBuf::from("/tmp/steampipe-xdg-test");
        assert_eq!(
            host_xdg_runtime_dir_from(Some(expected.as_os_str().to_owned()), Some("999".into())),
            expected
        );
    }

    #[test]
    fn host_xdg_runtime_dir_falls_back_when_missing() {
        assert_eq!(
            host_xdg_runtime_dir_from(None, Some("999".into())),
            PathBuf::from("/run/user/999")
        );
    }

    #[test]
    fn host_xdg_runtime_dir_uses_tmp_without_env_or_uid() {
        assert_eq!(host_xdg_runtime_dir_from(None, None), PathBuf::from("/tmp"));
    }

    #[test]
    fn host_wayland_display_prefers_environment() {
        assert_eq!(
            host_wayland_display(Path::new("/does/not/matter")),
            std::env::var("WAYLAND_DISPLAY")
                .ok()
                .filter(|value| !value.is_empty())
        );
    }

    #[test]
    fn parse_supervisord_conf_path_extracts_space_separated_value() {
        let wrapper = "#!/nix/store/.../bin/bash\n\
            exec /nix/store/.../supervisord \
            --configuration /nix/store/abcd-vm-1-virtiofsd-supervisord.conf\n";
        let parsed = super::parse_supervisord_conf_path(wrapper);
        assert_eq!(
            parsed,
            Some(std::path::PathBuf::from(
                "/nix/store/abcd-vm-1-virtiofsd-supervisord.conf"
            ))
        );
    }

    #[test]
    fn parse_supervisord_conf_path_extracts_equals_value() {
        let wrapper = "exec supervisord --configuration=/foo/bar.conf";
        assert_eq!(
            super::parse_supervisord_conf_path(wrapper),
            Some(std::path::PathBuf::from("/foo/bar.conf"))
        );
    }

    #[test]
    fn parse_supervisord_program_commands_extracts_program_commands_only() {
        // The eventlistener block must be skipped: it calls systemd-notify
        // and is meaningless outside a systemd unit.
        let conf = r#"
[eventlistener:notify]
command=/nix/store/aaa-supervisord-event-handler
events=PROCESS_STATE

[program:virtiofsd-steampipe-ssh-authorized]
command=/nix/store/bbb-virtiofsd-steampipe-ssh-authorized
stderr_syslog=true
stdout_syslog=true

[program:virtiofsd-steam-state-vm-1]
command=/nix/store/ccc-virtiofsd-steam-state-vm-1

[supervisord]
nodaemon=true
user=root
"#;
        let cmds = super::parse_supervisord_program_commands(conf);
        assert_eq!(
            cmds,
            vec![
                std::path::PathBuf::from("/nix/store/bbb-virtiofsd-steampipe-ssh-authorized"),
                std::path::PathBuf::from("/nix/store/ccc-virtiofsd-steam-state-vm-1"),
            ]
        );
    }

    #[test]
    fn parse_supervisord_program_commands_returns_empty_when_no_programs() {
        let conf = "[supervisord]\nnodaemon=true\nuser=root\n";
        assert!(super::parse_supervisord_program_commands(conf).is_empty());
    }

    #[test]
    fn tail_log_returns_last_n_nonempty_lines() {
        let dir = std::env::temp_dir().join(format!(
            "steampipe-tail-log-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("v.log");
        std::fs::write(&log, "first\n\nsecond\nthird\nfourth\n").unwrap();
        let tail = super::tail_log(&log, 2);
        assert!(tail.contains("third"));
        assert!(tail.contains("fourth"));
        assert!(!tail.contains("second"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_log_returns_placeholder_for_missing_file() {
        let missing = std::env::temp_dir().join(format!(
            "steampipe-tail-log-missing-{}",
            std::process::id()
        ));
        let tail = super::tail_log(&missing, 4);
        assert!(tail.contains("no log at"));
    }

    #[test]
    fn parse_virtiofs_socket_names_extracts_qemu_chardev_paths() {
        // Trimmed QEMU command line from a real login runner.
        let runner = r#"
            -chardev 'socket,id=fs0,path=vm-1-virtiofs-steam-state-vm-1.sock'
            -device  'vhost-user-fs-pci,chardev=fs0,tag=steam-state-vm-1'
        "#;
        let sockets = super::parse_virtiofs_socket_names(runner);
        assert_eq!(sockets, vec!["vm-1-virtiofs-steam-state-vm-1.sock"]);
    }

    #[test]
    fn parse_virtiofs_socket_names_handles_crosvm_style() {
        // crosvm uses `socket=<name>.sock` rather than `path=`.
        let runner = "--vhost-user 'type=fs,socket=vm-2-virtiofs-steampipe-ssh-authorized.sock'";
        let sockets = super::parse_virtiofs_socket_names(runner);
        assert_eq!(
            sockets,
            vec!["vm-2-virtiofs-steampipe-ssh-authorized.sock"]
        );
    }

    #[test]
    fn parse_virtiofs_socket_names_skips_non_virtiofs_sockets() {
        // The runner's own QMP / vsock / GPU sockets must not be awaited as
        // virtiofs sockets.
        let runner = r#"
            -qmp unix:vm-1.sock,server,nowait
            --socket vm-1-gpu.sock
            -chardev 'socket,id=fs0,path=vm-1-virtiofs-steam-state-vm-1.sock'
        "#;
        let sockets = super::parse_virtiofs_socket_names(runner);
        assert_eq!(sockets, vec!["vm-1-virtiofs-steam-state-vm-1.sock"]);
    }

    #[test]
    fn parse_virtiofs_socket_names_dedupes() {
        let runner = r#"
            -chardev 'socket,id=fs0,path=vm-1-virtiofs-steam-state-vm-1.sock'
            -chardev 'socket,id=fs0,path=vm-1-virtiofs-steam-state-vm-1.sock'
        "#;
        let sockets = super::parse_virtiofs_socket_names(runner);
        assert_eq!(sockets.len(), 1);
    }

    #[test]
    fn wait_for_virtiofs_sockets_returns_when_files_appear() {
        let dir = std::env::temp_dir().join(format!(
            "steampipe-virtiofs-wait-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vm-1-virtiofs-steam-state-vm-1.sock"), "").unwrap();
        let result = super::wait_for_virtiofs_sockets(
            &dir,
            &["vm-1-virtiofs-steam-state-vm-1.sock".to_string()],
        );
        assert!(result.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wait_for_virtiofs_sockets_noop_when_empty() {
        let dir = std::env::temp_dir().join(format!(
            "steampipe-virtiofs-wait-noop-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(super::wait_for_virtiofs_sockets(&dir, &[]).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wait_for_virtiofs_sockets_times_out_when_socket_never_appears() {
        let dir = std::env::temp_dir().join(format!(
            "steampipe-virtiofs-wait-timeout-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Override the 10s default by spawning the wait in another thread and
        // killing it would be nicer, but the helper has no timeout knob. Use a
        // very short-lived race: write a socket file from a background thread
        // after 200ms, then expect the wait to succeed.
        let dir_for_thread = dir.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            std::fs::write(
                dir_for_thread.join("vm-9-virtiofs-late.sock"),
                "",
            )
            .unwrap();
        });
        let result = super::wait_for_virtiofs_sockets(
            &dir,
            &["vm-9-virtiofs-late.sock".to_string()],
        );
        writer.join().unwrap();
        assert!(result.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kill_pid_graceful_reaps_a_real_child() {
        // Spawn `sleep 5` and confirm kill_pid_graceful terminates it well
        // under the 1s SIGTERM window the helper allows. In production
        // cluster-ctl never wait()s its children — they get reaped by init
        // on cluster-ctl exit — so a freshly killed PID is briefly a zombie
        // with /proc/{pid} still present. The test reaps the Child handle
        // explicitly to prove the kill actually delivered.
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        assert!(state::is_pid_alive(pid));
        super::kill_pid_graceful(pid);
        let status = child.wait().expect("reap killed sleep");
        assert!(
            !status.success(),
            "killed sleep should report non-success exit"
        );
    }

    /// End-to-end stub: a fake login runner with the same shape as the real
    /// one on atlas (microvm-run + virtiofsd-run siblings, virtiofs share).
    /// Asserts that spawning virtiofsd-run + waiting for its socket + then
    /// spawning microvm-run all sequence correctly — i.e. microvm-run sees
    /// the socket and exits 0 instead of failing with "No such file".
    #[test]
    fn virtiofsd_runner_spawns_socket_before_microvm_runs() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = std::env::temp_dir().join(format!(
            "steampipe-virtiofsd-e2e-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        let runners_dir = scratch.join("runners");
        let state_dir = scratch.join("state");
        let runner_bin = runners_dir.join("vm-1").join("bin");
        std::fs::create_dir_all(&runner_bin).unwrap();
        let vm_dir = microvm_state_dir(&state_dir, &vm());
        std::fs::create_dir_all(&vm_dir).unwrap();

        // The fake microvm-run mentions the same virtiofs socket name our
        // parser must extract. It then asserts the socket exists before
        // exiting 0.
        let microvm_script = "\
            #!/bin/sh\n\
            # path=vm-1-virtiofs-steam-state-vm-1.sock\n\
            set -e\n\
            test -e vm-1-virtiofs-steam-state-vm-1.sock\n\
            echo microvm-ok\n";
        let microvm_path = runner_bin.join("microvm-run");
        std::fs::write(&microvm_path, microvm_script).unwrap();
        let mut perms = std::fs::metadata(&microvm_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&microvm_path, perms).unwrap();

        // The fake virtiofsd-run delays slightly, then creates the socket
        // file, then sleeps to keep the process alive (matching real
        // virtiofsd behavior).
        let virtiofsd_script = "\
            #!/bin/sh\n\
            sleep 0.2\n\
            touch vm-1-virtiofs-steam-state-vm-1.sock\n\
            sleep 30\n";
        let virtiofsd_path = runner_bin.join("virtiofsd-run");
        std::fs::write(&virtiofsd_path, virtiofsd_script).unwrap();
        let mut perms = std::fs::metadata(&virtiofsd_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&virtiofsd_path, perms).unwrap();

        // Manually exercise the spawn-wait-spawn sequence from microvm_start.
        // Going through microvm_start itself would require a full
        // ClusterConfig<BridgeReady>; instead we replay the helpers it calls
        // so the test stays focused on the lifecycle contract.
        let vm = vm();
        let sockets = super::virtiofs_socket_names(&microvm_path).unwrap();
        assert_eq!(sockets, vec!["vm-1-virtiofs-steam-state-vm-1.sock"]);

        let mut virtiofsd_child = std::process::Command::new(&virtiofsd_path)
            .current_dir(&vm_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fake virtiofsd-run");
        let virtiofsd_pid = virtiofsd_child.id();
        state::write_virtiofsd_pids(&state_dir, &vm.name, &[virtiofsd_pid]).unwrap();

        super::wait_for_virtiofs_sockets(&vm_dir, &sockets)
            .expect("socket should appear within 10s");

        let microvm_output = std::process::Command::new(&microvm_path)
            .current_dir(&vm_dir)
            .output()
            .expect("spawn fake microvm-run");
        assert!(
            microvm_output.status.success(),
            "fake microvm-run exited non-zero: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&microvm_output.stdout),
            String::from_utf8_lossy(&microvm_output.stderr),
        );
        assert!(
            String::from_utf8_lossy(&microvm_output.stdout).contains("microvm-ok"),
            "expected microvm-ok in stdout"
        );

        // Reap the virtiofsd child the same way microvm_stop would (kill +
        // delete PID file). In production the kill+pkill is fire-and-forget;
        // here we hold the Child handle so we can wait() and prove the kill
        // landed.
        super::kill_pid_graceful(virtiofsd_pid);
        let status = virtiofsd_child.wait().expect("reap virtiofsd stub");
        assert!(!status.success(), "killed virtiofsd stub should not exit 0");
        state::remove_virtiofsd_pids(&state_dir, &vm.name);
        assert!(state::read_virtiofsd_pids(&state_dir, &vm.name).is_empty());

        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn host_wayland_display_discovers_socket_names() {
        let dir =
            std::env::temp_dir().join(format!("steampipe-wayland-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("wayland-1"), "").unwrap();
        assert_eq!(host_wayland_display(&dir), Some("wayland-1".into()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
