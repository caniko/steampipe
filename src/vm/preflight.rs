//! Pre-flight checks: self-healing cleanup, diagnostics, and connectivity verification.
//!
//! Runs before MCP tool invocation to ensure a clean state:
//! - Stale PID files from dead VM processes
//! - Orphaned microvm processes with no valid lease claim
//! - Leftover socket files in VM state directories
//!
//! Also verifies VM→host connectivity for tests, plus GPU/Vulkan/Wayland
//! readiness for graphics-enabled VMs.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::backend::Backend;
use crate::core::config::{BackendKind, BridgeReady, ClusterConfig, IpAddr, VmDef, par_each_vm};
use crate::core::state;
use crate::vm::lease;

const GPU_REQUIRED_DRI_DEVICES: &[&str] = &["/dev/dri/card0", "/dev/dri/renderD128"];
const GPU_REQUIRED_VULKAN_EXTENSIONS: &[&str] = &[
    "VK_KHR_swapchain",
    "VK_KHR_external_memory_fd",
    "VK_EXT_external_memory_dma_buf",
];
const GPU_REQUIRED_WAYLAND_INTERFACES: &[&str] = &[
    "wl_compositor",
    "zwp_linux_dmabuf_v1",
    "zxdg_output_manager_v1",
];

/// Clean up stale state: dead PIDs, orphaned processes, leftover sockets.
///
/// Scans the full configured VM pool regardless of which VMs the current config
/// targets, since orphans may exist on slots outside the current cluster.
///
/// Returns a summary of what was cleaned, or `None` if everything was clean.
pub fn cleanup_stale_state<S>(config: &ClusterConfig<S>) -> Option<String> {
    let mut cleaned = Vec::new();

    for id in 1..=config.max_vms {
        let vm_name = format!("vm-{id}");

        if let Some(pid) = state::read_pid(&config.state_dir, &vm_name)
            && !state::is_pid_alive(pid)
        {
            state::remove_pid(&config.state_dir, &vm_name);
            cleaned.push(format!("{vm_name}: removed stale PID {pid}"));
        }

        let pattern = format!("microvm@{vm_name}");
        if let Ok(out) = std::process::Command::new("pgrep")
            .args(["-f", &pattern])
            .output()
            && out.status.success()
        {
            let has_claim = lease::probe_holder(id, &config.lock_dir).is_some();
            if !has_claim {
                let pids = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let _ = std::process::Command::new("pkill")
                    .args(["-f", &pattern])
                    .status();
                cleaned.push(format!("{vm_name}: killed orphan PIDs {pids}"));
            }
        }

        let vm_dir = config.state_dir.join(&vm_name);
        if vm_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&vm_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(fname) = path.file_name().and_then(|n| n.to_str())
                    && state::is_socket_file(fname)
                {
                    let _ = std::fs::remove_file(&path);
                    cleaned.push(format!("{vm_name}: removed {fname}"));
                }
            }
        }
    }

    if cleaned.is_empty() {
        None
    } else {
        Some(format!("Pre-flight cleanup:\n  {}", cleaned.join("\n  ")))
    }
}

/// Verify that at least one VM can TCP-connect back to the host.
pub async fn ensure_vm_to_host_connectivity<S>(
    config: &ClusterConfig<S>,
    backend: &Backend,
    target_vms: &[VmDef],
) -> anyhow::Result<()> {
    if !matches!(backend, Backend::MicroVm(_)) {
        return Ok(());
    }

    let Some(vm) = target_vms.first() else {
        return Ok(());
    };

    let listener = std::net::TcpListener::bind(format!("{}:0", config.host_ip)).map_err(|e| {
        anyhow::anyhow!(
            "Cannot bind to {} — is the bridge up? ({})\n  Run: just cluster-net-up",
            config.host_ip,
            e
        )
    })?;
    let port = listener.local_addr()?.port();

    if probe_vm_to_host(backend, &vm.ip, &config.host_ip, port).await {
        return Ok(());
    }

    eprintln!(
        "[preflight] VM {} cannot reach host {}:{} — checking nftables...",
        vm.name, config.host_ip, port
    );

    if has_nixos_fw_bridge_rule(&config.bridge) {
        anyhow::bail!(
            "VM→host connectivity failed but the nixos-fw accept rule for {br}\n\
             is present. TCP from {vm} to {host} is blocked by something else\n\
             (NAT, IP forwarding, conflicting rule, etc.). Run `cluster-ctl doctor`.",
            br = config.bridge,
            vm = vm.name,
            host = config.host_ip,
        );
    }

    anyhow::bail!(
        "VM→host connectivity failed and the nixos-fw accept rule for {br}\n\
         is missing. The `services.steampipe-cluster` NixOS module supplies\n\
         this rule via networking.firewall.extraInputRules — rebuild your\n\
         host configuration to apply it (e.g. `sudo nixos-rebuild switch`).\n\
         Steampipe no longer mutates host firewall state at runtime.",
        br = config.bridge,
    );
}

async fn probe_vm_to_host(backend: &Backend, vm_ip: &IpAddr, host_ip: &str, port: u16) -> bool {
    let cmd = format!(
        "timeout 3 bash -c '</dev/tcp/{host_ip}/{port}' 2>/dev/null && echo REACH || echo NOREACH"
    );
    let result = backend
        .run_cmd_timeout(vm_ip, &cmd, Duration::from_secs(6))
        .await;
    result.stdout.trim() == "REACH"
}

fn has_nixos_fw_bridge_rule(bridge: &str) -> bool {
    let output = std::process::Command::new("nft")
        .args(["list", "chain", "inet", "nixos-fw", "input"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.contains(&format!("iifname \"{bridge}\"")) && text.contains("accept")
        }
        _ => false,
    }
}

/// Shell script that checks for a DRM render device inside a VM.
pub fn gpu_check_script() -> &'static str {
    r#"
if ls /dev/dri/card* >/dev/null 2>&1; then
    echo "gpu-ok $(ls /dev/dri/card* | head -1)"
else
    echo "gpu-missing"
    echo "  /dev/dri/ contents:" >&2
    ls -la /dev/dri/ 2>&1 || echo "  /dev/dri/ does not exist" >&2
    exit 1
fi
"#
}

/// Verify that VMs have a DRM device available (required for GPU display modes).
pub async fn ensure_vm_gpu(
    backend: &Backend,
    target_vms: &[VmDef],
    display: crate::cli::DisplayMode,
) -> anyhow::Result<()> {
    if !display.requires_gpu() {
        return Ok(());
    }

    let script = gpu_check_script();
    let mut failures = Vec::new();

    for vm in target_vms {
        let result = backend
            .run_cmd_timeout(&vm.ip, script, Duration::from_secs(10))
            .await;
        if !result.stdout.contains("gpu-ok") {
            failures.push(format!(
                "  {}: no DRM device found (is microvm.graphics.enable = true?)\n    {}",
                vm.name,
                result.stderr.trim(),
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "GPU check failed — display mode \"{display}\" requires virtio-gpu but \
             these VMs have no DRM device:\n{}",
            failures.join("\n"),
        );
    }
}

#[cfg(test)]
pub fn parse_gpu_check(stdout: &str) -> Option<&str> {
    stdout.trim().strip_prefix("gpu-ok ").map(|s| s.trim())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VulkanDevice {
    pub name: String,
    pub driver: Option<String>,
    pub api_version: Option<String>,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProbeResult {
    Pass,
    Fail,
    Skipped,
}

impl ProbeResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skipped => "SKIPPED",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GpuFailureKind {
    NoDevice,
    NoLoaderIcd,
    NoSwapchainExt,
    NoDmabufExt,
    WaylandMissing,
    ProbeCrash,
    Unknown,
}

impl GpuFailureKind {
    fn hint(&self) -> &'static str {
        match self {
            Self::NoDevice => {
                "virtio-gpu is not visible in the guest; verify microvm.graphics.enable and /dev/dri access"
            }
            Self::NoLoaderIcd => {
                "Vulkan loader could not find a usable ICD; verify VK_DRIVER_FILES or VK_ICD_FILENAMES"
            }
            Self::NoSwapchainExt => {
                "Swapchain support is missing; verify virtio-gpu Vulkan/venus is enabled instead of llvmpipe-only fallback"
            }
            Self::NoDmabufExt => {
                "External memory dma-buf support is missing; verify venus/cross-domain virtio-gpu configuration"
            }
            Self::WaylandMissing => {
                "Wayland compositor is missing required interfaces; verify weston/sway started on the GPU path"
            }
            Self::ProbeCrash => {
                "The Vulkan render probe crashed or timed out; inspect vkcube/vulkaninfo output inside the VM"
            }
            Self::Unknown => "Inspect vulkaninfo, wayland-info, and compositor logs inside the VM",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GpuPreflightReport {
    pub vm: String,
    pub dri_devices: Vec<String>,
    pub module_loaded: bool,
    pub vulkan_devices: Vec<VulkanDevice>,
    pub wayland_interfaces: Vec<String>,
    pub probe_result: ProbeResult,
    pub failure_kind: Option<GpuFailureKind>,
    pub failure_hint: Option<String>,
    pub notes: Vec<String>,
}

impl GpuPreflightReport {
    fn vulkan_label(&self) -> String {
        self.vulkan_devices
            .first()
            .map(|device| device.name.clone())
            .unwrap_or_else(|| "NONE".to_string())
    }

    fn dri_ok(&self) -> bool {
        GPU_REQUIRED_DRI_DEVICES
            .iter()
            .all(|required| self.dri_devices.iter().any(|found| found == required))
    }

    fn wayland_ok(&self) -> bool {
        GPU_REQUIRED_WAYLAND_INTERFACES.iter().all(|required| {
            self.wayland_interfaces
                .iter()
                .any(|found| found == required)
        })
    }

    fn status_cell(ok: bool) -> &'static str {
        if ok { "OK" } else { "FAIL" }
    }

    fn probe_cell(&self) -> String {
        match (&self.probe_result, &self.failure_kind) {
            (ProbeResult::Pass, _) => "PASS".to_string(),
            (ProbeResult::Skipped, _) => "SKIPPED".to_string(),
            (ProbeResult::Fail, Some(kind)) => format!("FAIL: {}", failure_kind_label(kind)),
            (ProbeResult::Fail, None) => "FAIL".to_string(),
        }
    }
}

fn failure_kind_label(kind: &GpuFailureKind) -> &'static str {
    match kind {
        GpuFailureKind::NoDevice => "no device",
        GpuFailureKind::NoLoaderIcd => "no loader/icd",
        GpuFailureKind::NoSwapchainExt => "no swapchain ext",
        GpuFailureKind::NoDmabufExt => "no dma-buf ext",
        GpuFailureKind::WaylandMissing => "wayland missing",
        GpuFailureKind::ProbeCrash => "probe crash",
        GpuFailureKind::Unknown => "unknown",
    }
}

pub async fn gpu_preflight<S>(
    config: &ClusterConfig<S>,
    target: Option<&str>,
    json: bool,
) -> anyhow::Result<Vec<GpuPreflightReport>> {
    let targets = config.resolve_targets(target, false)?;
    let reports = gpu_preflight_running(config, &targets).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        print_gpu_preflight_table(&reports);
    }

    if reports
        .iter()
        .any(|report| report.probe_result != ProbeResult::Pass)
    {
        anyhow::bail!("One or more VMs failed GPU preflight");
    }

    Ok(reports)
}

pub async fn gpu_preflight_with_boot(
    config: &ClusterConfig<BridgeReady>,
    target: Option<&str>,
    runners_dir: &Path,
    json: bool,
) -> anyhow::Result<Vec<GpuPreflightReport>> {
    let targets = config.resolve_targets(target, false)?;
    let reports = gpu_preflight_booting(config, &targets, runners_dir).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        print_gpu_preflight_table(&reports);
    }

    if reports
        .iter()
        .any(|report| report.probe_result != ProbeResult::Pass)
    {
        anyhow::bail!("One or more VMs failed GPU preflight");
    }

    Ok(reports)
}

pub async fn gpu_preflight_reports_for_targets<S>(
    config: &ClusterConfig<S>,
    targets: &[VmDef],
) -> anyhow::Result<Vec<GpuPreflightReport>> {
    gpu_preflight_running(config, targets).await
}

async fn gpu_preflight_running<S>(
    config: &ClusterConfig<S>,
    targets: &[VmDef],
) -> anyhow::Result<Vec<GpuPreflightReport>> {
    let backend = config.backend.clone();
    let vm_user = config.vm_user.clone();

    let mut results = par_each_vm(targets, |vm| {
        let backend = backend.clone();
        let vm_user = vm_user.clone();
        async move {
            if !backend.is_reachable(&vm.ip).await {
                return GpuPreflightReport {
                    vm: vm.name.to_string(),
                    dri_devices: Vec::new(),
                    module_loaded: false,
                    vulkan_devices: Vec::new(),
                    wayland_interfaces: Vec::new(),
                    probe_result: ProbeResult::Skipped,
                    failure_kind: None,
                    failure_hint: None,
                    notes: vec!["VM is not reachable over SSH".to_string()],
                };
            }

            let output = backend
                .run_cmd_timeout(
                    &vm.ip,
                    &gpu_preflight_script(&vm_user),
                    Duration::from_secs(15),
                )
                .await;
            if !output.success && output.stdout.trim().is_empty() {
                return GpuPreflightReport {
                    vm: vm.name.to_string(),
                    dri_devices: Vec::new(),
                    module_loaded: false,
                    vulkan_devices: Vec::new(),
                    wayland_interfaces: Vec::new(),
                    probe_result: ProbeResult::Fail,
                    failure_kind: Some(GpuFailureKind::Unknown),
                    failure_hint: Some(GpuFailureKind::Unknown.hint().to_string()),
                    notes: vec![output.stderr.trim().to_string()],
                };
            }

            parse_gpu_preflight_output(&vm.name, &output.stdout)
        }
    })
    .await?;

    results.sort_by(|a, b| a.vm.cmp(&b.vm));
    Ok(results)
}

async fn gpu_preflight_booting(
    config: &ClusterConfig<BridgeReady>,
    targets: &[VmDef],
    runners_dir: &Path,
) -> anyhow::Result<Vec<GpuPreflightReport>> {
    let mut reports = Vec::with_capacity(targets.len());

    for vm in targets {
        let mut started_here = false;
        if !config.backend.is_reachable(&vm.ip).await {
            config.backend.stop_instance(config, vm);
            crate::core::admission::admit().await;
            let _ = config.backend.start_instance(config, vm, runners_dir)?;
            started_here = true;
            if !config.backend.wait_ready(&vm.ip, 30).await {
                reports.push(GpuPreflightReport {
                    vm: vm.name.to_string(),
                    dri_devices: Vec::new(),
                    module_loaded: false,
                    vulkan_devices: Vec::new(),
                    wayland_interfaces: Vec::new(),
                    probe_result: ProbeResult::Fail,
                    failure_kind: Some(GpuFailureKind::Unknown),
                    failure_hint: Some(GpuFailureKind::Unknown.hint().to_string()),
                    notes: vec!["Booted VM did not become reachable over SSH".to_string()],
                });
                continue;
            }
        }

        let output = config
            .backend
            .run_cmd_timeout(
                &vm.ip,
                &gpu_preflight_script(&config.vm_user),
                Duration::from_secs(15),
            )
            .await;
        reports.push(parse_gpu_preflight_output(&vm.name, &output.stdout));

        if started_here {
            config.backend.stop_instance(config, vm);
        }
    }

    reports.sort_by(|a, b| a.vm.cmp(&b.vm));
    Ok(reports)
}

fn gpu_preflight_script(vm_user: &str) -> String {
    format!(
        r#"set +e
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR" 2>/dev/null || true
if ! pgrep -x weston >/dev/null; then
    weston --renderer=gl --no-config >/tmp/steampipe-gpu-preflight-weston.log 2>&1 &
    for _ in $(seq 1 20); do
        [ -S "$XDG_RUNTIME_DIR/wayland-1" ] && break
        sleep 0.25
    done
fi
export WAYLAND_DISPLAY=wayland-1

probe_args=""
if vkcube --help 2>&1 | grep -q -- '--frames'; then
    probe_args="--frames 1"
elif vkcube --help 2>&1 | grep -q -- '--c'; then
    probe_args="--c 1"
elif vkcube --help 2>&1 | grep -qE '(^|[[:space:]])-c([[:space:]]|,|$)'; then
    probe_args="-c 1"
fi

echo '===DRI==='
ls /dev/dri/card* /dev/dri/renderD* 2>/dev/null || true
echo '===MODULE==='
if [ -d /sys/module/virtio_gpu ]; then
    echo PRESENT
else
    echo MISSING
fi
echo '===VULKAN_SUMMARY==='
vulkaninfo --summary 2>&1 || true
echo '===VULKAN_EXTENSIONS==='
vulkaninfo 2>&1 | grep -E 'VK_(KHR_swapchain|KHR_external_memory_fd|EXT_external_memory_dma_buf)' || true
echo '===WAYLAND==='
wayland-info 2>&1 || true
echo '===PROBE==='
if [ -n "$probe_args" ]; then
    timeout 10 sh -c "vkcube $probe_args" >/tmp/steampipe-gpu-preflight-vkcube.log 2>&1
    rc=$?
    if [ $rc -eq 0 ]; then
        echo PASS
    else
        echo FAIL:$rc
    fi
else
    echo FAIL:flag-unsupported
fi
if [ -f /tmp/steampipe-gpu-preflight-vkcube.log ]; then
    tail -n 80 /tmp/steampipe-gpu-preflight-vkcube.log
fi
"#
    )
}

fn parse_gpu_preflight_output(vm_name: &str, stdout: &str) -> GpuPreflightReport {
    let sections = split_labeled_sections(stdout);
    let vulkan_summary = sections
        .get("VULKAN_SUMMARY")
        .map(String::as_str)
        .unwrap_or("");

    let dri_devices = sections
        .get("DRI")
        .map(|s| parse_dri_devices(s))
        .unwrap_or_default();
    let module_loaded = sections
        .get("MODULE")
        .is_some_and(|s| s.lines().any(|line| line.trim() == "PRESENT"));
    let vulkan_devices = parse_vulkan_devices(
        vulkan_summary,
        sections
            .get("VULKAN_EXTENSIONS")
            .map(String::as_str)
            .unwrap_or(""),
    );
    let wayland_interfaces = sections
        .get("WAYLAND")
        .map(|s| parse_wayland_interfaces(s))
        .unwrap_or_default();
    let probe_section = sections.get("PROBE").map(String::as_str).unwrap_or("");

    let mut notes = Vec::new();
    let (probe_result, failure_kind) = classify_gpu_report(
        &dri_devices,
        module_loaded,
        &vulkan_devices,
        &wayland_interfaces,
        vulkan_summary,
        probe_section,
        &mut notes,
    );

    GpuPreflightReport {
        vm: vm_name.to_string(),
        dri_devices,
        module_loaded,
        vulkan_devices,
        wayland_interfaces,
        probe_result,
        failure_hint: failure_kind.as_ref().map(|kind| kind.hint().to_string()),
        failure_kind,
        notes,
    }
}

fn split_labeled_sections(stdout: &str) -> std::collections::BTreeMap<String, String> {
    let mut sections = std::collections::BTreeMap::new();
    let mut current = None::<String>;
    let mut buf = String::new();

    for line in stdout.lines() {
        if let Some(label) = line
            .strip_prefix("===")
            .and_then(|rest| rest.strip_suffix("==="))
        {
            if let Some(key) = current.take() {
                sections.insert(key, buf.trim().to_string());
                buf.clear();
            }
            current = Some(label.to_string());
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }

    if let Some(key) = current {
        sections.insert(key, buf.trim().to_string());
    }

    sections
}

fn parse_dri_devices(section: &str) -> Vec<String> {
    section
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("/dev/dri/"))
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_vulkan_devices(summary: &str, extensions: &str) -> Vec<VulkanDevice> {
    let extension_set: Vec<String> = extensions
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            GPU_REQUIRED_VULKAN_EXTENSIONS
                .iter()
                .find(|ext| trimmed.contains(**ext))
                .map(|ext| (*ext).to_string())
        })
        .collect();

    let mut devices = Vec::new();
    let mut current_name = None::<String>;
    let mut current_driver = None::<String>;
    let mut current_api = None::<String>;

    for line in summary.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("GPU") {
            if current_name.is_some() {
                devices.push(VulkanDevice {
                    name: current_name.take().unwrap_or_default(),
                    driver: current_driver.take(),
                    api_version: current_api.take(),
                    extensions: extension_set.clone(),
                });
            }
            current_name = rest
                .split_once('=')
                .map(|(_, value)| value.trim().to_string());
        } else if let Some(value) = trimmed.strip_prefix("driverName")
            && let Some((_, value)) = value.split_once('=')
        {
            current_driver = Some(value.trim().to_string());
        } else if let Some(value) = trimmed.strip_prefix("apiVersion")
            && let Some((_, value)) = value.split_once('=')
        {
            current_api = Some(value.trim().to_string());
        } else if let Some(value) = trimmed.strip_prefix("deviceName")
            && let Some((_, value)) = value.split_once('=')
            && current_name.is_none()
        {
            current_name = Some(value.trim().to_string());
        }
    }

    if current_name.is_some() {
        devices.push(VulkanDevice {
            name: current_name.unwrap_or_default(),
            driver: current_driver,
            api_version: current_api,
            extensions: extension_set,
        });
    }

    devices
}

fn parse_wayland_interfaces(section: &str) -> Vec<String> {
    let mut interfaces = Vec::new();
    for line in section.lines() {
        let trimmed = line.trim();
        for iface in GPU_REQUIRED_WAYLAND_INTERFACES {
            if trimmed.contains(iface) && !interfaces.iter().any(|existing| existing == iface) {
                interfaces.push((*iface).to_string());
            }
        }
    }
    interfaces
}

fn classify_gpu_report(
    dri_devices: &[String],
    module_loaded: bool,
    vulkan_devices: &[VulkanDevice],
    wayland_interfaces: &[String],
    vulkan_summary: &str,
    probe_section: &str,
    notes: &mut Vec<String>,
) -> (ProbeResult, Option<GpuFailureKind>) {
    let has_required_dri = GPU_REQUIRED_DRI_DEVICES
        .iter()
        .all(|required| dri_devices.iter().any(|found| found == required));
    if !has_required_dri {
        return (ProbeResult::Fail, Some(GpuFailureKind::NoDevice));
    }

    if !module_loaded {
        notes.push("virtio_gpu kernel module is not visible under /sys/module".to_string());
    }

    if vulkan_devices.is_empty() {
        let lower = vulkan_summary.to_lowercase();
        if lower.contains("icd") || lower.contains("loader") {
            return (ProbeResult::Fail, Some(GpuFailureKind::NoLoaderIcd));
        }
        return (ProbeResult::Fail, Some(GpuFailureKind::NoDevice));
    }

    let has_swapchain = vulkan_devices.iter().all(|device| {
        device
            .extensions
            .iter()
            .any(|ext| ext == "VK_KHR_swapchain")
    });
    if !has_swapchain {
        return (ProbeResult::Fail, Some(GpuFailureKind::NoSwapchainExt));
    }

    let has_dmabuf = vulkan_devices.iter().all(|device| {
        device
            .extensions
            .iter()
            .any(|ext| ext == "VK_KHR_external_memory_fd")
            && device
                .extensions
                .iter()
                .any(|ext| ext == "VK_EXT_external_memory_dma_buf")
    });
    if !has_dmabuf {
        return (ProbeResult::Fail, Some(GpuFailureKind::NoDmabufExt));
    }

    let has_wayland = GPU_REQUIRED_WAYLAND_INTERFACES
        .iter()
        .all(|required| wayland_interfaces.iter().any(|found| found == required));
    if !has_wayland {
        return (ProbeResult::Fail, Some(GpuFailureKind::WaylandMissing));
    }

    let probe_line = probe_section.lines().next().unwrap_or("").trim();
    if probe_line == "PASS" {
        return (ProbeResult::Pass, None);
    }
    if probe_line.starts_with("FAIL:") {
        notes.push(probe_section.trim().to_string());
        return (ProbeResult::Fail, Some(GpuFailureKind::ProbeCrash));
    }

    (ProbeResult::Fail, Some(GpuFailureKind::Unknown))
}

fn print_gpu_preflight_table(reports: &[GpuPreflightReport]) {
    println!(
        "{:<8} {:<6} {:<7} {:<20} {:<9} {:<18}",
        "VM", "DRI", "Module", "Vulkan", "Wayland", "Probe"
    );
    println!("{}", "─".repeat(78));
    for report in reports {
        println!(
            "{:<8} {:<6} {:<7} {:<20} {:<9} {:<18}",
            report.vm,
            GpuPreflightReport::status_cell(report.dri_ok()),
            GpuPreflightReport::status_cell(report.module_loaded),
            report.vulkan_label(),
            GpuPreflightReport::status_cell(report.wayland_ok()),
            report.probe_cell(),
        );
        if let Some(kind) = &report.failure_kind {
            println!(
                "  {}: {}",
                kind.hint(),
                report
                    .notes
                    .first()
                    .map(String::as_str)
                    .unwrap_or(report.probe_result.as_str())
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorLevel {
    Ok,
    Warn,
    Fixed,
    Error,
}

struct DoctorItem {
    level: DoctorLevel,
    label: String,
    detail: String,
}

impl DoctorItem {
    fn print(&self) {
        let tag = match self.level {
            DoctorLevel::Ok => "OK   ",
            DoctorLevel::Warn => "WARN ",
            DoctorLevel::Fixed => "FIXED",
            DoctorLevel::Error => "ERROR",
        };
        println!("[{tag}] {}: {}", self.label, self.detail);
    }
}

pub async fn doctor<S>(config: &ClusterConfig<S>, fix: bool) -> anyhow::Result<()> {
    let mut items = Vec::new();

    if config.state_dir.exists() {
        items.push(DoctorItem {
            level: DoctorLevel::Ok,
            label: "state directory".to_string(),
            detail: config.state_dir.display().to_string(),
        });
    } else if fix {
        state::ensure_state_dir(&config.state_dir)?;
        items.push(DoctorItem {
            level: DoctorLevel::Fixed,
            label: "state directory".to_string(),
            detail: format!("created {}", config.state_dir.display()),
        });
    } else {
        items.push(DoctorItem {
            level: DoctorLevel::Warn,
            label: "state directory".to_string(),
            detail: format!("missing {}", config.state_dir.display()),
        });
    }

    for vm in &config.vms {
        match state::read_pid(&config.state_dir, &vm.name) {
            Some(pid) if state::is_pid_alive(pid) => items.push(DoctorItem {
                level: DoctorLevel::Ok,
                label: format!("pid:{}", vm.name),
                detail: format!("live PID {pid}"),
            }),
            Some(pid) if fix => {
                state::remove_pid(&config.state_dir, &vm.name);
                items.push(DoctorItem {
                    level: DoctorLevel::Fixed,
                    label: format!("pid:{}", vm.name),
                    detail: format!("removed stale PID file (was {pid})"),
                });
            }
            Some(pid) => items.push(DoctorItem {
                level: DoctorLevel::Warn,
                label: format!("pid:{}", vm.name),
                detail: format!("stale PID file (was {pid})"),
            }),
            None => items.push(DoctorItem {
                level: DoctorLevel::Ok,
                label: format!("pid:{}", vm.name),
                detail: "no PID file".to_string(),
            }),
        }

        let pattern = format!("microvm@{}", vm.name);
        let orphan_pids = std::process::Command::new("pgrep")
            .args(["-f", &pattern])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|pids| !pids.is_empty())
            .filter(|_| lease::probe_holder(vm.index, &config.lock_dir).is_none());
        if let Some(pids) = orphan_pids {
            if fix {
                let _ = std::process::Command::new("pkill")
                    .args(["-f", &pattern])
                    .status();
                items.push(DoctorItem {
                    level: DoctorLevel::Fixed,
                    label: format!("orphan:{}", vm.name),
                    detail: format!("killed orphan PIDs {pids}"),
                });
            } else {
                items.push(DoctorItem {
                    level: DoctorLevel::Warn,
                    label: format!("orphan:{}", vm.name),
                    detail: format!("orphan PIDs {pids}"),
                });
            }
        } else {
            items.push(DoctorItem {
                level: DoctorLevel::Ok,
                label: format!("orphan:{}", vm.name),
                detail: "no orphans".to_string(),
            });
        }

        let vm_dir = config.state_dir.join(&vm.name);
        let stale_sockets: Vec<String> = std::fs::read_dir(&vm_dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| state::is_socket_file(name))
            .collect();
        if stale_sockets.is_empty() {
            items.push(DoctorItem {
                level: DoctorLevel::Ok,
                label: format!("sockets:{}", vm.name),
                detail: "none".to_string(),
            });
        } else if fix {
            for socket in &stale_sockets {
                let _ = std::fs::remove_file(vm_dir.join(socket));
            }
            items.push(DoctorItem {
                level: DoctorLevel::Fixed,
                label: format!("sockets:{}", vm.name),
                detail: format!("removed {} stale socket file(s)", stale_sockets.len()),
            });
        } else {
            items.push(DoctorItem {
                level: DoctorLevel::Warn,
                label: format!("sockets:{}", vm.name),
                detail: format!("{} stale socket file(s)", stale_sockets.len()),
            });
        }

        let tap = format!("tap-{}", vm.name);
        let tap_exists = std::process::Command::new("ip")
            .args(["link", "show", &tap])
            .status()
            .is_ok_and(|status| status.success());
        items.push(DoctorItem {
            level: if tap_exists {
                DoctorLevel::Ok
            } else {
                DoctorLevel::Warn
            },
            label: format!("tap:{}", vm.name),
            detail: if tap_exists {
                format!("{tap} exists")
            } else {
                format!("{tap} missing")
            },
        });
    }

    let bridge_exists = crate::core::config::bridge_exists(&config.bridge);
    items.push(DoctorItem {
        level: if bridge_exists {
            DoctorLevel::Ok
        } else {
            DoctorLevel::Warn
        },
        label: "bridge".to_string(),
        detail: if bridge_exists {
            format!("{} exists", config.bridge)
        } else {
            format!("{} missing", config.bridge)
        },
    });

    items.push(DoctorItem {
        level: if config.ssh_key.exists() {
            DoctorLevel::Ok
        } else {
            DoctorLevel::Error
        },
        label: "ssh key".to_string(),
        detail: config.ssh_key.display().to_string(),
    });

    let nft_cluster_exists = std::process::Command::new("nft")
        .args(["list", "table", "ip", "cluster"])
        .status()
        .is_ok_and(|status| status.success());
    items.push(DoctorItem {
        level: if nft_cluster_exists {
            DoctorLevel::Ok
        } else {
            DoctorLevel::Warn
        },
        label: "nftables".to_string(),
        detail: if nft_cluster_exists {
            "cluster table exists".to_string()
        } else {
            "no cluster table (NAT may not be configured)".to_string()
        },
    });

    if matches!(config.backend_kind, BackendKind::Microvm) {
        let reports = gpu_preflight_running(config, &config.vms).await?;
        for report in reports {
            let level = match report.probe_result {
                ProbeResult::Pass => DoctorLevel::Ok,
                ProbeResult::Skipped => DoctorLevel::Warn,
                ProbeResult::Fail => DoctorLevel::Warn,
            };
            let detail = match &report.failure_kind {
                Some(kind) => format!("{} ({})", report.probe_cell(), kind.hint()),
                None => report.probe_cell(),
            };
            items.push(DoctorItem {
                level,
                label: format!("graphics:{}", report.vm),
                detail,
            });
        }
    }

    for item in &items {
        item.print();
    }

    let warnings = items
        .iter()
        .filter(|item| matches!(item.level, DoctorLevel::Warn))
        .count();
    let fixed = items
        .iter()
        .filter(|item| matches!(item.level, DoctorLevel::Fixed))
        .count();
    let errors = items
        .iter()
        .filter(|item| matches!(item.level, DoctorLevel::Error))
        .count();

    println!();
    println!("==> {warnings} warning(s), {fixed} fixed, {errors} error(s)");

    if errors > 0 {
        anyhow::bail!("Doctor found {errors} error(s)");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_check_script_checks_dev_dri() {
        let script = gpu_check_script();
        assert!(script.contains("/dev/dri/card*"));
    }

    #[test]
    fn gpu_check_script_prints_gpu_ok_on_success() {
        let script = gpu_check_script();
        assert!(script.contains("gpu-ok"));
    }

    #[test]
    fn gpu_check_script_prints_gpu_missing_on_failure() {
        let script = gpu_check_script();
        assert!(script.contains("gpu-missing"));
    }

    #[test]
    fn gpu_check_script_exits_nonzero_on_failure() {
        let script = gpu_check_script();
        assert!(script.contains("exit 1"));
    }

    #[test]
    fn gpu_check_script_prints_diagnostics_on_failure() {
        let script = gpu_check_script();
        assert!(script.contains("/dev/dri/"));
    }

    #[test]
    fn parse_gpu_check_success() {
        assert_eq!(
            parse_gpu_check("gpu-ok /dev/dri/card0\n"),
            Some("/dev/dri/card0"),
        );
    }

    #[test]
    fn parse_gpu_check_success_with_whitespace() {
        assert_eq!(
            parse_gpu_check("  gpu-ok /dev/dri/card1  \n"),
            Some("/dev/dri/card1"),
        );
    }

    #[test]
    fn parse_gpu_check_failure() {
        assert_eq!(parse_gpu_check("gpu-missing\n"), None);
    }

    #[test]
    fn parse_gpu_check_empty() {
        assert_eq!(parse_gpu_check(""), None);
    }

    #[test]
    fn parse_gpu_check_random_output() {
        assert_eq!(parse_gpu_check("something unexpected"), None);
    }

    #[test]
    fn requires_gpu_true_for_gpu_modes() {
        use crate::ui::cli::DisplayMode;
        assert!(DisplayMode::WestonGpu.requires_gpu());
        assert!(DisplayMode::SwayGpu.requires_gpu());
    }

    #[test]
    fn requires_gpu_false_for_non_gpu_modes() {
        use crate::ui::cli::DisplayMode;
        assert!(!DisplayMode::Headless.requires_gpu());
        assert!(!DisplayMode::Weston.requires_gpu());
        assert!(!DisplayMode::Sway.requires_gpu());
    }

    mod gpu {
        use super::*;

        fn fixture_all_pass() -> &'static str {
            r#"===DRI===
/dev/dri/card0
/dev/dri/renderD128
===MODULE===
PRESENT
===VULKAN_SUMMARY===
GPU0:
deviceName     = virtio_gpu
driverName     = mesa
apiVersion     = 1.3.250
===VULKAN_EXTENSIONS===
VK_KHR_swapchain
VK_KHR_external_memory_fd
VK_EXT_external_memory_dma_buf
===WAYLAND===
interface: 'wl_compositor', version: 6, name: 1
interface: 'zwp_linux_dmabuf_v1', version: 4, name: 9
interface: 'zxdg_output_manager_v1', version: 3, name: 10
===PROBE===
PASS
"#
        }

        fn fixture_missing_dri() -> &'static str {
            r#"===DRI===
/dev/dri/renderD128
===MODULE===
PRESENT
===VULKAN_SUMMARY===
===VULKAN_EXTENSIONS===
===WAYLAND===
===PROBE===
FAIL:1
"#
        }

        fn fixture_missing_icd() -> &'static str {
            r#"===DRI===
/dev/dri/card0
/dev/dri/renderD128
===MODULE===
PRESENT
===VULKAN_SUMMARY===
ERROR: [Loader Message] Code 0 : vkCreateInstance: Found no drivers!
Cannot create Vulkan instance.
===VULKAN_EXTENSIONS===
===WAYLAND===
interface: 'wl_compositor', version: 6, name: 1
===PROBE===
FAIL:1
loader icd failure
"#
        }

        fn fixture_missing_wayland() -> &'static str {
            r#"===DRI===
/dev/dri/card0
/dev/dri/renderD128
===MODULE===
PRESENT
===VULKAN_SUMMARY===
GPU0:
deviceName     = virtio_gpu
===VULKAN_EXTENSIONS===
VK_KHR_swapchain
VK_KHR_external_memory_fd
VK_EXT_external_memory_dma_buf
===WAYLAND===
failed to connect to display
===PROBE===
FAIL:1
"#
        }

        fn fixture_probe_crash() -> &'static str {
            r#"===DRI===
/dev/dri/card0
/dev/dri/renderD128
===MODULE===
PRESENT
===VULKAN_SUMMARY===
GPU0:
deviceName     = virtio_gpu
===VULKAN_EXTENSIONS===
VK_KHR_swapchain
VK_KHR_external_memory_fd
VK_EXT_external_memory_dma_buf
===WAYLAND===
interface: 'wl_compositor', version: 6, name: 1
interface: 'zwp_linux_dmabuf_v1', version: 4, name: 9
interface: 'zxdg_output_manager_v1', version: 3, name: 10
===PROBE===
FAIL:124
timeout
"#
        }

        #[test]
        fn parser_classifies_all_pass_fixture() {
            let report = parse_gpu_preflight_output("vm-1", fixture_all_pass());
            assert_eq!(report.probe_result, ProbeResult::Pass);
            assert_eq!(report.failure_kind, None);
            assert_eq!(report.vulkan_devices[0].name, "virtio_gpu");
        }

        #[test]
        fn parser_classifies_missing_dri_fixture() {
            let report = parse_gpu_preflight_output("vm-1", fixture_missing_dri());
            assert_eq!(report.probe_result, ProbeResult::Fail);
            assert_eq!(report.failure_kind, Some(GpuFailureKind::NoDevice));
        }

        #[test]
        fn parser_classifies_missing_icd_fixture() {
            let report = parse_gpu_preflight_output("vm-1", fixture_missing_icd());
            assert_eq!(report.probe_result, ProbeResult::Fail);
            assert_eq!(report.failure_kind, Some(GpuFailureKind::NoLoaderIcd));
        }

        #[test]
        fn parser_classifies_missing_wayland_fixture() {
            let report = parse_gpu_preflight_output("vm-1", fixture_missing_wayland());
            assert_eq!(report.probe_result, ProbeResult::Fail);
            assert_eq!(report.failure_kind, Some(GpuFailureKind::WaylandMissing));
        }

        #[test]
        fn parser_classifies_probe_crash_fixture() {
            let report = parse_gpu_preflight_output("vm-1", fixture_probe_crash());
            assert_eq!(report.probe_result, ProbeResult::Fail);
            assert_eq!(report.failure_kind, Some(GpuFailureKind::ProbeCrash));
        }
    }
}
