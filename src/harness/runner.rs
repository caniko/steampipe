//! End-to-end test orchestration: deploy, launch, monitor, collect logs, report.
//!
//! The test loop deploys the game binary to VMs, launches a local host process,
//! starts game instances on VMs, monitors for heartbeat stalls and timeouts,
//! and produces a pass/fail summary with failure code breakdown.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Deserialize;
use tokio::sync::{Mutex, watch};

use crate::core::backend::Backend;
use crate::core::config::{ChaosProfile, ClusterConfig, IpAddr, VmName};
use crate::harness::output::{OutputFormat, RunResult, RunStatus, TestSession, TimeoutKind};
use crate::ui::cli::{CaptureMode, NetworkMode};

/// Configuration for a test run.
///
/// Created from CLI arguments in `main.rs` and passed to [`run`].
pub struct TestConfig {
    pub network: NetworkMode,
    pub players: u8,
    pub vm_args: Option<String>,
    pub host_args: Option<String>,
    pub max_runs: u32,
    /// Maximum time without semantic progress before the run is declared stuck.
    pub timeout: Duration,
    /// Emergency wall-clock cap even if semantic progress continues.
    pub hard_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub stop_on_failure: bool,
    pub deploy: bool,
    pub build: bool,
    pub filter_pattern: Option<String>,
    pub output_file: Option<PathBuf>,
    pub capture_mode: CaptureMode,
    pub capture_interval_secs: Option<u32>,
    pub screenshot_backend: crate::cli::ScreenshotBackend,
    pub visual_validator: Option<String>,
    pub visual_config: Option<crate::core::config::VisualConfig>,
    pub scenes: Vec<String>,
    pub scene_vm: Option<String>,
    pub record_video: bool,
    /// VM display mode: headless (game gets `--headless`), weston, or sway.
    pub display: crate::cli::DisplayMode,
    /// When false, suppress `println!` output (for MCP tools where stdout is JSON-RPC).
    pub verbose: bool,
    /// Chaos profile to apply during tests (netem params).
    pub chaos: Option<ChaosProfile>,
    /// Heartbeat stall threshold in seconds.
    pub heartbeat_stall_secs: u64,
    /// Output format (text, junit, jsonl).
    pub output_format: OutputFormat,
    /// Shell command to run when test session completes.
    pub on_complete: Option<String>,
    /// Shell command to run when any test fails.
    pub on_failure: Option<String>,
    /// Parsed exit code mapping from config.
    pub exit_codes: HashMap<i32, String>,
    /// Launch the local host process under strace (captures network syscalls).
    pub strace: bool,
    /// Extra env vars exported to both the local host process and the VM-side
    /// game launched via SSH. Used by tests that need a shared per-session
    /// tag (e.g. `THESPAN_SESSION_ID` for libp2p protocol-id namespacing) so
    /// concurrent fix-loops on the same host can't cross-discover each other.
    /// Production behaviour (no entries) is unchanged.
    pub env: std::collections::BTreeMap<String, String>,
}

const DEFAULT_HARD_TIMEOUT_MIN_SECS: u64 = 1_800;
const DEFAULT_HARD_TIMEOUT_MULTIPLIER: u64 = 5;
const CAPTURE_WARN_BYTES: u64 = 500 * 1024 * 1024;

pub fn resolve_hard_timeout(timeout: Duration, explicit: Option<Duration>) -> Duration {
    explicit.unwrap_or_else(|| {
        Duration::from_secs(
            timeout
                .as_secs()
                .saturating_mul(DEFAULT_HARD_TIMEOUT_MULTIPLIER)
                .max(DEFAULT_HARD_TIMEOUT_MIN_SECS),
        )
    })
}

pub fn validate_visual_capture_config(
    display: crate::cli::DisplayMode,
    screenshot_backend: crate::cli::ScreenshotBackend,
) -> anyhow::Result<()> {
    use crate::cli::{DisplayMode, ScreenshotBackend};
    if screenshot_backend == ScreenshotBackend::Vnc
        && !matches!(display, DisplayMode::Sway | DisplayMode::SwayGpu)
    {
        anyhow::bail!(
            "VNC capture requires --display sway or sway-gpu; wayvnc needs a wlroots compositor and weston is experimental, got \"{display}\""
        );
    }
    if screenshot_backend == ScreenshotBackend::Grim && display.requires_gpu() {
        anyhow::bail!(
            "grim capture is not supported with --display {display}; GPU display modes render to virtio-gpu without a host-visible Wayland socket, use --screenshot-backend vnc"
        );
    }
    Ok(())
}

// ── Heartbeat trait: static dispatch for local vs. VM heartbeat sources ──

/// Reads heartbeat snapshots from a source.
trait HeartbeatSource {
    fn snapshots(&self) -> Vec<HeartbeatSnapshot>;
}

struct LocalHeartbeat<'a> {
    dir: &'a Path,
}

struct VmHeartbeat<'a> {
    rt: &'a tokio::runtime::Handle,
    backend: &'a Backend,
    vms: &'a [(VmName, IpAddr)],
    remote_dir: &'a str,
    heartbeat_subdir: &'a str,
}

struct LocalDisplay {
    child: Child,
    wayland_display: String,
    xdg_runtime_dir: PathBuf,
}

impl LocalDisplay {
    fn maybe_start(
        display: crate::cli::DisplayMode,
        host_args: &str,
    ) -> anyhow::Result<Option<Self>> {
        if !should_auto_start_local_display(display, host_args, local_display_exists()) {
            return Ok(None);
        }

        Self::start()
            .map(Some)
            .map_err(|err| anyhow::anyhow!("failed to start local Sway display: {err}"))
    }

    fn start() -> anyhow::Result<Self> {
        let runtime =
            std::env::temp_dir().join(format!("steampipe-wayland-{}", std::process::id()));
        std::fs::create_dir_all(&runtime)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
        }

        let mut child = Command::new("sway")
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let deadline = Instant::now() + Duration::from_secs(10);
        let wayland_display = loop {
            if let Some(display) = first_wayland_socket(&runtime)? {
                break display;
            }
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("Sway exited before Wayland socket was ready: {status}");
            }
            if Instant::now() > deadline {
                anyhow::bail!("timed out waiting for Wayland socket");
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        Ok(Self {
            child,
            wayland_display,
            xdg_runtime_dir: runtime,
        })
    }

    fn apply_to(&self, command: &mut Command) {
        command.env("XDG_RUNTIME_DIR", &self.xdg_runtime_dir);
        command.env("WAYLAND_DISPLAY", &self.wayland_display);
    }
}

impl Drop for LocalDisplay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.xdg_runtime_dir);
    }
}

fn local_display_exists() -> bool {
    std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var_os("WAYLAND_SOCKET").is_some()
}

fn should_auto_start_local_display(
    _display: crate::cli::DisplayMode,
    host_args: &str,
    has_display: bool,
) -> bool {
    !has_display && !host_args.split_whitespace().any(|arg| arg == "--headless")
}

fn first_wayland_socket(runtime: &Path) -> anyhow::Result<Option<String>> {
    let mut displays = Vec::new();
    for entry in std::fs::read_dir(runtime)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with("wayland-") && !name.ends_with(".lock") {
            displays.push(name.to_string());
        }
    }
    displays.sort();
    Ok(displays.into_iter().next())
}

fn fatal_output_marker(output: &str) -> Option<&'static str> {
    const MARKERS: [(&str, &str); 15] = [
        ("panicked at", "PANIC"),
        ("thread '", "PANIC"),
        ("[FATAL]", "FATAL"),
        ("PHASE_WATCHDOG", "PHASE_WATCHDOG"),
        ("CONNECTION_LOST", "CONNECTION_LOST"),
        ("INVARIANT_VIOLATION", "INVARIANT_VIOLATION"),
        ("COMMITMENT_FAILURE", "COMMITMENT_FAILURE"),
        ("FORMATION_WATCHDOG", "FORMATION_WATCHDOG"),
        ("MOVE_RESEND_WATCHDOG", "MOVE_RESEND_WATCHDOG"),
        ("PEER_READY_TIMEOUT", "PEER_READY_TIMEOUT"),
        ("STEAM_UNAVAILABLE", "STEAM_UNAVAILABLE"),
        ("BATTLE_STUCK", "BATTLE_STUCK"),
        ("DAG_VIOLATION", "DAG_VIOLATION"),
        ("DAG.VIOLATION", "DAG_VIOLATION"),
        ("DESYNC", "DESYNC"),
    ];

    for (needle, label) in MARKERS {
        if output.contains(needle) {
            return Some(label);
        }
    }

    for line in output.lines() {
        let nonzero_graceful_shutdown = line.contains("Graceful shutdown: exit_code=")
            && !line.contains("Graceful shutdown: exit_code=0");
        let nonzero_graceful_shutdown_request = line
            .contains("GracefulShutdownRequest received (exit_code=")
            && !line.contains("GracefulShutdownRequest received (exit_code=0");
        if nonzero_graceful_shutdown || nonzero_graceful_shutdown_request {
            return Some("GRACEFUL_SHUTDOWN_ERROR");
        }
    }

    None
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
struct HeartbeatSnapshot {
    timestamp_ms: u64,
    #[serde(default)]
    phase: String,
    #[serde(default)]
    round: u32,
    #[serde(default)]
    turn: u32,
    #[serde(default)]
    progress_seq: u64,
}

impl HeartbeatSnapshot {
    fn semantic_progress(&self) -> SemanticProgress {
        SemanticProgress {
            progress_seq: self.progress_seq,
            phase: self.phase.clone(),
            round: self.round,
            turn: self.turn,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticProgress {
    progress_seq: u64,
    phase: String,
    round: u32,
    turn: u32,
}

#[derive(Debug, Clone)]
struct HeartbeatObservation {
    oldest_timestamp: HeartbeatSnapshot,
    slowest_progress: HeartbeatSnapshot,
}

#[derive(Debug, Default, Clone)]
struct HeartbeatMonitorState {
    last_timestamp_ms: Option<u64>,
    last_timestamp_snapshot: Option<HeartbeatSnapshot>,
    last_progress: Option<SemanticProgress>,
    last_progress_at_ms: Option<u64>,
    last_progress_snapshot: Option<HeartbeatSnapshot>,
}

#[derive(Debug, Clone)]
struct MonitorFailure {
    kind: TimeoutKind,
    banner: String,
}

impl MonitorFailure {
    fn heartbeat_stall(
        source: &'static str,
        snapshot: HeartbeatSnapshot,
        stale_secs: u64,
        stall_secs: u64,
    ) -> Self {
        Self {
            kind: TimeoutKind::HeartbeatStall,
            banner: format!(
                "--- HEARTBEAT_STALL ({source}: {stale_secs}s without a fresh heartbeat > {stall_secs}s, {}) ---",
                heartbeat_context(&snapshot)
            ),
        }
    }

    fn no_progress_timeout(
        source: &'static str,
        snapshot: HeartbeatSnapshot,
        stalled_secs: u64,
        timeout_secs: u64,
    ) -> Self {
        Self {
            kind: TimeoutKind::NoProgressTimeout,
            banner: format!(
                "--- NO_PROGRESS_TIMEOUT ({source}: {stalled_secs}s without semantic progress > {timeout_secs}s, {}) ---",
                heartbeat_context(&snapshot)
            ),
        }
    }

    fn hard_timeout(
        runtime_secs: u64,
        hard_timeout_secs: u64,
        source: Option<&'static str>,
        stalled_secs: Option<u64>,
        snapshot: Option<HeartbeatSnapshot>,
    ) -> Self {
        let detail = match (source, stalled_secs, snapshot) {
            (Some(source), Some(stalled_secs), Some(snapshot)) => format!(
                "{source}: last semantic progress {stalled_secs}s ago, {}",
                heartbeat_context(&snapshot)
            ),
            _ => "no heartbeat context available".to_string(),
        };
        Self {
            kind: TimeoutKind::HardTimeout,
            banner: format!(
                "--- HARD_TIMEOUT (runtime {runtime_secs}s > {hard_timeout_secs}s, {detail}) ---"
            ),
        }
    }
}

impl HeartbeatSource for LocalHeartbeat<'_> {
    fn snapshots(&self) -> Vec<HeartbeatSnapshot> {
        let Ok(entries) = std::fs::read_dir(self.dir) else {
            return Vec::new();
        };
        let mut snapshots = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("game_progress_") && name.ends_with(".json") {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    snapshots.extend(parse_heartbeats(&content));
                }
            }
        }
        snapshots
    }
}

impl HeartbeatSource for VmHeartbeat<'_> {
    fn snapshots(&self) -> Vec<HeartbeatSnapshot> {
        // Reads both the per-cluster subdir (current layout) and the legacy
        // remote-dir root (for older deployed binaries that don't honor
        // STEAMPIPE_HEARTBEAT_DIR). The legacy fallback can be removed once
        // all deployed games are recent enough.
        let cmd = format!(
            "cat {0}/{1}/game_progress_*.json {0}/game_progress_*.json 2>/dev/null || true",
            self.remote_dir, self.heartbeat_subdir
        );
        let mut snapshots = Vec::new();
        for (_, ip) in self.vms {
            let result = self.rt.block_on(self.backend.run_cmd_timeout(
                ip,
                &cmd,
                std::time::Duration::from_secs(5),
            ));
            snapshots.extend(parse_heartbeats(&result.stdout));
        }
        snapshots
    }
}

fn parse_heartbeats(content: &str) -> Vec<HeartbeatSnapshot> {
    serde_json::Deserializer::from_str(content)
        .into_iter::<HeartbeatSnapshot>()
        .filter_map(Result::ok)
        .collect()
}

fn observe_heartbeat_source(source: &impl HeartbeatSource) -> Option<HeartbeatObservation> {
    let snapshots = source.snapshots();
    let oldest_timestamp = snapshots.iter().min_by_key(|hb| hb.timestamp_ms)?.clone();
    let slowest_progress = snapshots
        .iter()
        .min_by_key(|hb| (hb.progress_seq, hb.timestamp_ms))?
        .clone();

    Some(HeartbeatObservation {
        oldest_timestamp,
        slowest_progress,
    })
}

fn check_heartbeat_source(
    source_name: &'static str,
    source: &impl HeartbeatSource,
    state: &mut HeartbeatMonitorState,
    heartbeat_stall_secs: u64,
    no_progress_timeout: Duration,
    now_ms: u64,
) -> Option<MonitorFailure> {
    let observation = observe_heartbeat_source(source)?;

    let oldest = observation.oldest_timestamp.clone();
    if let Some(prev) = state.last_timestamp_ms
        && oldest.timestamp_ms <= prev
    {
        let stale_secs = now_ms.saturating_sub(oldest.timestamp_ms) / 1000;
        if stale_secs > heartbeat_stall_secs {
            state.last_timestamp_snapshot = Some(oldest.clone());
            return Some(MonitorFailure::heartbeat_stall(
                source_name,
                oldest,
                stale_secs,
                heartbeat_stall_secs,
            ));
        }
    }
    state.last_timestamp_ms = Some(oldest.timestamp_ms);
    state.last_timestamp_snapshot = Some(oldest);

    let slowest = observation.slowest_progress.clone();
    let progress = slowest.semantic_progress();
    if state.last_progress.as_ref() != Some(&progress) {
        state.last_progress = Some(progress);
        state.last_progress_at_ms = Some(now_ms);
    } else if let Some(last_progress_at_ms) = state.last_progress_at_ms {
        let stalled_secs = now_ms.saturating_sub(last_progress_at_ms) / 1000;
        if stalled_secs > no_progress_timeout.as_secs() {
            state.last_progress_snapshot = Some(slowest.clone());
            return Some(MonitorFailure::no_progress_timeout(
                source_name,
                slowest,
                stalled_secs,
                no_progress_timeout.as_secs(),
            ));
        }
    }
    state.last_progress_snapshot = Some(slowest);

    None
}

fn select_hard_timeout_context(
    now_ms: u64,
    local_state: &HeartbeatMonitorState,
    vm_state: &HeartbeatMonitorState,
) -> (Option<&'static str>, Option<u64>, Option<HeartbeatSnapshot>) {
    [("local", local_state), ("vm", vm_state)]
        .into_iter()
        .filter_map(|(name, state)| {
            Some((
                name,
                now_ms.saturating_sub(state.last_progress_at_ms?) / 1000,
                state.last_progress_snapshot.clone()?,
            ))
        })
        .max_by_key(|(_, stalled_secs, _)| *stalled_secs)
        .map(|(name, stalled_secs, snapshot)| (Some(name), Some(stalled_secs), Some(snapshot)))
        .unwrap_or((None, None, None))
}

fn heartbeat_context(snapshot: &HeartbeatSnapshot) -> String {
    let phase = if snapshot.phase.is_empty() {
        "unknown"
    } else {
        snapshot.phase.as_str()
    };
    format!(
        "phase={phase}, round={}, turn={}, progress_seq={}",
        snapshot.round, snapshot.turn, snapshot.progress_seq
    )
}

async fn run_configured_scenes<S>(
    config: &ClusterConfig<S>,
    project_root: &Path,
    test_config: &TestConfig,
    target_vms: &[crate::core::config::VmDef],
    run_num: u32,
    output: &mut String,
) -> anyhow::Result<Vec<crate::game::visual::VisualRunResult>> {
    if test_config.scenes.is_empty() {
        return Ok(Vec::new());
    }

    let visual = test_config.visual_config.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "--scenes requires validator_kind = \"golden\" and a configured [visual] block"
        )
    })?;
    let vm = find_scene_vm(target_vms, test_config.scene_vm.as_deref())?;
    let run_label = format!("run-{run_num}");
    let output_dir = project_root
        .join("logs")
        .join("scenes")
        .join(&run_label)
        .join(&vm.name);
    let mut visual_results = Vec::new();

    print_and_push(output, "=== SCENE CAPTURES ===", test_config.verbose);
    for scene_name in &test_config.scenes {
        let scene = visual
            .scene(scene_name)
            .ok_or_else(|| anyhow::anyhow!("visual scene '{scene_name}' not found"))?;
        let backend = scene_capture_backend(visual, scene);
        validate_visual_capture_config(test_config.display, backend)?;
        if backend == crate::cli::ScreenshotBackend::Vnc {
            crate::capture::ensure_wayvnc(&config.backend, vm, &config.vm_user, true).await?;
        }
        crate::game::scene::runner::run_scene_named(
            &config.backend,
            vm,
            &config.vm_user,
            visual,
            scene_name,
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

        let options = crate::capture::ScreenshotOptions {
            backend,
            validator: test_config.visual_validator.clone(),
            golden: test_config.visual_config.clone(),
            scene: Some(scene_name.clone()),
            run_label: Some(run_label.clone()),
            window_target: scene.window.clone(),
            allow_vnc_input: true,
        };
        let (image_path, mut scene_results) =
            crate::capture::capture_single_vm(config, vm, &output_dir, &options).await?;
        if scene_results.is_empty() {
            print_and_push(
                output,
                &format!(
                    "  {} on {}: captured {}",
                    scene.name,
                    vm.name,
                    image_path.display()
                ),
                test_config.verbose,
            );
        } else {
            for result in &scene_results {
                print_and_push(
                    output,
                    &format!(
                        "  {} on {}: {} (ssim {:.5}, max_delta {})",
                        result.scene,
                        vm.name,
                        if result.passed { "passed" } else { "failed" },
                        result.ssim,
                        result.max_delta
                    ),
                    test_config.verbose,
                );
            }
        }
        visual_results.append(&mut scene_results);
    }
    output.push('\n');
    Ok(visual_results)
}

#[derive(Clone)]
struct StallCaptureContext {
    backend: Backend,
    vm_user: String,
    target_vms: Vec<crate::core::config::VmDef>,
    output_root: PathBuf,
    run_label: String,
    capture_lock: std::sync::Arc<Mutex<()>>,
    screenshot_backend: crate::cli::ScreenshotBackend,
}

async fn capture_reason_frames(
    context: &StallCaptureContext,
    reason: &str,
    stamp: &str,
) -> anyhow::Result<()> {
    let _guard = context.capture_lock.lock().await;
    if context.screenshot_backend == crate::cli::ScreenshotBackend::Vnc {
        for vm in &context.target_vms {
            crate::capture::ensure_wayvnc(&context.backend, vm, &context.vm_user, false).await?;
        }
    }
    for vm in &context.target_vms {
        let dir = context
            .output_root
            .join(&context.run_label)
            .join(reason)
            .join(&vm.name);
        std::fs::create_dir_all(&dir)?;
        let captured = crate::capture::screenshot_vm(
            &context.backend,
            vm,
            &dir,
            context.screenshot_backend,
            None,
        )
        .await?;
        let final_path = dir.join(format!("{stamp}.png"));
        std::fs::rename(captured, final_path)?;
    }
    warn_if_capture_storage_exceeds_budget(&context.output_root.join(&context.run_label));
    Ok(())
}

async fn run_timelapse_task(
    context: StallCaptureContext,
    interval_seconds: u32,
    mut stop_rx: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds as u64));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut seq = 0u64;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                seq += 1;
                let stamp = format!("{seq:06}");
                if let Err(error) = capture_reason_frames(&context, "timelapse", &stamp).await {
                    eprintln!("Warning: timelapse capture failed: {error}");
                }
            }
            changed = stop_rx.changed() => {
                if changed.is_err() || *stop_rx.borrow() {
                    break;
                }
            }
        }
    }
}

async fn start_video_capture<S>(
    config: &ClusterConfig<S>,
    target_vms: &[crate::core::config::VmDef],
) -> Vec<String> {
    let mut started = Vec::new();
    for vm in target_vms {
        let command = r#"if ! command -v wf-recorder >/dev/null 2>&1; then
    echo MISSING
    exit 0
fi
export XDG_RUNTIME_DIR="/tmp/runtime-$(whoami)"
export WAYLAND_DISPLAY=wayland-1
rm -f /tmp/steampipe-run.webm /tmp/steampipe-wf-recorder.log
pkill -INT -x wf-recorder >/dev/null 2>&1 || true
nohup sh -lc 'wf-recorder -f /tmp/steampipe-run.webm >/tmp/steampipe-wf-recorder.log 2>&1' >/dev/null 2>&1 &
sleep 2
pgrep -x wf-recorder >/dev/null && echo STARTED || echo FAILED"#;
        let output = config.backend.run_cmd(&vm.ip, command).await;
        match output.stdout.trim() {
            "STARTED" => started.push(vm.name.to_string()),
            "MISSING" => eprintln!("Warning: {} missing wf-recorder; skipping video", vm.name),
            _ => eprintln!("Warning: {} failed to start wf-recorder", vm.name),
        }
    }
    started
}

async fn stop_video_capture<S>(
    config: &ClusterConfig<S>,
    target_vms: &[crate::core::config::VmDef],
    run_root: &Path,
    started: &[String],
) {
    let video_dir = run_root.join("video");
    let _ = std::fs::create_dir_all(&video_dir);
    for vm in target_vms {
        if !started.iter().any(|name| name == &vm.name.to_string()) {
            continue;
        }
        let stop = config
            .backend
            .run_cmd(
                &vm.ip,
                r#"pkill -INT -x wf-recorder >/dev/null 2>&1 || true
sleep 1
[ -s /tmp/steampipe-run.webm ] && echo READY || echo EMPTY"#,
            )
            .await;
        if !stop.stdout.contains("READY") {
            eprintln!(
                "Warning: {} video capture empty; inspect /tmp/steampipe-wf-recorder.log in guest",
                vm.name
            );
            continue;
        }
        if let Err(error) = config
            .backend
            .download(&vm.ip, "/tmp/steampipe-run.webm", &video_dir)
            .await
        {
            eprintln!(
                "Warning: failed to download video from {}: {error}",
                vm.name
            );
            continue;
        }
        let downloaded = video_dir.join("steampipe-run.webm");
        let final_path = video_dir.join(format!("{}.webm", vm.name));
        if downloaded.exists() {
            let _ = std::fs::rename(downloaded, final_path);
        }
    }
}

fn warn_if_capture_storage_exceeds_budget(run_root: &Path) {
    let mut total = 0u64;
    let mut stack = vec![run_root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(metadata) = entry.metadata() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    if total > CAPTURE_WARN_BYTES {
        eprintln!(
            "Warning: capture artifacts for {} exceed {} MiB",
            run_root.display(),
            CAPTURE_WARN_BYTES / 1024 / 1024
        );
    }
}

// ── Main test orchestration ──

/// Run the end-to-end test orchestration loop.
pub async fn run<S>(
    config: &ClusterConfig<S>,
    project_root: &Path,
    test_config: TestConfig,
) -> anyhow::Result<String> {
    if test_config.players < 2 || test_config.players > 8 {
        anyhow::bail!("players must be 2-8, got {}", test_config.players);
    }
    if test_config.capture_mode != CaptureMode::Off
        || test_config.visual_validator.is_some()
        || test_config.visual_config.is_some()
    {
        validate_visual_capture_config(test_config.display, test_config.screenshot_backend)?;
    }
    if test_config.capture_mode == CaptureMode::Timelapse
        && test_config.capture_interval_secs.is_none()
    {
        anyhow::bail!("--capture-mode timelapse requires --capture-interval");
    }

    let vm_count = (test_config.players as usize).saturating_sub(1);
    if config.vms.len() < vm_count {
        anyhow::bail!(
            "cluster '{}' has {} claimed VM(s), but {} player(s) require {vm_count} VM(s). Run the matching up command first, e.g. `nix run .#cluster-1v1-up` or `nix run .#cluster-tournament-up`.",
            config.cluster_name,
            config.vms.len(),
            test_config.players
        );
    }
    let target_vms = &config.vms[..vm_count];

    let filter_re = Regex::new(
        test_config.filter_pattern.as_deref().unwrap_or(
            r"FAIL|PASS|panic|error|bug_report|violation|completed|timeout|killed|summary|Player|disconnected",
        ),
    )?;

    let backend = &config.backend;
    let verbose = test_config.verbose;
    let mut output = String::new();

    output.push_str(&format!(
        "Cluster test: network={}, players={}, vms={vm_count}, max_runs={}\n\n",
        test_config.network, test_config.players, test_config.max_runs,
    ));

    // Step 1: Deploy (reuse deploy module)
    if test_config.deploy {
        print_and_push(&mut output, "=== DEPLOYING ===", verbose);
        if test_config.build {
            crate::deploy::build_release(project_root, &config.cargo_package).await?;
            print_and_push(&mut output, "Build OK", verbose);
        }

        let deploy_results =
            crate::deploy::deploy_to_vms(backend, target_vms, config, project_root, false).await?;
        let failed_count = deploy_results.iter().filter(|r| r.error.is_some()).count();
        if failed_count > 0 {
            anyhow::bail!("Deploy failed for {} VMs", failed_count);
        }
        output.push('\n');
    }

    let mut gpu_preflight = Vec::new();
    let mut readiness = Vec::new();

    // Pre-flight: verify VMs have a GPU device if using a GPU display mode
    if test_config.display.requires_gpu() {
        print_and_push(&mut output, "=== GPU CHECK ===", verbose);
        crate::vm::preflight::ensure_vm_gpu(backend, target_vms, test_config.display).await?;
        gpu_preflight = crate::vm::preflight::gpu_preflight_reports_for_targets(config, target_vms)
            .await
            .unwrap_or_default();
        print_and_push(&mut output, "  All VMs have a DRM device", verbose);
        output.push('\n');
    }

    // Step 2a: Start compositor on VMs if display mode requires it
    if test_config.display != crate::cli::DisplayMode::Headless {
        print_and_push(
            &mut output,
            &format!("=== STARTING {} ON VMs ===", test_config.display),
            verbose,
        );
        let setup_script = crate::steam::compositor_setup(test_config.display, &config.vm_user);
        for vm in target_vms {
            let result = backend.run_cmd(&vm.ip, &setup_script).await;
            if result.success {
                print_and_push(
                    &mut output,
                    &format!("  {}: {} ready", vm.name, test_config.display),
                    verbose,
                );
            } else {
                if result.stdout.trim().is_empty() && result.stderr.trim().is_empty() {
                    let tail = crate::steam::vm_log_tail(&config.state_dir, vm)
                        .unwrap_or_else(|| "(vm.log unavailable)".into());
                    print_and_push(
                        &mut output,
                        &format!(
                            "  {}: compositor FAILED — no remote output; vm.log tail:\n{tail}",
                            vm.name
                        ),
                        verbose,
                    );
                    anyhow::bail!(
                        "{}: compositor setup produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
                        vm.name
                    );
                }
                print_and_push(
                    &mut output,
                    &format!("  {}: compositor FAILED — {}", vm.name, result.stderr),
                    verbose,
                );
                anyhow::bail!("Compositor setup failed on {}", vm.name);
            }
        }
        output.push('\n');
        readiness = crate::game::compositor::status_rows(config)
            .await
            .unwrap_or_default();
    }

    // Step 2b: Ensure Steam on VMs if steam network
    if test_config.network == NetworkMode::Steam {
        print_and_push(&mut output, "=== STARTING STEAM ON VMs ===", verbose);
        let compositor = crate::steam::steam_compositor_setup(test_config.display, &config.vm_user);
        for vm in target_vms {
            match crate::steam::ensure_steam(backend, &vm.ip, &config.vm_user, &compositor).await {
                Ok(()) => {
                    print_and_push(&mut output, &format!("  {}: Steam ready", vm.name), verbose)
                }
                Err(e) => {
                    print_and_push(
                        &mut output,
                        &format!("  {}: FAILED — {e}", vm.name),
                        verbose,
                    );
                    anyhow::bail!("Steam setup failed on {}", vm.name);
                }
            }
        }
        output.push('\n');
    }

    // Pre-flight: verify VM→host connectivity (catches missing nftables rules after reboot)
    crate::vm::preflight::ensure_vm_to_host_connectivity(config, backend, target_vms).await?;

    // Apply chaos profile if configured
    if let Some(ref chaos) = test_config.chaos {
        print_and_push(&mut output, "=== APPLYING CHAOS PROFILE ===", verbose);
        for vm in target_vms {
            crate::netem::apply(
                config,
                &vm.name,
                chaos.latency,
                chaos.jitter,
                chaos.loss,
                chaos.rate,
            )?;
        }
        output.push('\n');
    }

    // Step 3: Run loop
    let binary_name = &config.binary_name;
    let binary_path = project_root.join(format!("target/release/{binary_name}"));
    if !binary_path.exists() {
        anyhow::bail!(
            "target/release/{binary_name} not found — run with --build or `cargo build --release -p {binary_name}`"
        );
    }

    // Default args based on network mode when not explicitly provided.
    // Host never gets --headless; VMs get it only in headless display mode.
    let vm_headless = if test_config.display == crate::cli::DisplayMode::Headless {
        " --headless"
    } else {
        ""
    };
    let default_vm_args;
    let default_host_args;
    match test_config.network {
        NetworkMode::Lan => {
            default_host_args = "--auto-host-udp --auto-play".to_string();
            default_vm_args = format!(
                "--auto-join-udp --udp-addr {}:{} --auto-play{vm_headless}",
                config.host_ip, config.udp_port,
            );
        }
        NetworkMode::Steam => {
            default_host_args = "--auto-host-steam --auto-play".to_string();
            let target_flag = crate::accounts::local_steam_id()
                .map(|id| format!(" --steam-target-id {id}"))
                .unwrap_or_default();
            default_vm_args = format!("--auto-join-steam --auto-play{vm_headless}{target_flag}");
        }
    }
    let vm_args = test_config.vm_args.as_deref().unwrap_or(&default_vm_args);
    let host_args = test_config
        .host_args
        .as_deref()
        .unwrap_or(&default_host_args);
    let local_display = LocalDisplay::maybe_start(test_config.display, host_args)?;
    if let Some(display) = &local_display {
        print_and_push(
            &mut output,
            &format!("=== LOCAL WAYLAND DISPLAY {} ===", display.wayland_display),
            verbose,
        );
        output.push('\n');
    }

    // Per-cluster runtime dir: heartbeat files and strace output are scoped to
    // <project_root>/.steampipe-runtime/<cluster_name>/ so two parallel
    // cluster_test invocations with distinct cluster names cannot wipe each
    // other's heartbeats or merge into a single strace log.
    let local_heartbeat_dir = ensure_cluster_runtime_dirs(project_root, &config.cluster_name);
    let strace_output =
        cluster_runtime_dir(project_root, &config.cluster_name).join("strace-host.log");
    // VM-side heartbeats live under `<remote_dir>/<vm_heartbeat_subdir>` so the
    // VM scoping mirrors the host. The path is relative because launch_game
    // shells in via `cd remote_dir`.
    let vm_heartbeat_subdir = format!(".steampipe-runtime/{}/heartbeats", config.cluster_name);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut timed_out_count = 0u32;
    let mut exit_codes: Vec<Option<i32>> = Vec::new();
    let mut visual_results: Vec<crate::game::visual::VisualRunResult> = Vec::new();
    let mut consecutive_fail_code: Option<i32> = None;
    let mut consecutive_fail_count = 0u32;
    const REPEAT_FAILURE_LIMIT: u32 = 3;
    let session_start = Instant::now();
    let mut session = TestSession::new(
        &test_config.network.to_string(),
        test_config.players,
        vm_count,
    );

    for run_num in 1..=test_config.max_runs {
        let run_start = Instant::now();
        let run_label = format!("run-{run_num}");
        print_and_push(
            &mut output,
            &format!("=== RUN {run_num}/{} ===", test_config.max_runs),
            verbose,
        );

        // Kill existing games
        crate::run::kill_games(backend, target_vms, binary_name).await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Launch local (host) process FIRST so it's listening when VMs connect
        eprintln!("[cluster-ctl] Launching local process...");
        let mut local_cmd = if test_config.strace {
            let mut cmd = std::process::Command::new("strace");
            cmd.args(["-f", "-e", "trace=network", "-tt", "-o"]);
            cmd.arg(&strace_output);
            cmd.arg(&binary_path);
            cmd
        } else {
            std::process::Command::new(&binary_path)
        };
        if test_config.strace {
            println!("  strace enabled → {}", strace_output.display());
        }
        for arg in host_args.split_whitespace() {
            local_cmd.arg(arg);
        }
        local_cmd.current_dir(project_root);
        local_cmd.env("BEVY_ASSET_ROOT", project_root);
        local_cmd.env("STEAMPIPE_HEARTBEAT_DIR", &local_heartbeat_dir);
        if let Some(display) = &local_display {
            display.apply_to(&mut local_cmd);
        }
        let mut ld_paths = Vec::new();
        if let Some(lib_dir) = config.steam_api_lib.parent() {
            ld_paths.push(lib_dir.to_string_lossy().into_owned());
        }
        if let Ok(existing) = std::env::var("LD_LIBRARY_PATH")
            && !existing.is_empty()
        {
            ld_paths.push(existing);
        }
        if !ld_paths.is_empty() {
            local_cmd.env("LD_LIBRARY_PATH", ld_paths.join(":"));
        }
        // Profile-supplied env (and the auto-set `THESPAN_SESSION_ID`) goes
        // here so it's also available to subprocesses the host might spawn.
        for (k, v) in &test_config.env {
            local_cmd.env(k, v);
        }
        if verbose {
            local_cmd.stdout(std::process::Stdio::inherit());
            local_cmd.stderr(std::process::Stdio::inherit());
        } else {
            // In MCP mode, stdout is the JSON-RPC transport — must pipe to avoid corruption.
            // Also enables monitor_local_process to capture output for the report.
            local_cmd.stdout(std::process::Stdio::piped());
            local_cmd.stderr(std::process::Stdio::piped());
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            local_cmd.process_group(0);
        }

        let mut child = local_cmd.spawn()?;
        eprintln!("[cluster-ctl] Host process spawned with PID {}", child.id());

        // Wait for host to start listening before launching VMs
        eprintln!("[cluster-ctl] Waiting 3s for host to initialize...");
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Launch game on VMs (joiners)
        eprintln!("[cluster-ctl] Launching game on VMs...");
        let mut vm_launch_ok = true;
        for vm in target_vms {
            eprintln!("[cluster-ctl]   launching on {}...", vm.name);
            if let Err(e) = launch_game(
                backend,
                &vm.ip,
                &config.remote_dir,
                binary_name,
                &config.vm_user,
                &config.log_file,
                vm_args,
                &vm_heartbeat_subdir,
                &test_config.env,
            )
            .await
            {
                print_and_push(
                    &mut output,
                    &format!("  {}: launch FAILED: {e}", vm.name),
                    verbose,
                );
                vm_launch_ok = false;
            }
        }

        if !vm_launch_ok {
            failed += 1;
            print_and_push(&mut output, "--- FAIL (VM launch failed) ---", verbose);
            kill_process_group(&mut child, test_config.shutdown_timeout);
            crate::run::kill_games(backend, target_vms, binary_name).await;
            if test_config.stop_on_failure {
                break;
            }
            output.push('\n');
            continue;
        }
        eprintln!("[cluster-ctl] VMs launched OK");

        let capture_root = test_config
            .visual_config
            .as_ref()
            .and_then(|visual| visual.diff_dir.clone())
            .unwrap_or_else(|| project_root.join("logs").join("visual"));
        let capture_lock = std::sync::Arc::new(Mutex::new(()));
        let stall_capture_context = StallCaptureContext {
            backend: config.backend.clone(),
            vm_user: config.vm_user.clone(),
            target_vms: target_vms.to_vec(),
            output_root: capture_root.clone(),
            run_label: run_label.clone(),
            capture_lock: capture_lock.clone(),
            screenshot_backend: test_config.screenshot_backend,
        };

        let video_started = if test_config.record_video {
            start_video_capture(config, target_vms).await
        } else {
            Vec::new()
        };

        match run_configured_scenes(
            config,
            project_root,
            &test_config,
            target_vms,
            run_num,
            &mut output,
        )
        .await
        {
            Ok(mut scene_results) => visual_results.append(&mut scene_results),
            Err(error) => {
                failed += 1;
                print_and_push(
                    &mut output,
                    &format!("--- FAIL (scene capture failed: {error}) ---"),
                    verbose,
                );
                crate::run::kill_games(backend, target_vms, binary_name).await;
                kill_process_group(&mut child, test_config.shutdown_timeout);
                stop_video_capture(
                    config,
                    target_vms,
                    &capture_root.join(&run_label),
                    &video_started,
                )
                .await;
                if test_config.stop_on_failure {
                    break;
                }
                output.push('\n');
                continue;
            }
        }

        let (stop_tx, stop_rx) = watch::channel(false);
        let timelapse_task = if test_config.capture_mode == CaptureMode::Timelapse {
            Some(tokio::spawn(run_timelapse_task(
                stall_capture_context.clone(),
                test_config.capture_interval_secs.unwrap_or(5),
                stop_rx,
            )))
        } else {
            None
        };

        // Monitor with heartbeat detection
        cleanup_heartbeat_files(&local_heartbeat_dir);

        let timeout = test_config.timeout;
        let shutdown_timeout = test_config.shutdown_timeout;
        let heartbeat_dir_owned = local_heartbeat_dir.clone();
        let vm_ips: Vec<(VmName, IpAddr)> = target_vms
            .iter()
            .map(|vm| (vm.name.clone(), vm.ip.clone()))
            .collect();
        let backend_for_monitor = backend.clone();
        let remote_dir_owned = config.remote_dir.clone();
        let vm_heartbeat_subdir_owned = vm_heartbeat_subdir.clone();

        let hb_stall = test_config.heartbeat_stall_secs;
        let hard_timeout = test_config.hard_timeout;
        let stall_capture_context_for_monitor = stall_capture_context.clone();
        let (combined, exit_code, monitor_failure) = tokio::task::spawn_blocking(move || {
            monitor_local_process(
                &mut child,
                timeout,
                hard_timeout,
                shutdown_timeout,
                &heartbeat_dir_owned,
                &vm_ips,
                &backend_for_monitor,
                &remote_dir_owned,
                &vm_heartbeat_subdir_owned,
                hb_stall,
                if test_config.capture_mode == CaptureMode::FailureAndStall
                    || test_config.capture_mode == CaptureMode::Timelapse
                {
                    Some(stall_capture_context_for_monitor)
                } else {
                    None
                },
            )
        })
        .await??;
        let _ = stop_tx.send(true);
        if let Some(task) = timelapse_task {
            let _ = task.await;
        }
        stop_video_capture(
            config,
            target_vms,
            &capture_root.join(&run_label),
            &video_started,
        )
        .await;

        cleanup_heartbeat_files(&local_heartbeat_dir);

        // Filter and report
        for line in combined.lines().filter(|l| filter_re.is_match(l)) {
            output.push_str(line);
            output.push('\n');
        }

        let local_fatal = fatal_output_marker(&combined);
        let mut vm_logs_for_report: Option<Vec<(String, String)>> = None;
        let mut vm_fatal = None;
        if monitor_failure.is_none() && exit_code == Some(0) && local_fatal.is_none() {
            let mut logs = Vec::new();
            for vm in target_vms {
                let log =
                    collect_vm_log(backend, &vm.ip, &config.remote_dir, &config.log_file, 50).await;
                if vm_fatal.is_none()
                    && let Some(label) = fatal_output_marker(&log)
                {
                    vm_fatal = Some(label);
                }
                logs.push((vm.name.to_string(), log));
            }
            vm_logs_for_report = Some(logs);
        }

        // Classify result
        let failure_code: Option<i32>;
        let recorded_exit_code: Option<i32>;
        let run_status: RunStatus;
        let run_label: String;
        let fatal_marker = local_fatal.or(vm_fatal);
        if let Some(failure) = monitor_failure.as_ref() {
            timed_out_count += 1;
            failed += 1;
            failure_code = Some(timeout_failure_code(failure.kind));
            recorded_exit_code = None;
            run_status = RunStatus::Timeout(failure.kind);
            run_label = failure.kind.label().into();
            print_and_push(&mut output, &failure.banner, verbose);
        } else if let Some(label) = fatal_marker {
            failed += 1;
            failure_code = Some(1);
            recorded_exit_code = Some(1);
            run_status = RunStatus::Fail;
            run_label = label.into();
            print_and_push(
                &mut output,
                &format!("--- FAIL (fatal marker: {label}) ---"),
                verbose,
            );
        } else if exit_code == Some(0) {
            passed += 1;
            failure_code = None;
            recorded_exit_code = Some(0);
            run_status = RunStatus::Pass;
            run_label = "SUCCESS".into();
            print_and_push(&mut output, "--- PASS ---", verbose);
        } else {
            failed += 1;
            failure_code = exit_code;
            recorded_exit_code = exit_code;
            run_status = RunStatus::Fail;
            let code_str: Cow<'static, str> = exit_code
                .map(|c| {
                    Cow::Owned(format!(
                        "{c} ({})",
                        exit_code_label(c, &test_config.exit_codes)
                    ))
                })
                .unwrap_or(Cow::Borrowed("signal"));
            run_label = exit_code
                .map(|c| exit_code_label(c, &test_config.exit_codes).into_owned())
                .unwrap_or_else(|| "SIGNAL".into());
            print_and_push(
                &mut output,
                &format!("--- FAIL (exit {code_str}) ---"),
                verbose,
            );
        }

        // Collect VM logs on failure
        let run_failed =
            monitor_failure.is_some() || exit_code != Some(0) || fatal_marker.is_some();
        if run_failed {
            output.push_str("\n--- VM logs (last 30 lines each) ---\n");
            if vm_logs_for_report.is_none() {
                let mut logs = Vec::new();
                for vm in target_vms {
                    let log =
                        collect_vm_log(backend, &vm.ip, &config.remote_dir, &config.log_file, 30)
                            .await;
                    logs.push((vm.name.to_string(), log));
                }
                vm_logs_for_report = Some(logs);
            }
            for (name, log) in vm_logs_for_report.as_ref().into_iter().flatten() {
                output.push_str(&format!("  [{name}]:\n"));
                for line in log.lines() {
                    output.push_str(&format!("    {line}\n"));
                }
            }

            output.push_str("\n--- Local unfiltered tail (last 60 lines) ---\n");
            let tail_lines: Vec<&str> = combined.lines().rev().take(60).collect();
            for line in tail_lines.into_iter().rev() {
                output.push_str(line);
                output.push('\n');
            }

            // Capture screenshots on failure
            if matches!(
                test_config.capture_mode,
                CaptureMode::Failure | CaptureMode::FailureAndStall | CaptureMode::Timelapse
            ) {
                let screenshot_dir = project_root.join("logs");
                let options = crate::capture::ScreenshotOptions {
                    backend: test_config.screenshot_backend,
                    validator: test_config.visual_validator.clone(),
                    golden: test_config.visual_config.clone(),
                    scene: None,
                    run_label: None,
                    window_target: None,
                    allow_vnc_input: false,
                };
                let run_visual_results =
                    crate::capture::capture_on_failure(config, &screenshot_dir, run_num, &options)
                        .await;
                visual_results.extend(run_visual_results);
            }

            crate::run::kill_games(backend, target_vms, binary_name).await;
            if test_config.stop_on_failure {
                break;
            }
        } else {
            crate::run::kill_games(backend, target_vms, binary_name).await;
        }

        // Track exit codes for history
        exit_codes.push(recorded_exit_code);

        session.runs.push(RunResult {
            run_number: run_num,
            status: run_status,
            exit_code: recorded_exit_code,
            exit_label: run_label,
            duration: run_start.elapsed(),
        });

        // Track consecutive identical failures
        if let Some(code) = failure_code {
            if consecutive_fail_code == Some(code) {
                consecutive_fail_count += 1;
            } else {
                consecutive_fail_code = Some(code);
                consecutive_fail_count = 1;
            }
            if !test_config.stop_on_failure && consecutive_fail_count >= REPEAT_FAILURE_LIMIT {
                let label: Cow<'static, str> = if code < 0 {
                    Cow::Borrowed(timeout_failure_label(code))
                } else {
                    Cow::Owned(format!(
                        "{code} ({})",
                        exit_code_label(code, &test_config.exit_codes)
                    ))
                };
                print_and_push(
                    &mut output,
                    &format!(
                        "\n--- EARLY STOP: {consecutive_fail_count} consecutive identical failures (exit {label}) ---"
                    ),
                    verbose,
                );
                break;
            }
        } else {
            consecutive_fail_code = None;
            consecutive_fail_count = 0;
        }

        output.push('\n');
    }

    // Reset chaos if it was applied
    if test_config.chaos.is_some() {
        let _ = crate::netem::reset(config, None);
    }

    session.total_duration = session_start.elapsed();

    // Summary
    let total = passed + failed;
    let timeout_info: Cow<'static, str> = if timed_out_count > 0 {
        Cow::Owned(format!(", {timed_out_count} timed out"))
    } else {
        Cow::Borrowed("")
    };
    let summary = format!(
        "\n=== SUMMARY ===\n{passed}/{total} passed, {failed} failed{timeout_info}\nConfig: network={}, players={}, vms={vm_count}, timeout={}s, hard_timeout={}s",
        test_config.network,
        test_config.players,
        test_config.timeout.as_secs(),
        test_config.hard_timeout.as_secs(),
    );
    print_and_push(&mut output, &summary, verbose);

    // Inline flaky detection (when multiple runs)
    if test_config.max_runs > 1 {
        let flaky = crate::history::compute_flakiness(&exit_codes, &test_config.exit_codes);
        for entry in &flaky {
            if matches!(entry.pattern, crate::history::FlakPattern::Intermittent) {
                print_and_push(
                    &mut output,
                    &format!(
                        "  FLAKY: exit {} ({}) appeared in {}/{} runs (score: {:.0}%)",
                        entry.exit_code,
                        entry.label,
                        entry.occurrences,
                        entry.total_runs,
                        entry.flakiness_score * 100.0
                    ),
                    verbose,
                );
            }
        }
    }

    // Emit formatted output
    match test_config.output_format {
        OutputFormat::Text => {
            if let Some(path) = &test_config.output_file {
                std::fs::write(path, &output)?;
                if verbose {
                    println!("Output written to {}", path.display());
                }
            }
        }
        OutputFormat::Junit => {
            let xml = crate::harness::output::emit_junit(&session);
            if let Some(path) = &test_config.output_file {
                std::fs::write(path, &xml)?;
                if verbose {
                    println!("JUnit XML written to {}", path.display());
                }
            } else if verbose {
                println!("{xml}");
            }
        }
        OutputFormat::Jsonl => {
            let jsonl = crate::harness::output::emit_jsonl(&session);
            if let Some(path) = &test_config.output_file {
                std::fs::write(path, &jsonl)?;
                if verbose {
                    println!("JSON Lines written to {}", path.display());
                }
            } else if verbose {
                println!("{jsonl}");
            }
        }
    }

    // Capture git SHA for history
    let git_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    // Compute per-run durations
    let run_durations: Vec<f64> = session
        .runs
        .iter()
        .map(|r| r.duration.as_secs_f64())
        .collect();

    // Save to test history
    let result = crate::history::make_result(
        &test_config.network.to_string(),
        test_config.players,
        vm_count,
        test_config.max_runs,
        test_config.max_runs,
        passed,
        failed,
        timed_out_count,
        test_config.timeout.as_secs(),
        exit_codes,
        git_sha,
        session.total_duration.as_secs_f64(),
        run_durations,
        visual_results,
        gpu_preflight.clone(),
        readiness.clone(),
    );
    if let Err(e) = crate::history::save_result(&config.state_dir, result) {
        eprintln!("Warning: failed to save test history: {e}");
    }

    // Run notification hooks
    let hook_vars = crate::ui::hooks::HookVars {
        pass_count: passed,
        fail_count: failed,
        total_count: total,
        timeout_count: timed_out_count,
        pass_rate: passed
            .checked_mul(100)
            .and_then(|count| count.checked_div(total))
            .unwrap_or(0),
        duration: session.total_duration,
        exit_label: session
            .runs
            .iter()
            .rev()
            .find(|r| r.status != RunStatus::Pass)
            .map(|r| r.exit_label.clone())
            .unwrap_or_default(),
        network: test_config.network.to_string(),
        players: test_config.players,
        cluster_name: config.cluster_name.clone(),
    };

    if let Some(ref cmd) = test_config.on_complete {
        crate::ui::hooks::run_hook(cmd, &hook_vars).await;
    }
    if failed > 0 {
        if let Some(ref cmd) = test_config.on_failure {
            crate::ui::hooks::run_hook(cmd, &hook_vars).await;
        }
    }

    if failed > 0 {
        anyhow::bail!("{output}\n{failed}/{total} runs failed");
    }
    Ok(output)
}

// ── Helpers ──

fn print_and_push(output: &mut String, msg: &str, verbose: bool) {
    if verbose {
        println!("{msg}");
    }
    output.push_str(msg);
    output.push('\n');
}

fn find_scene_vm<'a>(
    target_vms: &'a [crate::core::config::VmDef],
    scene_vm: Option<&str>,
) -> anyhow::Result<&'a crate::core::config::VmDef> {
    match scene_vm {
        Some(target) => {
            let normalized = target
                .strip_prefix("vm-")
                .map(|id| format!("vm-{id}"))
                .unwrap_or_else(|| {
                    if target.bytes().all(|b| b.is_ascii_digit()) {
                        format!("vm-{target}")
                    } else {
                        target.to_string()
                    }
                });
            target_vms
                .iter()
                .find(|vm| vm.name.0 == normalized)
                .ok_or_else(|| anyhow::anyhow!("scene VM '{target}' not found in this run"))
        }
        None => target_vms
            .first()
            .ok_or_else(|| anyhow::anyhow!("scene capture requires at least one VM")),
    }
}

fn scene_capture_backend(
    visual: &crate::core::config::VisualConfig,
    scene: &crate::core::config::Scene,
) -> crate::cli::ScreenshotBackend {
    scene
        .backend
        .or(visual.default_backend)
        .map(|backend| match backend {
            crate::core::config::VisualBackend::Grim => crate::cli::ScreenshotBackend::Grim,
            crate::core::config::VisualBackend::Vnc => crate::cli::ScreenshotBackend::Vnc,
        })
        .unwrap_or(crate::cli::ScreenshotBackend::Vnc)
}

/// Launch game on an instance (detached via nohup).
/// Build a leading-space-separated string of `KEY='VALUE'` pairs suitable for
/// inlining inside an `export ...` clause in the SSH bash -c command. Values
/// containing `'` are escaped with the standard `'\''` trick. Keep keys and
/// values to printable ASCII without embedded `\0` — this is not a general
/// shell escaper, just enough for our ids and tags.
fn format_inline_exports(env: &std::collections::BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for (k, v) in env {
        let escaped = v.replace('\'', "'\\''");
        out.push_str(&format!(" {k}='{escaped}'"));
    }
    out
}

/// Build the bash -c command we hand to SSH on the VM to launch the game.
///
/// Caller-supplied env (e.g. `THESPAN_SESSION_ID` from the auto-set or
/// profile `env` table) is appended to the same `export` so the game binary
/// sees it. We inline-export rather than relying on SSH `SetEnv`/`AcceptEnv`
/// because those require sshd cooperation we don't control across distros /
/// image rebuilds.
///
/// Pulled out so we can assert the exact command shape from a unit test.
fn build_vm_launch_cmd(
    remote_dir: &str,
    binary_name: &str,
    vm_user: &str,
    log_file: &str,
    args: &str,
    heartbeat_dir: &str,
    extra_env: &std::collections::BTreeMap<String, String>,
) -> String {
    let extra_exports = format_inline_exports(extra_env);
    format!(
        "mkdir -p {remote_dir}/{heartbeat_dir} && \
         cd {remote_dir} && \
         . /etc/profile.d/steampipe-graphics.sh 2>/dev/null || true && \
         export LD_LIBRARY_PATH=\"{remote_dir}:${{STEAMPIPE_GRAPHICS_LIB_PATH:-}}:$LD_LIBRARY_PATH\" \
         XDG_RUNTIME_DIR=/tmp/runtime-{vm_user} \
         WAYLAND_DISPLAY=wayland-1 \
         WGPU_BACKEND=vulkan \
         STEAMPIPE_HEARTBEAT_DIR={remote_dir}/{heartbeat_dir}{extra_exports} && \
         nohup ./{binary_name} {args} > {log_file} 2>&1 < /dev/null & disown"
    )
}

async fn launch_game(
    backend: &Backend,
    ip: &str,
    remote_dir: &str,
    binary_name: &str,
    vm_user: &str,
    log_file: &str,
    args: &str,
    heartbeat_dir: &str,
    extra_env: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<()> {
    // Per-cluster heartbeat dir on the VM (`<remote_dir>/<heartbeat_dir>`)
    // mirrors the host-side scoping. Lease partitioning already guarantees one
    // game per VM at a time, but scoping per cluster name means a prior owner's
    // stale files never poison a new owner's read, even before game-side
    // `cleanup_stale_heartbeat_files` runs.
    // Prepend the graphical-app library path published by the vm-graphics
    // NixOS module (libxkbcommon, libwayland, libGL, libstdc++).
    // SSH non-login non-interactive sessions don't source /etc/profile, so we
    // pull the value from /etc/profile.d here instead of relying on shell init.
    // The variable is empty/unset on VMs without graphics, so a no-op there.
    let cmd = build_vm_launch_cmd(
        remote_dir,
        binary_name,
        vm_user,
        log_file,
        args,
        heartbeat_dir,
        extra_env,
    );
    // Use run_cmd_timeout to avoid hanging if the channel doesn't close
    let result = backend
        .run_cmd_timeout(ip, &cmd, Duration::from_secs(5))
        .await;
    if !result.success {
        // Timeout is OK here — the command backgrounds successfully but SSH may not close the channel
        if result.stderr.contains("timed out") {
            return Ok(());
        }
        anyhow::bail!("{}", result.stderr);
    }
    Ok(())
}

async fn collect_vm_log(
    backend: &Backend,
    ip: &str,
    remote_dir: &str,
    log_file: &str,
    max_lines: usize,
) -> String {
    let cmd = format!("tail -n {max_lines} {remote_dir}/{log_file} 2>/dev/null || echo '(no log)'");
    backend.run_cmd(ip, &cmd).await.stdout
}

fn cleanup_heartbeat_files(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("game_progress_") && name.ends_with(".json") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Per-cluster runtime directory under the project root.
///
/// Two parallel `cluster_test` invocations with distinct cluster names get
/// distinct heartbeat dirs and strace paths so they cannot wipe or merge each
/// other's progress files. Returns
/// `<project_root>/.steampipe-runtime/<cluster_name>/`.
fn cluster_runtime_dir(project_root: &Path, cluster_name: &str) -> std::path::PathBuf {
    project_root.join(".steampipe-runtime").join(cluster_name)
}

fn ensure_cluster_runtime_dirs(project_root: &Path, cluster_name: &str) -> std::path::PathBuf {
    let heartbeat_dir = cluster_runtime_dir(project_root, cluster_name).join("heartbeats");
    let _ = std::fs::create_dir_all(&heartbeat_dir);
    heartbeat_dir
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn timeout_failure_code(kind: TimeoutKind) -> i32 {
    match kind {
        TimeoutKind::HeartbeatStall => -1,
        TimeoutKind::NoProgressTimeout => -2,
        TimeoutKind::HardTimeout => -3,
    }
}

fn timeout_failure_label(code: i32) -> &'static str {
    match code {
        -1 => TimeoutKind::HeartbeatStall.label(),
        -2 => TimeoutKind::NoProgressTimeout.label(),
        -3 => TimeoutKind::HardTimeout.label(),
        _ => "TIMEOUT",
    }
}

// ── Process monitoring ──

/// Monitor the local process with timeout and heartbeat detection.
/// Runs synchronously (called from spawn_blocking).
fn monitor_local_process(
    child: &mut std::process::Child,
    timeout: Duration,
    hard_timeout: Duration,
    shutdown_timeout: Duration,
    local_heartbeat_dir: &Path,
    vm_ips: &[(VmName, IpAddr)],
    backend: &Backend,
    remote_dir: &str,
    vm_heartbeat_subdir: &str,
    heartbeat_stall_secs: u64,
    stall_capture: Option<StallCaptureContext>,
) -> anyhow::Result<(String, Option<i32>, Option<MonitorFailure>)> {
    use std::io::Read as _;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = stdout_pipe {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut pipe) = stderr_pipe {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });

    let start = Instant::now();
    let exit_code;
    let mut failure = None;
    let mut local_state = HeartbeatMonitorState::default();
    let mut vm_state = HeartbeatMonitorState::default();
    let mut last_heartbeat_check = Instant::now();
    let mut last_vm_check = Instant::now();

    let rt = tokio::runtime::Handle::current();
    let local_hb = LocalHeartbeat {
        dir: local_heartbeat_dir,
    };
    let vm_hb = VmHeartbeat {
        rt: &rt,
        backend,
        vms: vm_ips,
        remote_dir,
        heartbeat_subdir: vm_heartbeat_subdir,
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) if start.elapsed() > hard_timeout => {
                exit_code = kill_and_reap(child, shutdown_timeout);
                let now = now_ms() as u64;
                let runtime_secs = start.elapsed().as_secs();
                let (source, stalled_secs, snapshot) =
                    select_hard_timeout_context(now, &local_state, &vm_state);
                failure = Some(MonitorFailure::hard_timeout(
                    runtime_secs,
                    hard_timeout.as_secs(),
                    source,
                    stalled_secs,
                    snapshot,
                ));
                break;
            }
            Ok(None) => {
                if last_heartbeat_check.elapsed() > Duration::from_secs(2) {
                    last_heartbeat_check = Instant::now();
                    if let Some(stall) = check_heartbeat_source(
                        "local",
                        &local_hb,
                        &mut local_state,
                        heartbeat_stall_secs,
                        timeout,
                        now_ms() as u64,
                    ) {
                        if let Some(context) = &stall_capture {
                            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
                            let _ = rt.block_on(capture_reason_frames(context, "stall", &stamp));
                        }
                        exit_code = kill_and_reap(child, shutdown_timeout);
                        failure = Some(stall);
                        break;
                    }
                }

                if last_vm_check.elapsed() > Duration::from_secs(10) {
                    last_vm_check = Instant::now();
                    if let Some(stall) = check_heartbeat_source(
                        "vm",
                        &vm_hb,
                        &mut vm_state,
                        heartbeat_stall_secs,
                        timeout,
                        now_ms() as u64,
                    ) {
                        if let Some(context) = &stall_capture {
                            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
                            let _ = rt.block_on(capture_reason_frames(context, "stall", &stamp));
                        }
                        exit_code = kill_and_reap(child, shutdown_timeout);
                        failure = Some(stall);
                        break;
                    }
                }

                std::thread::sleep(Duration::from_millis(200));
            }
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok((format!("{stdout}{stderr}"), exit_code, failure))
}

/// Kill the process group and reap the exit code.
fn kill_and_reap(child: &mut std::process::Child, shutdown_timeout: Duration) -> Option<i32> {
    kill_process_group(child, shutdown_timeout);
    child.try_wait().ok().flatten().and_then(|s| s.code())
}

fn kill_process_group(child: &mut std::process::Child, shutdown_timeout: Duration) {
    let pid = child.id();

    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{pid}"))
            .output();
    }

    let term_start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if term_start.elapsed() > shutdown_timeout => {
                #[cfg(unix)]
                {
                    let _ = std::process::Command::new("kill")
                        .arg("-KILL")
                        .arg(format!("-{pid}"))
                        .output();
                }
                let _ = child.wait();
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Map a game process exit code to a human-readable failure label.
///
/// Checks the config-driven map first, then falls back to hardcoded defaults.
pub fn exit_code_label(code: i32, custom: &HashMap<i32, String>) -> Cow<'static, str> {
    if let Some(label) = custom.get(&code) {
        return Cow::Owned(label.clone());
    }
    Cow::Borrowed(match code {
        0 => "SUCCESS",
        1 => "ERROR",
        10 => "PHASE_WATCHDOG",
        11 => "DAG_VIOLATION",
        12 => "DESYNC",
        13 => "CONNECTION_LOST",
        14 => "INVARIANT_VIOLATION",
        15 => "COMMITMENT_FAILURE",
        16 => "FORMATION_WATCHDOG",
        17 => "MOVE_RESEND_WATCHDOG",
        18 => "PEER_READY_TIMEOUT",
        124 => "TIMEOUT",
        _ => "UNKNOWN",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_heartbeats_single() {
        let input = r#"{"timestamp_ms":1000,"phase":"Battle","round":1,"turn":2,"progress_seq":3}"#;
        let parsed = parse_heartbeats(input);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].timestamp_ms, 1000);
        assert_eq!(parsed[0].phase, "Battle");
        assert_eq!(parsed[0].round, 1);
        assert_eq!(parsed[0].turn, 2);
        assert_eq!(parsed[0].progress_seq, 3);
    }

    #[test]
    fn local_display_auto_start_gates_on_host_args_and_existing_display() {
        assert!(should_auto_start_local_display(
            crate::cli::DisplayMode::Sway,
            "--auto-host-steam",
            false
        ));
        assert!(!should_auto_start_local_display(
            crate::cli::DisplayMode::Sway,
            "--auto-host-steam --headless",
            false
        ));
        assert!(!should_auto_start_local_display(
            crate::cli::DisplayMode::Sway,
            "--auto-host-steam",
            true
        ));
        assert!(should_auto_start_local_display(
            crate::cli::DisplayMode::Headless,
            "--auto-host-steam",
            false
        ));
    }

    /// Empty env table → empty inline-export string. Production runs that
    /// don't set any session id must produce a bash `export ...` clause
    /// byte-identical to the pre-feature path.
    #[test]
    fn format_inline_exports_empty() {
        let env = std::collections::BTreeMap::new();
        assert_eq!(format_inline_exports(&env), "");
    }

    /// Populated env table → leading-space-separated `KEY='VALUE'` pairs in
    /// `BTreeMap` (sorted) order, ready to drop into an `export ...` clause.
    #[test]
    fn format_inline_exports_populates() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("THESPAN_SESSION_ID".into(), "fixloop-uifull-1v1-lan".into());
        env.insert("RUST_LOG".into(), "info".into());
        let out = format_inline_exports(&env);
        // BTreeMap iterates in lexicographic key order.
        assert_eq!(
            out,
            " RUST_LOG='info' THESPAN_SESSION_ID='fixloop-uifull-1v1-lan'"
        );
    }

    /// Single quotes in values are escaped with `'\''` so the surrounding
    /// `'...'` quoting in the bash command stays intact.
    #[test]
    fn format_inline_exports_escapes_single_quote() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("WEIRD".into(), "a'b".into());
        assert_eq!(format_inline_exports(&env), " WEIRD='a'\\''b'");
    }

    /// Production path: when no env entries are provided, the VM bash -c
    /// command defaults graphical clients to Vulkan and leaves no stray
    /// exports or trailing whitespace before the trailing `&&`.
    #[test]
    fn build_vm_launch_cmd_empty_env_unchanged() {
        let env = std::collections::BTreeMap::new();
        let cmd = build_vm_launch_cmd(
            "/home/u/cb",
            "chessbender",
            "u",
            "game.log",
            "--auto-join-udp --auto-play",
            ".steampipe-runtime/1v1/heartbeats",
            &env,
        );
        assert!(
            cmd.contains("WGPU_BACKEND=vulkan"),
            "expected Vulkan-first default in VM launch env, got: {cmd}",
        );
        // No caller-supplied KEY='VAL' shows up between
        // `STEAMPIPE_HEARTBEAT_DIR=...` and the trailing ` &&`.
        assert!(
            cmd.contains("STEAMPIPE_HEARTBEAT_DIR=/home/u/cb/.steampipe-runtime/1v1/heartbeats &&"),
            "expected production-shape export tail, got: {cmd}",
        );
        assert!(
            !cmd.contains("THESPAN_SESSION_ID="),
            "expected no env injection: {cmd}"
        );
    }

    /// With session id set, the bash command exports it on the same line
    /// as the other test-runner env so the VM-side game inherits it.
    #[test]
    fn build_vm_launch_cmd_injects_session_id() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("THESPAN_SESSION_ID".into(), "1v1-uifull-12345".into());
        let cmd = build_vm_launch_cmd(
            "/home/u/cb",
            "chessbender",
            "u",
            "game.log",
            "--auto-join-udp --auto-play",
            ".steampipe-runtime/1v1-uifull/heartbeats",
            &env,
        );
        assert!(
            cmd.contains("THESPAN_SESSION_ID='1v1-uifull-12345' &&"),
            "expected session id on the same export line, got: {cmd}",
        );
        // The injected env must precede the `nohup ./chessbender ...` so it
        // applies to the game process, not to a later subshell.
        let session_idx = cmd.find("THESPAN_SESSION_ID=").unwrap();
        let nohup_idx = cmd.find("nohup ").unwrap();
        assert!(
            session_idx < nohup_idx,
            "session id must export before launch: {cmd}"
        );
    }

    #[test]
    fn build_vm_launch_cmd_profile_env_can_override_vulkan_default() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("GALLIUM_DRIVER".into(), "virgl".into());
        env.insert("MESA_LOADER_DRIVER_OVERRIDE".into(), "virtio_gpu".into());
        env.insert("WGPU_BACKEND".into(), "gl".into());
        let cmd = build_vm_launch_cmd(
            "/home/u/cb",
            "chessbender",
            "u",
            "game.log",
            "--auto-join-udp --auto-play",
            ".steampipe-runtime/1v1-uifull-egl/heartbeats",
            &env,
        );

        let default_idx = cmd.find("WGPU_BACKEND=vulkan").unwrap();
        let override_idx = cmd.find("WGPU_BACKEND='gl'").unwrap();
        let mesa_idx = cmd
            .find("MESA_LOADER_DRIVER_OVERRIDE='virtio_gpu'")
            .unwrap();
        let gallium_idx = cmd.find("GALLIUM_DRIVER='virgl'").unwrap();
        let nohup_idx = cmd.find("nohup ").unwrap();
        assert!(
            default_idx < override_idx,
            "profile env must override Vulkan default: {cmd}"
        );
        assert!(
            override_idx < nohup_idx && mesa_idx < nohup_idx && gallium_idx < nohup_idx,
            "EGL override env must apply to game launch: {cmd}"
        );
    }

    #[test]
    fn fatal_output_marker_detects_false_pass_signals() {
        assert_eq!(
            fatal_output_marker("thread 'main' panicked at src/main.rs:1"),
            Some("PANIC")
        );
        assert_eq!(
            fatal_output_marker("[FATAL] Steam client unavailable"),
            Some("FATAL")
        );
        assert_eq!(
            fatal_output_marker("Graceful shutdown: exit_code=19 (STEAM_UNAVAILABLE)"),
            Some("STEAM_UNAVAILABLE")
        );
        assert_eq!(
            fatal_output_marker(
                "GracefulShutdownRequest received (exit_code=17, MOVE_RESEND_WATCHDOG)"
            ),
            Some("MOVE_RESEND_WATCHDOG")
        );
        assert_eq!(
            fatal_output_marker("GracefulShutdownRequest received (exit_code=42, CUSTOM_ERROR)"),
            Some("GRACEFUL_SHUTDOWN_ERROR")
        );
        assert_eq!(
            fatal_output_marker("Graceful shutdown: exit_code=0 (SUCCESS)"),
            None
        );
    }

    #[test]
    fn parse_heartbeats_concatenated_objects() {
        let input = concat!(
            "{",
            "\"timestamp_ms\":3000,\"phase\":\"Battle\",\"round\":2,\"turn\":1,\"progress_seq\":4",
            "}\n",
            "{",
            "\"timestamp_ms\":1000,\"phase\":\"Draft\",\"round\":1,\"turn\":1,\"progress_seq\":1",
            "}\n",
        );
        let parsed = parse_heartbeats(input);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].progress_seq, 4);
        assert_eq!(parsed[1].progress_seq, 1);
    }

    #[test]
    fn parse_heartbeats_empty() {
        assert!(parse_heartbeats("").is_empty());
        assert!(parse_heartbeats("not json").is_empty());
    }

    fn heartbeat(
        timestamp_ms: u64,
        phase: &str,
        round: u32,
        turn: u32,
        progress_seq: u64,
    ) -> HeartbeatSnapshot {
        HeartbeatSnapshot {
            timestamp_ms,
            phase: phase.to_string(),
            round,
            turn,
            progress_seq,
        }
    }

    struct FakeHeartbeat(Vec<HeartbeatSnapshot>);
    impl HeartbeatSource for FakeHeartbeat {
        fn snapshots(&self) -> Vec<HeartbeatSnapshot> {
            self.0.clone()
        }
    }

    #[test]
    fn observe_heartbeat_source_tracks_oldest_timestamp_and_slowest_progress() {
        let hb = FakeHeartbeat(vec![
            heartbeat(3000, "Battle", 2, 1, 4),
            heartbeat(1000, "Draft", 1, 1, 2),
            heartbeat(2000, "Battle", 1, 3, 1),
        ]);
        let observation = observe_heartbeat_source(&hb).expect("heartbeat observation");
        assert_eq!(observation.oldest_timestamp.timestamp_ms, 1000);
        assert_eq!(observation.slowest_progress.progress_seq, 1);
        assert_eq!(observation.slowest_progress.turn, 3);
    }

    #[test]
    fn check_heartbeat_source_initializes_without_failing() {
        let hb = FakeHeartbeat(vec![heartbeat(1000, "Draft", 1, 1, 0)]);
        let mut state = HeartbeatMonitorState::default();
        let failure =
            check_heartbeat_source("local", &hb, &mut state, 30, Duration::from_secs(30), 1000);
        assert!(failure.is_none());
        assert_eq!(state.last_timestamp_ms, Some(1000));
        assert_eq!(state.last_progress_at_ms, Some(1000));
    }

    #[test]
    fn check_heartbeat_source_detects_heartbeat_stall() {
        let hb = FakeHeartbeat(vec![heartbeat(1000, "Battle", 1, 1, 5)]);
        let mut state = HeartbeatMonitorState::default();
        assert!(
            check_heartbeat_source("vm", &hb, &mut state, 30, Duration::from_secs(60), 1000)
                .is_none()
        );

        let failure =
            check_heartbeat_source("vm", &hb, &mut state, 30, Duration::from_secs(60), 32_000)
                .expect("heartbeat stall");
        assert_eq!(failure.kind, TimeoutKind::HeartbeatStall);
        assert!(failure.banner.contains("HEARTBEAT_STALL"));
    }

    #[test]
    fn check_heartbeat_source_detects_no_progress_timeout() {
        let hb = FakeHeartbeat(vec![heartbeat(1000, "Battle", 1, 1, 5)]);
        let mut state = HeartbeatMonitorState::default();
        assert!(
            check_heartbeat_source("local", &hb, &mut state, 45, Duration::from_secs(30), 1000)
                .is_none()
        );

        let updated_timestamp = FakeHeartbeat(vec![heartbeat(20_000, "Battle", 1, 1, 5)]);
        let failure = check_heartbeat_source(
            "local",
            &updated_timestamp,
            &mut state,
            45,
            Duration::from_secs(30),
            32_000,
        )
        .expect("no progress timeout");
        assert_eq!(failure.kind, TimeoutKind::NoProgressTimeout);
        assert!(failure.banner.contains("NO_PROGRESS_TIMEOUT"));
    }

    #[test]
    fn check_heartbeat_source_accepts_tuple_change_with_same_progress_seq() {
        let mut state = HeartbeatMonitorState::default();
        let initial = FakeHeartbeat(vec![heartbeat(1000, "Draft", 1, 1, 0)]);
        assert!(
            check_heartbeat_source(
                "local",
                &initial,
                &mut state,
                45,
                Duration::from_secs(30),
                1000,
            )
            .is_none()
        );

        let phase_changed = FakeHeartbeat(vec![heartbeat(31_000, "Battle", 1, 1, 0)]);
        assert!(
            check_heartbeat_source(
                "local",
                &phase_changed,
                &mut state,
                45,
                Duration::from_secs(30),
                31_000,
            )
            .is_none()
        );
        assert_eq!(state.last_progress_at_ms, Some(31_000));
    }

    #[test]
    fn resolve_hard_timeout_uses_explicit_override() {
        let timeout = Duration::from_secs(120);
        assert_eq!(
            resolve_hard_timeout(timeout, Some(Duration::from_secs(900))),
            Duration::from_secs(900)
        );
    }

    #[test]
    fn resolve_hard_timeout_derives_generous_default() {
        assert_eq!(
            resolve_hard_timeout(Duration::from_secs(120), None),
            Duration::from_secs(1_800)
        );
        assert_eq!(
            resolve_hard_timeout(Duration::from_secs(600), None),
            Duration::from_secs(3_000)
        );
    }

    #[test]
    fn vnc_backend_rejects_non_sway_display() {
        let err = validate_visual_capture_config(
            crate::cli::DisplayMode::WestonGpu,
            crate::cli::ScreenshotBackend::Vnc,
        )
        .unwrap_err();
        assert!(err.to_string().contains("sway"));
    }

    #[test]
    fn vnc_backend_accepts_sway_gpu_display() {
        validate_visual_capture_config(
            crate::cli::DisplayMode::SwayGpu,
            crate::cli::ScreenshotBackend::Vnc,
        )
        .unwrap();
    }

    #[test]
    fn grim_backend_rejects_gpu_display() {
        let err = validate_visual_capture_config(
            crate::cli::DisplayMode::SwayGpu,
            crate::cli::ScreenshotBackend::Grim,
        )
        .unwrap_err();
        assert!(err.to_string().contains("vnc"));
    }

    #[test]
    fn grim_backend_accepts_non_gpu_sway() {
        validate_visual_capture_config(
            crate::cli::DisplayMode::Sway,
            crate::cli::ScreenshotBackend::Grim,
        )
        .unwrap();
    }

    #[test]
    fn exit_code_labels_default() {
        let empty = HashMap::new();
        assert_eq!(&*exit_code_label(0, &empty), "SUCCESS");
        assert_eq!(&*exit_code_label(1, &empty), "ERROR");
        assert_eq!(&*exit_code_label(10, &empty), "PHASE_WATCHDOG");
        assert_eq!(&*exit_code_label(124, &empty), "TIMEOUT");
        assert_eq!(&*exit_code_label(999, &empty), "UNKNOWN");
    }

    #[test]
    fn exit_code_labels_custom() {
        let mut custom = HashMap::new();
        custom.insert(42, "MY_ERROR".to_string());
        custom.insert(0, "OK".to_string());
        assert_eq!(&*exit_code_label(42, &custom), "MY_ERROR");
        assert_eq!(&*exit_code_label(0, &custom), "OK");
        assert_eq!(&*exit_code_label(10, &custom), "PHASE_WATCHDOG");
    }

    #[test]
    fn timeout_failure_code_labels_are_stable() {
        assert_eq!(timeout_failure_code(TimeoutKind::HeartbeatStall), -1);
        assert_eq!(timeout_failure_code(TimeoutKind::NoProgressTimeout), -2);
        assert_eq!(timeout_failure_code(TimeoutKind::HardTimeout), -3);
        assert_eq!(timeout_failure_label(-1), "HEARTBEAT_STALL");
        assert_eq!(timeout_failure_label(-2), "NO_PROGRESS_TIMEOUT");
        assert_eq!(timeout_failure_label(-3), "HARD_TIMEOUT");
    }
}
