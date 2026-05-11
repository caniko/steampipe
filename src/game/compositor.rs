use std::fmt;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::core::backend::{Backend, CmdOutput};
use crate::core::config::{ClusterConfig, VmDef, WindowTarget, par_each_vm};

const SWAY_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const COMPOSITOR_READY_TIMEOUT: Duration = Duration::from_secs(30);
const READY_DELAY_INITIAL: Duration = Duration::from_millis(250);
const READY_DELAY_MAX: Duration = Duration::from_secs(4);
const OUTPUT_STABILITY_GAP_SECS: &str = "0.25";
const PROBE_CAPTURE_BYTES: usize = 256;
const PROBE_PIXEL_BYTES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompositorReadiness {
    NoSocket { details: String },
    NoOutputs { details: String },
    NoCommit { details: String },
    WindowMissing { target: String },
    WindowUnmapped { target: String },
    Ready,
}

impl fmt::Display for CompositorReadiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSocket { details } => write!(f, "NoSocket ({details})"),
            Self::NoOutputs { details } => write!(f, "NoOutputs ({details})"),
            Self::NoCommit { details } => write!(f, "NoCommit ({details})"),
            Self::WindowMissing { target } => write!(f, "WindowMissing ({target})"),
            Self::WindowUnmapped { target } => write!(f, "WindowUnmapped ({target})"),
            Self::Ready => f.write_str("Ready"),
        }
    }
}

impl CompositorReadiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositorStatusRow {
    pub name: String,
    pub ip: String,
    pub readiness: Option<CompositorReadiness>,
    pub error: Option<String>,
}

pub async fn wait_until_ready(
    backend: &Backend,
    vm: &VmDef,
    target: Option<&WindowTarget>,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut delay = READY_DELAY_INITIAL;

    loop {
        let readiness = check_readiness(backend, vm, target).await?;
        if readiness.is_ready() {
            return Ok(());
        }

        let now = Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "{}: compositor not ready after {}s: {}",
                vm.name,
                timeout.as_secs(),
                readiness
            );
        }

        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(delay.min(remaining)).await;
        delay = (delay * 2).min(READY_DELAY_MAX);
    }
}

pub async fn check_readiness(
    backend: &Backend,
    vm: &VmDef,
    target: Option<&WindowTarget>,
) -> anyhow::Result<CompositorReadiness> {
    let outputs = run_sway_output_samples(backend, vm).await?;
    if !outputs.success {
        if looks_like_missing_socket(&outputs) {
            return Ok(CompositorReadiness::NoSocket {
                details: describe_remote_error(&outputs),
            });
        }
        anyhow::bail!(
            "{}: sway output query failed: {}",
            vm.name,
            describe_remote_error(&outputs)
        );
    }
    let (first, second) = parse_output_samples(&outputs.stdout)
        .with_context(|| format!("{}: parse sway output samples", vm.name))?;
    let first_active = active_outputs(&first);
    if first_active.is_empty() {
        return Ok(CompositorReadiness::NoOutputs {
            details: "sway reported zero active outputs".to_string(),
        });
    }

    let second_active = active_outputs(&second);
    if second_active.is_empty() {
        return Ok(CompositorReadiness::NoOutputs {
            details: "sway outputs went inactive during readiness sampling".to_string(),
        });
    }

    if !has_stable_output(&first_active, &second_active) {
        return Ok(CompositorReadiness::NoOutputs {
            details: "active outputs were not stable across two sway samples".to_string(),
        });
    }

    let probe = run_commit_probe(backend, vm).await?;
    if !probe.success {
        if looks_like_missing_socket(&probe) {
            return Ok(CompositorReadiness::NoSocket {
                details: describe_remote_error(&probe),
            });
        }
        return Ok(CompositorReadiness::NoCommit {
            details: describe_remote_error(&probe),
        });
    }
    if !probe.stdout.trim().is_empty() {
        let ppm_prefix = parse_od_hex_bytes(&probe.stdout)
            .with_context(|| format!("{}: parse grim probe bytes", vm.name))?;
        if !ppm_prefix_has_nonzero_pixels(&ppm_prefix)? {
            return Ok(CompositorReadiness::NoCommit {
                details: format!(
                    "grim probe captured no non-zero pixel data in the first {PROBE_PIXEL_BYTES} bytes"
                ),
            });
        }
    } else {
        return Ok(CompositorReadiness::NoCommit {
            details: "grim probe returned no data".to_string(),
        });
    }

    if let Some(target) = target {
        let tree = run_tree_query(backend, vm).await?;
        if !tree.success {
            if looks_like_missing_socket(&tree) {
                return Ok(CompositorReadiness::NoSocket {
                    details: describe_remote_error(&tree),
                });
            }
            anyhow::bail!(
                "{}: sway tree query failed: {}",
                vm.name,
                describe_remote_error(&tree)
            );
        }
        let root: SwayNode = serde_json::from_str(&tree.stdout)
            .with_context(|| format!("{}: parse sway tree", vm.name))?;
        return Ok(classify_window_target(&root, target));
    }

    Ok(CompositorReadiness::Ready)
}

pub async fn status_all<S>(config: &ClusterConfig<S>) -> anyhow::Result<()> {
    let rows = status_rows(config).await?;
    print_status_rows(&rows);
    Ok(())
}

pub async fn status_rows<S>(config: &ClusterConfig<S>) -> anyhow::Result<Vec<CompositorStatusRow>> {
    let backend = config.backend.clone();
    let mut rows = par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        async move {
            let readiness = check_readiness(&backend, &vm, None).await;
            match readiness {
                Ok(readiness) => CompositorStatusRow {
                    name: vm.name.to_string(),
                    ip: vm.ip.to_string(),
                    readiness: Some(readiness),
                    error: None,
                },
                Err(error) => CompositorStatusRow {
                    name: vm.name.to_string(),
                    ip: vm.ip.to_string(),
                    readiness: None,
                    error: Some(error.to_string()),
                },
            }
        }
    })
    .await?;
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

pub fn print_status_rows(rows: &[CompositorStatusRow]) {
    println!("{:<8} {:<14} {:<18} Detail", "VM", "IP", "Readiness");
    println!("{}", "─".repeat(92));
    for row in rows {
        let (state, detail) = match (&row.readiness, &row.error) {
            (Some(readiness), None) => {
                let detail = match readiness {
                    CompositorReadiness::NoSocket { details }
                    | CompositorReadiness::NoOutputs { details }
                    | CompositorReadiness::NoCommit { details } => details.clone(),
                    CompositorReadiness::WindowMissing { target }
                    | CompositorReadiness::WindowUnmapped { target } => target.clone(),
                    CompositorReadiness::Ready => "compositor ready".to_string(),
                };
                (readiness.to_string(), detail)
            }
            (None, Some(error)) => ("ERROR".to_string(), error.clone()),
            _ => (
                "ERROR".to_string(),
                "invalid compositor status state".to_string(),
            ),
        };
        println!("{:<8} {:<14} {:<18} {}", row.name, row.ip, state, detail);
    }
}

pub fn default_ready_timeout() -> Duration {
    COMPOSITOR_READY_TIMEOUT
}

fn classify_window_target(root: &SwayNode, target: &WindowTarget) -> CompositorReadiness {
    let target_label = format_window_target(target);
    let mut matched_any = false;

    for node in root.walk() {
        if !node.matches(target) {
            continue;
        }
        matched_any = true;
        if node.visible && node.rect.width > 0 && node.rect.height > 0 {
            return CompositorReadiness::Ready;
        }
    }

    if matched_any {
        CompositorReadiness::WindowUnmapped {
            target: target_label,
        }
    } else {
        CompositorReadiness::WindowMissing {
            target: target_label,
        }
    }
}

fn format_window_target(target: &WindowTarget) -> String {
    target
        .app_id
        .as_ref()
        .map(|app_id| format!("app_id={app_id}"))
        .or_else(|| target.class.as_ref().map(|class| format!("class={class}")))
        .or_else(|| target.title.as_ref().map(|title| format!("title={title}")))
        .unwrap_or_else(|| "unspecified target".to_string())
}

fn active_outputs(outputs: &[SwayOutput]) -> Vec<&SwayOutput> {
    outputs.iter().filter(|output| output.active).collect()
}

fn has_stable_output(first: &[&SwayOutput], second: &[&SwayOutput]) -> bool {
    first.iter().any(|left| {
        second
            .iter()
            .any(|right| left.name == right.name && left.fingerprint() == right.fingerprint())
    })
}

fn parse_output_samples(stdout: &str) -> anyhow::Result<(Vec<SwayOutput>, Vec<SwayOutput>)> {
    let (first, second) = stdout
        .split_once("\n__STEAMPIPE_OUTPUTS_SECOND__\n")
        .ok_or_else(|| anyhow::anyhow!("missing sway output sample delimiter"))?;
    let first =
        serde_json::from_str(first.trim()).context("decode first sway output sample as JSON")?;
    let second =
        serde_json::from_str(second.trim()).context("decode second sway output sample as JSON")?;
    Ok((first, second))
}

fn parse_od_hex_bytes(stdout: &str) -> anyhow::Result<Vec<u8>> {
    stdout
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16).context("decode od hex byte"))
        .collect()
}

fn ppm_prefix_has_nonzero_pixels(bytes: &[u8]) -> anyhow::Result<bool> {
    let mut index = 0usize;
    let magic = next_ppm_token(bytes, &mut index).context("read PPM magic")?;
    if magic != b"P6" {
        anyhow::bail!(
            "grim probe returned unsupported PPM magic {:?}",
            String::from_utf8_lossy(magic)
        );
    }

    let _width = next_ppm_token(bytes, &mut index).context("read PPM width")?;
    let _height = next_ppm_token(bytes, &mut index).context("read PPM height")?;
    let _maxval = next_ppm_token(bytes, &mut index).context("read PPM maxval")?;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }

    let payload = bytes
        .get(index..)
        .ok_or_else(|| anyhow::anyhow!("missing PPM pixel payload"))?;
    if payload.is_empty() {
        return Ok(false);
    }

    Ok(payload
        .iter()
        .take(PROBE_PIXEL_BYTES)
        .any(|byte| *byte != 0))
}

fn next_ppm_token<'a>(bytes: &'a [u8], index: &mut usize) -> Option<&'a [u8]> {
    while *index < bytes.len() {
        if bytes[*index] == b'#' {
            while *index < bytes.len() && bytes[*index] != b'\n' {
                *index += 1;
            }
        } else if bytes[*index].is_ascii_whitespace() {
            *index += 1;
        } else {
            break;
        }
    }

    if *index >= bytes.len() {
        return None;
    }

    let start = *index;
    while *index < bytes.len() && !bytes[*index].is_ascii_whitespace() {
        *index += 1;
    }
    Some(&bytes[start..*index])
}

async fn run_sway_output_samples(backend: &Backend, vm: &VmDef) -> anyhow::Result<CmdOutput> {
    let cmd = format!(
        "set -eu\n\
         export XDG_RUNTIME_DIR=\"${{XDG_RUNTIME_DIR:-/tmp/runtime-$(whoami)}}\"\n\
         export WAYLAND_DISPLAY=\"${{WAYLAND_DISPLAY:-wayland-1}}\"\n\
         swaymsg -r -t get_outputs\n\
         printf '\\n__STEAMPIPE_OUTPUTS_SECOND__\\n'\n\
         sleep {OUTPUT_STABILITY_GAP_SECS}\n\
         swaymsg -r -t get_outputs\n"
    );
    let output = backend
        .run_cmd_timeout(&vm.ip, &cmd, SWAY_QUERY_TIMEOUT)
        .await;
    Ok(output)
}

async fn run_tree_query(backend: &Backend, vm: &VmDef) -> anyhow::Result<CmdOutput> {
    let cmd = "set -eu\n\
         export XDG_RUNTIME_DIR=\"${XDG_RUNTIME_DIR:-/tmp/runtime-$(whoami)}\"\n\
         export WAYLAND_DISPLAY=\"${WAYLAND_DISPLAY:-wayland-1}\"\n\
         swaymsg -r -t get_tree\n";
    let output = backend
        .run_cmd_timeout(&vm.ip, cmd, SWAY_QUERY_TIMEOUT)
        .await;
    Ok(output)
}

async fn run_commit_probe(backend: &Backend, vm: &VmDef) -> anyhow::Result<CmdOutput> {
    let cmd = format!(
        "set -eu\n\
         export XDG_RUNTIME_DIR=\"${{XDG_RUNTIME_DIR:-/tmp/runtime-$(whoami)}}\"\n\
         export WAYLAND_DISPLAY=\"${{WAYLAND_DISPLAY:-wayland-1}}\"\n\
         probe_path=\"/tmp/steampipe-compositor-probe-$$.ppm\"\n\
         rm -f \"$probe_path\"\n\
         grim -t ppm \"$probe_path\"\n\
         head -c {PROBE_CAPTURE_BYTES} \"$probe_path\" | od -An -t x1 -v\n\
         rm -f \"$probe_path\"\n"
    );
    let output = backend
        .run_cmd_timeout(&vm.ip, &cmd, SWAY_QUERY_TIMEOUT)
        .await;
    Ok(output)
}

fn looks_like_missing_socket(output: &CmdOutput) -> bool {
    let combined = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    combined.contains("swaysock")
        || combined.contains("unable to retrieve socket path")
        || combined.contains("failed to connect to sway ipc")
        || combined.contains("connection refused")
}

fn describe_remote_error(output: &CmdOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    "remote command returned no diagnostics".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct SwayMode {
    width: i32,
    height: i32,
    refresh: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct SwayOutput {
    name: String,
    active: bool,
    #[serde(default)]
    make: String,
    #[serde(default)]
    model: String,
    current_mode: Option<SwayMode>,
}

impl SwayOutput {
    fn fingerprint(&self) -> String {
        let mode = self
            .current_mode
            .as_ref()
            .map(|mode| format!("{}x{}@{}", mode.width, mode.height, mode.refresh))
            .unwrap_or_else(|| "no-mode".to_string());
        format!("{}:{}:{mode}", self.make, self.model)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct WindowProperties {
    class: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct Rect {
    width: i32,
    height: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct SwayNode {
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    visible: bool,
    #[serde(default)]
    rect: Rect,
    #[serde(default)]
    window_properties: Option<WindowProperties>,
    #[serde(default)]
    nodes: Vec<SwayNode>,
    #[serde(default)]
    floating_nodes: Vec<SwayNode>,
}

impl SwayNode {
    fn walk(&self) -> SwayNodeWalk<'_> {
        SwayNodeWalk { stack: vec![self] }
    }

    fn matches(&self, target: &WindowTarget) -> bool {
        target
            .app_id
            .as_ref()
            .is_some_and(|app_id| self.app_id.as_deref() == Some(app_id.as_str()))
            || target.class.as_ref().is_some_and(|class| {
                self.window_properties
                    .as_ref()
                    .and_then(|props| props.class.as_deref())
                    == Some(class.as_str())
            })
            || target
                .title
                .as_ref()
                .is_some_and(|title| self.name.as_deref() == Some(title.as_str()))
    }
}

struct SwayNodeWalk<'a> {
    stack: Vec<&'a SwayNode>,
}

impl<'a> Iterator for SwayNodeWalk<'a> {
    type Item = &'a SwayNode;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        self.stack.extend(node.floating_nodes.iter().rev());
        self.stack.extend(node.nodes.iter().rev());
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_target_app_id(app_id: &str) -> WindowTarget {
        WindowTarget {
            app_id: Some(app_id.to_string()),
            title: None,
            class: None,
        }
    }

    fn window_target_class(class: &str) -> WindowTarget {
        WindowTarget {
            app_id: None,
            title: None,
            class: Some(class.to_string()),
        }
    }

    #[test]
    fn no_socket_is_detected_from_swaymsg_errors() {
        let output = CmdOutput {
            stdout: String::new(),
            stderr: "Unable to retrieve socket path".to_string(),
            success: false,
        };
        assert!(looks_like_missing_socket(&output));
    }

    #[test]
    fn output_samples_require_an_active_output() {
        let first: Vec<SwayOutput> = serde_json::from_str(
            r#"[{"name":"HEADLESS-1","active":false,"make":"virtio","model":"gpu","current_mode":{"width":1280,"height":720,"refresh":60000}}]"#,
        )
        .unwrap();
        assert!(active_outputs(&first).is_empty());
    }

    #[test]
    fn output_stability_requires_matching_fingerprint_across_samples() {
        let first: Vec<SwayOutput> = serde_json::from_str(
            r#"[{"name":"HEADLESS-1","active":true,"make":"virtio","model":"gpu","current_mode":{"width":1280,"height":720,"refresh":60000}}]"#,
        )
        .unwrap();
        let second: Vec<SwayOutput> = serde_json::from_str(
            r#"[{"name":"HEADLESS-1","active":true,"make":"virtio","model":"gpu","current_mode":{"width":1920,"height":1080,"refresh":60000}}]"#,
        )
        .unwrap();

        assert!(!has_stable_output(
            &active_outputs(&first),
            &active_outputs(&second)
        ));
    }

    #[test]
    fn ppm_probe_detects_nonzero_pixels_after_header() {
        let bytes = b"P6\n2 2\n255\n\x00\x00\x00\x00\x00\x00\x12\x34\x56\x00\x00\x00";
        assert!(ppm_prefix_has_nonzero_pixels(bytes).unwrap());
    }

    #[test]
    fn missing_window_is_reported() {
        let root: SwayNode = serde_json::from_str(
            r#"{
                "app_id": null,
                "name": "root",
                "visible": false,
                "rect": {"width": 0, "height": 0},
                "nodes": [{
                    "app_id": "other-app",
                    "name": "Other",
                    "visible": true,
                    "rect": {"width": 800, "height": 600},
                    "nodes": [],
                    "floating_nodes": []
                }],
                "floating_nodes": []
            }"#,
        )
        .unwrap();

        assert_eq!(
            classify_window_target(&root, &window_target_app_id("regicide")),
            CompositorReadiness::WindowMissing {
                target: "app_id=regicide".to_string()
            }
        );
    }

    #[test]
    fn hidden_window_is_reported_as_unmapped() {
        let root: SwayNode = serde_json::from_str(
            r#"{
                "app_id": null,
                "name": "root",
                "visible": false,
                "rect": {"width": 0, "height": 0},
                "nodes": [{
                    "app_id": null,
                    "name": "Steam",
                    "visible": false,
                    "rect": {"width": 800, "height": 600},
                    "window_properties": {"class": "Steam"},
                    "nodes": [],
                    "floating_nodes": []
                }],
                "floating_nodes": []
            }"#,
        )
        .unwrap();

        assert_eq!(
            classify_window_target(&root, &window_target_class("Steam")),
            CompositorReadiness::WindowUnmapped {
                target: "class=Steam".to_string()
            }
        );
    }

    #[test]
    fn visible_window_marks_readiness_ready() {
        let root: SwayNode = serde_json::from_str(
            r#"{
                "app_id": null,
                "name": "root",
                "visible": false,
                "rect": {"width": 0, "height": 0},
                "nodes": [{
                    "app_id": "regicide",
                    "name": "Regicide",
                    "visible": true,
                    "rect": {"width": 1280, "height": 720},
                    "nodes": [],
                    "floating_nodes": []
                }],
                "floating_nodes": []
            }"#,
        )
        .unwrap();

        assert_eq!(
            classify_window_target(&root, &window_target_app_id("regicide")),
            CompositorReadiness::Ready
        );
    }
}
