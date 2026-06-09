//! Cluster configuration: VM definitions, project config, and typestate bridge validation.
//!
//! The main type is [`ClusterConfig`] which holds all settings for a cluster session.
//! It uses a typestate pattern (`Unchecked` → `BridgeReady`) to enforce that the
//! network bridge is validated before starting VMs.

use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use crate::core::backend::Backend;
use clap::ValueEnum;

/// Which backend to use for running test instances.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// NixOS microVMs via microvm-run + SSH (default)
    #[default]
    #[serde(alias = "microvm")]
    Microvm,
    /// Docker containers via steamcmd base image
    Docker,
    /// Local processes on the host (no isolation)
    Local,
}

// ── Newtypes for domain-specific string primitives ──

/// A VM name (e.g. "vm-3"). Prevents accidental use as an IP or MAC.
///
/// Derefs to `str` for convenience. Implements `Display`, `Ord`, `AsRef<str>`, `AsRef<Path>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VmName(pub String);

/// An IPv4 address string (e.g. "10.0.100.3").
///
/// Derefs to `str` for convenience. Prevents mixing with VM names in function signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpAddr(pub String);

macro_rules! impl_str_newtype {
    ($ty:ident) => {
        impl Deref for $ty {
            type Target = str;
            fn deref(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
        impl AsRef<std::path::Path> for $ty {
            fn as_ref(&self) -> &std::path::Path {
                self.0.as_ref()
            }
        }
        impl std::borrow::Borrow<str> for $ty {
            fn borrow(&self) -> &str {
                &self.0
            }
        }
        impl PartialEq<str> for $ty {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }
        impl PartialEq<&str> for $ty {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    };
}

impl_str_newtype!(VmName);
impl_str_newtype!(IpAddr);

/// A single VM in the cluster.
#[derive(Debug, Clone)]
pub struct VmDef {
    pub name: VmName,
    pub ip: IpAddr,
    pub index: u8,
}

/// Run an async operation on each VM in parallel, collecting results.
pub async fn par_each_vm<F, Fut, T>(vms: &[VmDef], op: F) -> anyhow::Result<Vec<T>>
where
    F: Fn(VmDef) -> Fut,
    Fut: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut tasks = tokio::task::JoinSet::new();
    for vm in vms {
        tasks.spawn(op(vm.clone()));
    }
    let mut results = Vec::with_capacity(vms.len());
    while let Some(result) = tasks.join_next().await {
        results.push(result?);
    }
    Ok(results)
}

// ── Config file (steampipe.toml) ──

/// Project-level configuration file structure.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub backend: Option<BackendKind>,
    pub binary_name: Option<String>,
    pub cargo_package: Option<String>,
    pub vm_user: Option<String>,
    pub remote_dir: Option<String>,
    pub steam_api_lib: Option<String>,
    pub steam_appid_file: Option<String>,
    pub assets_dir: Option<String>,
    pub log_file: Option<String>,
    pub max_vms: Option<u8>,
    pub ssh_key: Option<String>,
    /// Directory for VM lock files (default: $XDG_RUNTIME_DIR/steampipe/)
    pub lock_dir: Option<String>,
    /// Minimum free RAM per VM in MB (default: 2048)
    pub ram_per_vm_mb: Option<u64>,
    /// Minimum RAM to keep free for the host in MB (default: 2048)
    pub host_ram_reserve_mb: Option<u64>,
    pub network: Option<NetworkFileConfig>,
    pub cluster: Option<ClusterSection>,
    pub docker: Option<DockerConfig>,
    pub local: Option<LocalConfig>,
    /// Named test profiles: `[profile.smoke]`, `[profile.stress]`, etc.
    pub profile: Option<HashMap<String, TestProfile>>,
    /// Named chaos profiles: `[chaos.unstable-wifi]`, etc.
    pub chaos: Option<HashMap<String, ChaosProfile>>,
    /// Structured visual golden-image testing configuration.
    pub visual: Option<VisualConfig>,
    /// Exit code → label mapping (keys are string integers, e.g. "10" = "PHASE_WATCHDOG").
    pub exit_codes: Option<HashMap<String, String>>,
    /// Heartbeat stall threshold in seconds (default: 30).
    pub heartbeat_stall_secs: Option<u64>,
    /// Notification hooks.
    pub hooks: Option<HooksConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DockerConfig {
    pub image: Option<String>,
    pub network: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LocalConfig {
    pub work_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct NetworkFileConfig {
    pub bridge: Option<String>,
    pub subnet: Option<String>,
    pub prefix: Option<u8>,
    pub host_ip: Option<String>,
    pub udp_port: Option<u16>,
    /// Owner of TAP devices (username). Needed for unprivileged VM hypervisors.
    pub tap_owner: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ClusterSection {
    pub name: Option<String>,
    /// Absolute path override for state directory (share cluster state across projects).
    pub state_dir: Option<String>,
}

/// A named test profile with all test parameters optional (CLI overrides profile).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct TestProfile {
    pub players: Option<u8>,
    pub max_runs: Option<u32>,
    pub timeout: Option<u64>,
    pub hard_timeout: Option<u64>,
    pub shutdown_timeout: Option<u64>,
    pub network: Option<String>,
    pub display: Option<String>,
    pub vm_args: Option<String>,
    pub host_args: Option<String>,
    pub no_build: Option<bool>,
    pub no_deploy: Option<bool>,
    pub no_stop_on_failure: Option<bool>,
    pub capture_on_failure: Option<bool>,
    pub screenshot_backend: Option<String>,
    pub visual_validator: Option<String>,
    pub validator_kind: Option<ValidatorKind>,
    pub filter_pattern: Option<String>,
    pub output_file: Option<String>,
    pub chaos_profile: Option<String>,
    pub heartbeat_stall_secs: Option<u64>,
    pub output_format: Option<String>,
    pub on_complete: Option<String>,
    pub on_failure: Option<String>,
    /// Extra environment variables exported to both peers (the host process
    /// spawned locally and the VM-side game launched via SSH). Inline-exported
    /// into the bash -c command so we don't depend on `AcceptEnv` on the
    /// remote sshd. Values undergo no shell escaping beyond what the consumer
    /// would already need — keep ids and tags free of spaces/quotes.
    pub env: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidatorKind {
    Shell,
    Golden,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VisualBackend {
    Grim,
    #[default]
    Vnc,
}

impl fmt::Display for VisualBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grim => f.write_str("grim"),
            Self::Vnc => f.write_str("vnc"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualDiffPaths {
    pub actual: PathBuf,
    pub diff: PathBuf,
    pub golden: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(default)]
pub struct VisualConfig {
    pub golden_dir: Option<PathBuf>,
    pub diff_dir: Option<PathBuf>,
    pub default_tolerance: Option<Tolerance>,
    pub default_backend: Option<VisualBackend>,
    pub scene: Vec<Scene>,
}

impl<'de> Deserialize<'de> for VisualConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct RawVisualConfig {
            golden_dir: Option<PathBuf>,
            diff_dir: Option<PathBuf>,
            default_tolerance: Option<Tolerance>,
            default_backend: Option<VisualBackend>,
            scene: Vec<Scene>,
        }

        let raw = RawVisualConfig::deserialize(deserializer)?;
        let config = Self {
            golden_dir: raw.golden_dir,
            diff_dir: raw.diff_dir,
            default_tolerance: raw.default_tolerance,
            default_backend: raw.default_backend,
            scene: raw.scene,
        };
        config.validate().map_err(serde::de::Error::custom)?;
        Ok(config)
    }
}

impl VisualConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.scene.is_empty() && self.golden_dir.is_none() {
            return Err("visual.golden_dir is required when visual.scene entries exist".into());
        }
        let mut names = std::collections::BTreeSet::new();
        for scene in &self.scene {
            validate_scene_name(&scene.name)?;
            if !names.insert(scene.name.as_str()) {
                return Err(format!("visual scene name '{}' is duplicated", scene.name));
            }
            scene.validate()?;
        }
        Ok(())
    }

    pub fn resolve_paths(&mut self, project_root: &Path) {
        if let Some(path) = &self.golden_dir {
            self.golden_dir = Some(resolve_project_path(project_root, path));
        }
        if let Some(path) = &self.diff_dir {
            self.diff_dir = Some(resolve_project_path(project_root, path));
        }
    }

    pub fn golden_path(&self, scene_name: &str) -> anyhow::Result<PathBuf> {
        validate_scene_name(scene_name).map_err(anyhow::Error::msg)?;
        let golden_dir = self
            .golden_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("visual.golden_dir is required"))?;
        Ok(golden_dir.join(format!("{scene_name}.png")))
    }

    pub fn diff_path(&self, scene_name: &str, run_label: &str) -> anyhow::Result<VisualDiffPaths> {
        validate_scene_name(scene_name).map_err(anyhow::Error::msg)?;
        validate_scene_name(run_label).map_err(anyhow::Error::msg)?;
        let diff_dir = self
            .diff_dir
            .as_ref()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("tests/visual/diff"));
        let dir = diff_dir.join(run_label);
        Ok(VisualDiffPaths {
            actual: dir.join(format!("{scene_name}.actual.png")),
            diff: dir.join(format!("{scene_name}.diff.png")),
            golden: dir.join(format!("{scene_name}.golden.png")),
        })
    }

    #[allow(dead_code)]
    pub fn scene(&self, name: &str) -> Option<&Scene> {
        self.scene.iter().find(|scene| scene.name == name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scene {
    #[serde(deserialize_with = "deserialize_scene_name")]
    pub name: String,
    pub description: Option<String>,
    pub precondition: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub window: Option<WindowTarget>,
    pub resolution: Option<Resolution>,
    pub backend: Option<VisualBackend>,
    pub tolerance: Option<Tolerance>,
    pub mask: Vec<MaskRect>,
    pub roi: Option<Roi>,
    pub wait_for: Option<WaitFor>,
    pub action: Vec<SceneAction>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowTarget {
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub class: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Resolution {
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tolerance {
    pub ssim: Option<f64>,
    pub max_pixel_delta: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaskRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Roi {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WaitFor {
    pub window_visible: Option<bool>,
    pub min_age_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SceneAction {
    pub step: String,
    pub keys: Option<String>,
    pub combo: Option<String>,
    pub focus: Option<String>,
    pub shell_command: Option<String>,
    pub delay_ms: Option<u64>,
    pub sync: Option<SceneSync>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SceneSync {
    pub window_visible: Option<String>,
    pub min_age_ms: Option<u64>,
    pub sway_node_named: Option<String>,
    pub file_exists: Option<String>,
    pub log_line_matches: Option<LogLineMatches>,
    pub fixed_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogLineMatches {
    pub path: String,
    pub regex: String,
}

impl Scene {
    #[allow(dead_code)]
    pub fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds.unwrap_or(30)
    }

    fn validate(&self) -> Result<(), String> {
        if let Some(name) = self.precondition.as_deref() {
            validate_scene_name(name)?;
        }
        if let Some(wait_for) = &self.wait_for
            && wait_for.window_visible == Some(true)
            && self.window.is_none()
        {
            return Err(format!(
                "visual scene '{}' sets wait_for.window_visible but has no window target",
                self.name
            ));
        }
        for action in &self.action {
            action.validate(&self.name)?;
        }
        Ok(())
    }
}

impl SceneAction {
    fn validate(&self, scene_name: &str) -> Result<(), String> {
        if self.step.trim().is_empty() {
            return Err(format!(
                "visual scene '{}' has an action with an empty step name",
                scene_name
            ));
        }
        let kinds = [
            self.keys.is_some(),
            self.combo.is_some(),
            self.focus.is_some(),
            self.shell_command.is_some(),
            self.delay_ms.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if kinds != 1 {
            return Err(format!(
                "visual scene '{}' action '{}' must define exactly one of keys, combo, focus, shell_command, or delay_ms",
                scene_name, self.step
            ));
        }
        let sync = self.sync.as_ref().ok_or_else(|| {
            format!(
                "visual scene '{}' action '{}' requires a sync predicate",
                scene_name, self.step
            )
        })?;
        sync.validate(scene_name, &self.step)
    }
}

impl SceneSync {
    fn validate(&self, scene_name: &str, step_name: &str) -> Result<(), String> {
        let kinds = [
            self.window_visible.is_some(),
            self.sway_node_named.is_some(),
            self.file_exists.is_some(),
            self.log_line_matches.is_some(),
            self.fixed_ms.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if kinds != 1 {
            return Err(format!(
                "visual scene '{}' action '{}' must define exactly one sync predicate",
                scene_name, step_name
            ));
        }
        if self.min_age_ms.is_some() && self.window_visible.is_none() {
            return Err(format!(
                "visual scene '{}' action '{}' sets min_age_ms without window_visible",
                scene_name, step_name
            ));
        }
        if let Some(log) = &self.log_line_matches
            && (log.path.trim().is_empty() || log.regex.trim().is_empty())
        {
            return Err(format!(
                "visual scene '{}' action '{}' log_line_matches requires both path and regex",
                scene_name, step_name
            ));
        }
        Ok(())
    }
}

fn resolve_project_path(project_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

fn deserialize_scene_name<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let name = String::deserialize(deserializer)?;
    validate_scene_name(&name).map_err(serde::de::Error::custom)?;
    Ok(name)
}

fn validate_scene_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("visual scene name cannot be empty".into());
    }
    if name.len() > 128 {
        return Err(format!("visual scene name '{name}' is too long"));
    }
    if name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && name != "."
        && name != ".."
        && !name.contains("..")
    {
        Ok(())
    } else {
        Err(format!(
            "visual scene name '{name}' must match [A-Za-z0-9._-]+ and must not contain '..'"
        ))
    }
}

impl ProjectConfig {
    pub fn effective_validator_kind(&self, profile: Option<&TestProfile>) -> ValidatorKind {
        if let Some(kind) = profile.and_then(|profile| profile.validator_kind) {
            return kind;
        }
        if self.visual.is_some() {
            return ValidatorKind::Golden;
        }
        if profile
            .and_then(|profile| profile.visual_validator.as_ref())
            .is_some()
        {
            return ValidatorKind::Shell;
        }
        ValidatorKind::None
    }

    pub fn warn_if_legacy_visual_validator_shadowed(&self, profile_name: Option<&str>) {
        let Some(profiles) = self.profile.as_ref() else {
            return;
        };
        if self.visual.is_none() {
            return;
        }
        let legacy_present = match profile_name {
            Some(name) => profiles
                .get(name)
                .and_then(|profile| profile.visual_validator.as_ref())
                .is_some(),
            None => profiles
                .values()
                .any(|profile| profile.visual_validator.as_ref().is_some()),
        };
        if legacy_present {
            eprintln!(
                "Warning: profile visual_validator is deprecated and ignored when [visual] is configured; validator_kind defaults to golden"
            );
        }
    }
}

#[cfg(test)]
pub mod visual {
    use super::*;

    const VISUAL_EXAMPLE: &str = r#"
        [visual]
        golden_dir = "tests/visual/golden"
        diff_dir = "tests/visual/diff"
        default_tolerance = { ssim = 0.985, max_pixel_delta = 8 }
        default_backend = "vnc"

        [[visual.scene]]
        name = "main-menu"
        description = "Steam launches, game shows main menu"
        window = { app_id = "regicide" }
        resolution = { width = 1280, height = 720 }
        backend = "vnc"
        tolerance = { ssim = 0.97, max_pixel_delta = 16 }
        mask = [ { x = 0, y = 0, w = 1280, h = 32 } ]
        roi = { x = 64, y = 64, w = 1152, h = 600 }
        wait_for = { window_visible = true, min_age_ms = 500 }
        precondition = "booted"
        timeout_seconds = 45

        [[visual.scene.action]]
        step = "press return"
        keys = "Return"
        sync = { window_visible = "regicide", min_age_ms = 500 }
    "#;

    fn parse_visual(input: &str) -> Result<ProjectConfig, toml::de::Error> {
        toml::from_str(input)
    }

    #[test]
    fn example_round_trips_structurally() {
        let parsed: ProjectConfig = parse_visual(VISUAL_EXAMPLE).unwrap();
        let encoded = toml::to_string(&parsed).unwrap();
        let reparsed: ProjectConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(parsed.visual, reparsed.visual);
    }

    #[test]
    fn parses_fully_populated_scene() {
        let parsed: ProjectConfig = parse_visual(VISUAL_EXAMPLE).unwrap();
        let visual = parsed.visual.unwrap();
        assert_eq!(
            visual.golden_dir.as_deref(),
            Some(Path::new("tests/visual/golden"))
        );
        assert_eq!(visual.scene.len(), 1);
        let scene = &visual.scene[0];
        assert_eq!(scene.name, "main-menu");
        assert_eq!(
            scene.window.as_ref().unwrap().app_id.as_deref(),
            Some("regicide")
        );
        assert_eq!(scene.resolution.as_ref().unwrap().width, Some(1280));
        assert_eq!(scene.mask[0].h, 32);
        assert_eq!(scene.wait_for.as_ref().unwrap().min_age_ms, Some(500));
        assert_eq!(scene.precondition.as_deref(), Some("booted"));
        assert_eq!(scene.timeout_seconds, Some(45));
        assert_eq!(scene.action.len(), 1);
        assert_eq!(scene.action[0].keys.as_deref(), Some("Return"));
    }

    #[test]
    fn golden_and_diff_paths_use_scene_name_contract() {
        let parsed: ProjectConfig = parse_visual(VISUAL_EXAMPLE).unwrap();
        let visual = parsed.visual.unwrap();
        assert_eq!(
            visual.golden_path("main-menu").unwrap(),
            PathBuf::from("tests/visual/golden/main-menu.png")
        );
        let diff = visual.diff_path("main-menu", "run-7").unwrap();
        assert_eq!(
            diff.actual,
            PathBuf::from("tests/visual/diff/run-7/main-menu.actual.png")
        );
        assert_eq!(
            diff.diff,
            PathBuf::from("tests/visual/diff/run-7/main-menu.diff.png")
        );
        assert_eq!(
            diff.golden,
            PathBuf::from("tests/visual/diff/run-7/main-menu.golden.png")
        );
    }

    #[test]
    fn resolve_paths_makes_visual_paths_project_relative() {
        let parsed: ProjectConfig = parse_visual(VISUAL_EXAMPLE).unwrap();
        let mut visual = parsed.visual.unwrap();
        visual.resolve_paths(Path::new("/project/root"));
        assert_eq!(
            visual.golden_dir.as_deref(),
            Some(Path::new("/project/root/tests/visual/golden"))
        );
        assert_eq!(
            visual.diff_dir.as_deref(),
            Some(Path::new("/project/root/tests/visual/diff"))
        );
    }

    #[test]
    fn unknown_validator_kind_is_rejected() {
        let err = parse_visual(
            r#"
            [profile.visual]
            validator_kind = "magic"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("validator_kind"));
    }

    #[test]
    fn invalid_mask_negative_width_is_rejected() {
        let err = parse_visual(
            r#"
            [visual]
            golden_dir = "tests/visual/golden"

            [[visual.scene]]
            name = "main-menu"
            mask = [ { x = 0, y = 0, w = -1, h = 32 } ]
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid value") || err.contains("w"));
    }

    #[test]
    fn scene_requires_golden_dir() {
        let err = parse_visual(
            r#"
            [visual]

            [[visual.scene]]
            name = "main-menu"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("golden_dir"));
    }

    #[test]
    fn duplicate_scene_names_are_rejected() {
        let err = parse_visual(
            r#"
            [visual]
            golden_dir = "tests/visual/golden"

            [[visual.scene]]
            name = "main-menu"

            [[visual.scene]]
            name = "main-menu"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicated"));
    }

    #[test]
    fn scene_action_requires_exactly_one_kind() {
        let err = parse_visual(
            r#"
            [visual]
            golden_dir = "tests/visual/golden"

            [[visual.scene]]
            name = "main-menu"

            [[visual.scene.action]]
            step = "bad"
            keys = "Return"
            combo = "Ctrl+P"
            sync = { fixed_ms = 10 }
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("exactly one of keys, combo, focus, shell_command, or delay_ms"));
    }

    #[test]
    fn scene_sync_requires_exactly_one_predicate() {
        let err = parse_visual(
            r#"
            [visual]
            golden_dir = "tests/visual/golden"

            [[visual.scene]]
            name = "main-menu"

            [[visual.scene.action]]
            step = "bad"
            delay_ms = 0
            sync = { fixed_ms = 10, file_exists = "/tmp/x" }
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("exactly one sync predicate"));
    }

    #[test]
    fn scene_name_rejects_path_traversal() {
        let err = parse_visual(
            r#"
            [visual]
            golden_dir = "tests/visual/golden"

            [[visual.scene]]
            name = "../main-menu"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("scene name"));
    }

    #[test]
    fn scene_name_rejects_shell_metacharacters() {
        let err = parse_visual(
            r#"
            [visual]
            golden_dir = "tests/visual/golden"

            [[visual.scene]]
            name = "main;rm"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("scene name"));
    }

    #[test]
    fn unknown_visual_fields_are_rejected() {
        let err = parse_visual(
            r#"
            [visual]
            golden_dir = "tests/visual/golden"
            tolarance = "typo"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn validator_kind_inference_prefers_visual_block() {
        let parsed: ProjectConfig = parse_visual(
            r#"
            [visual]
            golden_dir = "tests/visual/golden"

            [profile.legacy]
            visual_validator = "validator --image $STEAMPIPE_VISUAL_IMAGE"
        "#,
        )
        .unwrap();
        let profile = parsed.profile.as_ref().unwrap().get("legacy").unwrap();
        assert_eq!(
            parsed.effective_validator_kind(Some(profile)),
            ValidatorKind::Golden
        );
    }
}

/// Network emulation parameters for chaos testing.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ChaosProfile {
    pub latency: Option<u32>,
    pub jitter: Option<u32>,
    pub loss: Option<f32>,
    pub rate: Option<u32>,
}

/// Dynamic chaos injection schedule (stretch goal).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
#[allow(dead_code)]
pub struct ChaosSchedule {
    pub delay_secs: Option<u64>,
    pub profiles: Vec<String>,
    pub interval_secs: Option<u64>,
}

/// Notification hooks executed after test runs.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct HooksConfig {
    pub on_complete: Option<String>,
    pub on_failure: Option<String>,
}

/// Load project config from a steampipe.toml file.
pub fn load_project_config(project_root: &Path) -> Option<ProjectConfig> {
    let path = project_root.join("steampipe.toml");
    if !path.exists() {
        return None;
    }
    let contents = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&contents).ok()
}

/// Generate a default steampipe.toml template.
pub fn generate_config_template() -> String {
    r#"# steampipe.toml — cluster-ctl project configuration

# Backend: "microvm" (default), "docker", or "local"
# backend = "microvm"

# ── Required fields ──

# The game binary name (as built by cargo)
binary_name = "my-game"

# The cargo package to build (for `cargo build -p`)
cargo_package = "my-game"

# SSH user on VMs
vm_user = "player"

# Directory on VMs where the binary + assets are deployed
remote_dir = "/home/player/game"

# ── Optional fields (defaults shown) ──

# Path to Steam API shared library (relative to project root)
# steam_api_lib = "vendor/steamworks-rs/steamworks-sys/lib/steam/redistributable_bin/linux64/libsteam_api.so"

# Steam app ID file (relative to project root)
# steam_appid_file = "steam_appid.txt"

# Assets directory to deploy (relative to project root)
# assets_dir = "assets"

# Game log filename on VMs
# log_file = "game.log"

# Maximum number of VMs (1-7)
# max_vms = 7

# SSH key path (relative to project root)
# ssh_key = "nix/test-cluster/cluster_key"

# On NixOS hosts with services.steampipe-cluster enabled, --credentials
# defaults to /etc/steampipe/credentials.toml automatically.

[network]
# bridge = "br-cluster"
# subnet = "10.0.100"
# prefix = 24
# host_ip = "10.0.100.254"
# udp_port = 27100

[cluster]
# name = "default"
# state_dir = "/absolute/path/to/shared/state"

# Docker backend settings (used when backend = "docker")
# [docker]
# image = "steamcmd/steamcmd:latest"
# network = "steampipe-net"

# Local backend settings (used when backend = "local")
# [local]
# work_dir = "/tmp/steampipe-local"

# Heartbeat stall threshold (seconds before declaring a test stalled)
# heartbeat_stall_secs = 30

# Exit code labels (map game exit codes to human-readable names)
# [exit_codes]
# 0 = "SUCCESS"
# 10 = "PHASE_WATCHDOG"
# 11 = "DAG_VIOLATION"

# Named test profiles (use with --profile <name>)
# [profile.smoke]
# players = 2
# max_runs = 5
# timeout = 120        # no semantic progress for 120s
# hard_timeout = 600   # optional emergency wall-clock cap
# network = "lan"

# [profile.stress]
# players = 8
# max_runs = 100
# timeout = 600
# hard_timeout = 1800

# Named chaos profiles (use with --chaos-profile <name> on test, or --profile <name> on netem)
# [chaos.unstable-wifi]
# latency = 80
# jitter = 20
# loss = 2.0

# [chaos.satellite]
# latency = 600
# jitter = 50
# loss = 0.5
# rate = 1000

# Notification hooks (template vars: {pass_count}, {fail_count}, {pass_rate}, {duration}, {exit_label})
# [hooks]
# on_complete = "notify-send 'Tests done: {pass_rate}% pass rate'"
# on_failure = "curl -X POST https://slack.webhook/... -d '{\"text\": \"Failed: {exit_label}\"}'"
"#.to_string()
}

/// Look up a test profile by name from a project config.
pub fn lookup_test_profile(project_root: &Path, name: &str) -> Option<TestProfile> {
    let proj = load_project_config(project_root)?;
    proj.profile?.get(name).cloned()
}

/// Look up a chaos profile by name from a project config.
pub fn lookup_chaos_profile(project_root: &Path, name: &str) -> Option<ChaosProfile> {
    let proj = load_project_config(project_root)?;
    proj.chaos?.get(name).cloned()
}

/// Flat struct mirroring yh-mcp's `SteampipeGlobalArgs` for `.yh.cluster.toml` output.
#[derive(serde::Serialize)]
struct YhClusterToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_count: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssh_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credentials: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_root: Option<String>,
}

fn resolve_path(base: &Path, p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() {
        p.to_string()
    } else {
        base.join(path).display().to_string()
    }
}

/// Generate `.yh.cluster.toml` content by merging CLI args over `steampipe.toml` values.
pub fn generate_yh_cluster_config(
    project_root: &Path,
    cli_vm_count: Option<u8>,
    cli_cluster: Option<&str>,
    cli_backend: Option<BackendKind>,
    cli_state_dir: Option<&Path>,
    cli_lock_dir: Option<&Path>,
    cli_ssh_key: Option<&Path>,
    cli_credentials: Option<&Path>,
) -> String {
    let proj = load_project_config(project_root);

    let cfg = YhClusterToml {
        vm_count: cli_vm_count.or_else(|| proj.as_ref().and_then(|p| p.max_vms)),
        backend: cli_backend
            .map(|b| format!("{b:?}").to_lowercase())
            .or_else(|| {
                proj.as_ref()
                    .and_then(|p| p.backend.as_ref())
                    .map(|b| format!("{b:?}").to_lowercase())
            }),
        cluster: cli_cluster.map(String::from).or_else(|| {
            proj.as_ref()
                .and_then(|p| p.cluster.as_ref())
                .and_then(|c| c.name.clone())
        }),
        state_dir: cli_state_dir.map(|p| p.display().to_string()).or_else(|| {
            proj.as_ref()
                .and_then(|p| p.cluster.as_ref())
                .and_then(|c| c.state_dir.clone())
        }),
        lock_dir: cli_lock_dir
            .map(|p| p.display().to_string())
            .or_else(|| proj.as_ref().and_then(|p| p.lock_dir.clone())),
        ssh_key: cli_ssh_key.map(|p| p.display().to_string()).or_else(|| {
            proj.as_ref()
                .and_then(|p| p.ssh_key.as_deref())
                .map(|k| resolve_path(project_root, k))
        }),
        credentials: cli_credentials.map(|p| {
            if p.is_absolute() {
                p.display().to_string()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(p).display().to_string())
                    .unwrap_or_else(|_| p.display().to_string())
            }
        }),
        project_root: Some(project_root.display().to_string()),
    };

    let body = toml::to_string_pretty(&cfg).expect("YhClusterToml serialization");
    format!(
        "# .yh.cluster.toml — cluster defaults for yh-mcp steampipe tools\n\
         # Generated by: cluster-ctl yh-config\n\n\
         {body}"
    )
}

// ── Typestate markers for bridge validation ──

/// Bridge has not been checked yet.
#[derive(Debug, Clone, Copy)]
pub struct Unchecked;
/// Bridge existence has been verified.
#[derive(Debug, Clone, Copy)]
pub struct BridgeReady;

/// Cluster-wide configuration.
///
/// The type parameter `S` tracks whether the network bridge has been validated:
/// - `ClusterConfig<Unchecked>` (default from `from_vm_count()`) — bridge not yet verified
/// - `ClusterConfig<BridgeReady>` (from `validate_bridge()`) — safe to start VMs
#[derive(Debug, Clone)]
pub struct ClusterConfig<S = Unchecked> {
    pub bridge: String,
    pub subnet: String,
    pub prefix: u8,
    pub host_ip: String,
    pub vm_user: String,
    pub remote_dir: String,
    pub binary_name: String,
    pub cargo_package: String,
    pub ssh_key: PathBuf,
    pub udp_port: u16,
    pub steam_api_lib: PathBuf,
    pub steam_appid_file: String,
    pub assets_dir: String,
    pub log_file: String,
    pub state_dir: PathBuf,
    pub cluster_name: String,
    /// Number of VMs requested by the caller before lease resolution.
    pub requested_vm_count: u8,
    /// Total VM pool size (from `steampipe.toml`'s `max_vms`, default 7).
    /// `vms` is the operational VM set for this command; `max_vms` is the pool to lease from.
    pub max_vms: u8,
    pub vms: Vec<VmDef>,
    pub backend: Backend,
    pub backend_kind: BackendKind,
    pub lock_dir: PathBuf,
    pub ram_per_vm: u64,
    pub host_ram_reserve: u64,
    pub tap_owner: Option<String>,
    /// Parsed exit code → label mapping from config.
    pub exit_codes: HashMap<i32, String>,
    /// Heartbeat stall threshold in seconds.
    pub heartbeat_stall_secs: u64,
    /// Notification hooks.
    pub hooks: HooksConfig,
    _state: PhantomData<S>,
}

/// Check whether a network bridge interface exists.
pub fn bridge_exists(name: &str) -> bool {
    std::process::Command::new("ip")
        .args(["link", "show", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

impl ClusterConfig<Unchecked> {
    /// Build config from steampipe.toml for a specific VM ID set.
    pub fn new(
        project_root: &Path,
        vm_ids: &[u8],
        cluster_name: Option<&str>,
        backend_override: Option<BackendKind>,
    ) -> anyhow::Result<Self> {
        let proj = load_project_config(project_root);
        Self::from_parts_with_vm_ids(
            project_root,
            proj,
            vm_ids,
            cluster_name,
            backend_override,
            None,
        )
    }

    /// Build config from steampipe.toml for a requested VM count.
    pub fn from_vm_count(
        project_root: &Path,
        vm_count: u8,
        cluster_name: Option<&str>,
        backend_override: Option<BackendKind>,
    ) -> anyhow::Result<Self> {
        let proj = load_project_config(project_root);
        Self::from_parts(
            project_root,
            proj,
            vm_count,
            cluster_name,
            backend_override,
            None,
        )
    }

    /// Audit for Steam-only host-config construction:
    ///
    /// - Dispatch sites: `steam check`, `login`, `guard`, `start`, `accounts`,
    ///   `clean-logins` at `src/main.rs:719`, `:730`, `:749`, `:755`, `:759`,
    ///   `:760`.
    /// - Steam field reads: `backend`, `vm_user`, `lock_dir`, `cluster_name`,
    ///   `state_dir`, and VM metadata in `src/game/steam.rs:283`, `:296`, `:327`,
    ///   `:354`, `:472`, `:478`, `:503`, `:516`, `:542`, `:578`, `:592`, `:600`,
    ///   `:603`, `:622`, `:666`.
    /// - Accounts field reads: `backend` and `vms` in `src/game/accounts.rs:15`,
    ///   `:19`, `:22`.
    /// - Clean-logins only consumes `config.vms` at `src/main.rs:764`.
    /// - Verified not read anywhere on that path: `remote_dir`, `binary_name`,
    ///   `cargo_package`, `steam_api_lib`, `steam_appid_file`, `assets_dir`,
    ///   `log_file`.
    ///
    /// Suitable only for commands that do not require project-rooted runner
    /// metadata. Those fields are populated with debug sentinels and release
    /// empty strings so accidental use is caught by `assert_project_fields_available`.
    pub fn from_host_config(
        host: &crate::core::nixos_module::NixosModuleConfig,
        vm_count: u8,
        cluster_name: Option<&str>,
        backend_override: Option<BackendKind>,
        ssh_key_override: Option<&Path>,
        state_dir_override: Option<&Path>,
        lock_dir_override: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let cluster_name = cluster_name.unwrap_or("default").to_string();
        let state_dir = match state_dir_override {
            Some(path) => PathBuf::from(path),
            None => xdg_state_dir(&cluster_name)?,
        };
        let lock_dir = match lock_dir_override {
            Some(path) => PathBuf::from(path),
            None => xdg_runtime_dir()?,
        };
        let ssh_key = match ssh_key_override {
            Some(path) => PathBuf::from(path),
            None => host
                .ssh_key
                .clone()
                .unwrap_or_else(default_host_config_ssh_key_path),
        };
        let backend_kind = backend_override.unwrap_or_default();
        let vm_user = host.vm_user.clone().unwrap_or_else(|| "nixos".to_string());
        let backend = build_backend(backend_kind, &ssh_key, &vm_user, None);
        let vm_ids: Vec<u8> = (1..=vm_count).collect();
        let vms = Self::vm_defs_for_ids(&host.subnet, vm_count, &vm_ids)?;
        let mb = 1024 * 1024;

        Ok(Self {
            bridge: host.bridge.clone(),
            subnet: host.subnet.clone(),
            prefix: host.prefix,
            host_ip: host.host_ip.clone(),
            vm_user,
            remote_dir: sentinel("remote_dir"),
            binary_name: sentinel("binary_name"),
            cargo_package: sentinel("cargo_package"),
            ssh_key,
            udp_port: 27100,
            steam_api_lib: PathBuf::new(),
            steam_appid_file: sentinel("steam_appid_file"),
            assets_dir: sentinel("assets_dir"),
            log_file: sentinel("log_file"),
            state_dir,
            cluster_name,
            requested_vm_count: vm_count,
            max_vms: vm_count,
            vms,
            backend,
            backend_kind,
            lock_dir,
            ram_per_vm: 2048 * mb,
            host_ram_reserve: 2048 * mb,
            tap_owner: host.tap_owner.clone(),
            exit_codes: HashMap::new(),
            heartbeat_stall_secs: 30,
            hooks: HooksConfig::default(),
            _state: PhantomData,
        })
    }

    /// Back-compat wrapper for the earlier phase name.
    #[allow(dead_code)]
    pub fn from_module(
        module: &crate::core::nixos_module::NixosModuleConfig,
        cluster_name: Option<&str>,
        backend_override: Option<BackendKind>,
        state_dir_override: Option<&Path>,
        lock_dir_override: Option<&Path>,
        ssh_key_override: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::from_host_config(
            module,
            module.vm_count,
            cluster_name,
            backend_override,
            ssh_key_override,
            state_dir_override,
            lock_dir_override,
        )
    }

    /// Build config from an already-loaded `ProjectConfig` (no filesystem access).
    ///
    /// `new()` delegates here after loading `steampipe.toml`. Tests can call this
    /// directly with a synthetic `ProjectConfig`.
    pub fn from_parts(
        project_root: &Path,
        proj: Option<ProjectConfig>,
        vm_count: u8,
        cluster_name: Option<&str>,
        backend_override: Option<BackendKind>,
        lock_dir_override: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::from_parts_inner(
            project_root,
            proj,
            vm_count,
            None,
            cluster_name,
            backend_override,
            lock_dir_override,
        )
    }

    /// Build config from an explicit VM ID set with an already-loaded `ProjectConfig`.
    pub fn from_parts_with_vm_ids(
        project_root: &Path,
        proj: Option<ProjectConfig>,
        vm_ids: &[u8],
        cluster_name: Option<&str>,
        backend_override: Option<BackendKind>,
        lock_dir_override: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let requested_vm_count = u8::try_from(vm_ids.len())
            .map_err(|_| anyhow::anyhow!("too many VM IDs requested: {}", vm_ids.len()))?;
        Self::from_parts_inner(
            project_root,
            proj,
            requested_vm_count,
            Some(vm_ids),
            cluster_name,
            backend_override,
            lock_dir_override,
        )
    }

    fn from_parts_inner(
        project_root: &Path,
        proj: Option<ProjectConfig>,
        requested_vm_count: u8,
        vm_ids: Option<&[u8]>,
        cluster_name: Option<&str>,
        backend_override: Option<BackendKind>,
        lock_dir_override: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let cluster_name = cluster_name
            .map(String::from)
            .or_else(|| proj.as_ref().and_then(|p| p.cluster.as_ref()?.name.clone()))
            .unwrap_or_else(|| "default".into());

        let state_dir = proj
            .as_ref()
            .and_then(|p| p.cluster.as_ref()?.state_dir.clone())
            .map(PathBuf::from)
            .unwrap_or_else(|| cluster_state_dir(&cluster_name));

        let max_vms = proj.as_ref().and_then(|p| p.max_vms).unwrap_or(7);
        let requested_vm_count = requested_vm_count.clamp(1, max_vms);
        let subnet = proj
            .as_ref()
            .and_then(|p| p.network.as_ref()?.subnet.clone())
            .unwrap_or_else(|| "10.0.100".into());

        let vms = if let Some(vm_ids) = vm_ids {
            Self::vm_defs_for_ids(&subnet, max_vms, vm_ids)?
        } else {
            let vm_ids: Vec<u8> = (1..=requested_vm_count).collect();
            Self::vm_defs_for_ids(&subnet, max_vms, &vm_ids)?
        };

        let ssh_key = proj
            .as_ref()
            .and_then(|p| p.ssh_key.clone())
            .map(|k| project_root.join(k))
            .unwrap_or_else(|| project_root.join("nix/test-cluster/cluster_key"));

        let require = |field: &str, val: Option<String>| -> anyhow::Result<String> {
            val.ok_or_else(|| anyhow::anyhow!("'{field}' is required in steampipe.toml"))
        };

        let vm_user = require("vm_user", proj.as_ref().and_then(|p| p.vm_user.clone()))?;

        let backend_kind = backend_override
            .or(proj.as_ref().and_then(|p| p.backend))
            .unwrap_or_default();

        let backend = match backend_kind {
            BackendKind::Microvm => Backend::new_microvm(&ssh_key, &vm_user),
            BackendKind::Docker => {
                let docker_cfg = proj.as_ref().and_then(|p| p.docker.as_ref());
                Backend::new_docker(
                    docker_cfg
                        .and_then(|d| d.image.as_deref())
                        .unwrap_or("steamcmd/steamcmd:latest"),
                    docker_cfg
                        .and_then(|d| d.network.as_deref())
                        .unwrap_or("steampipe-net"),
                )
            }
            BackendKind::Local => {
                let local_cfg = proj.as_ref().and_then(|p| p.local.as_ref());
                let work_dir = local_cfg
                    .and_then(|l| l.work_dir.as_deref())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::temp_dir().join("steampipe-local"));
                Backend::new_local(work_dir)
            }
        };

        let lock_dir = lock_dir_override
            .map(PathBuf::from)
            .or_else(|| {
                proj.as_ref()
                    .and_then(|p| p.lock_dir.clone())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(crate::vm::lease::default_lock_dir);

        let mb = 1024 * 1024;
        let ram_per_vm = proj.as_ref().and_then(|p| p.ram_per_vm_mb).unwrap_or(2048) * mb;
        let host_ram_reserve = proj
            .as_ref()
            .and_then(|p| p.host_ram_reserve_mb)
            .unwrap_or(2048)
            * mb;

        Ok(Self {
            bridge: proj.as_ref().and_then(|p| p.network.as_ref()?.bridge.clone()).unwrap_or_else(|| "br-cluster".into()),
            subnet,
            prefix: proj.as_ref().and_then(|p| p.network.as_ref()?.prefix).unwrap_or(24),
            host_ip: proj.as_ref().and_then(|p| p.network.as_ref()?.host_ip.clone()).unwrap_or_else(|| "10.0.100.254".into()),
            vm_user,
            remote_dir: require("remote_dir", proj.as_ref().and_then(|p| p.remote_dir.clone()))?,
            binary_name: require("binary_name", proj.as_ref().and_then(|p| p.binary_name.clone()))?,
            cargo_package: require("cargo_package", proj.as_ref().and_then(|p| p.cargo_package.clone()))?,
            ssh_key,
            udp_port: proj.as_ref().and_then(|p| p.network.as_ref()?.udp_port).unwrap_or(27100),
            steam_api_lib: proj.as_ref()
                .and_then(|p| p.steam_api_lib.clone())
                .map(|s| project_root.join(s))
                .unwrap_or_else(|| project_root.join("vendor/steamworks-rs/steamworks-sys/lib/steam/redistributable_bin/linux64/libsteam_api.so")),
            steam_appid_file: proj.as_ref().and_then(|p| p.steam_appid_file.clone()).unwrap_or_else(|| "steam_appid.txt".into()),
            assets_dir: proj.as_ref().and_then(|p| p.assets_dir.clone()).unwrap_or_else(|| "assets".into()),
            log_file: proj.as_ref().and_then(|p| p.log_file.clone()).unwrap_or_else(|| "game.log".into()),
            state_dir,
            cluster_name,
            requested_vm_count,
            max_vms,
            vms,
            backend,
            backend_kind,
            lock_dir,
            ram_per_vm,
            host_ram_reserve,
            tap_owner: proj.as_ref().and_then(|p| p.network.as_ref()?.tap_owner.clone()),
            exit_codes: proj
                .as_ref()
                .and_then(|p| p.exit_codes.as_ref())
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| Some((k.parse::<i32>().ok()?, v.clone())))
                        .collect()
                })
                .unwrap_or_default(),
            heartbeat_stall_secs: proj
                .as_ref()
                .and_then(|p| p.heartbeat_stall_secs)
                .unwrap_or(30),
            hooks: proj
                .as_ref()
                .and_then(|p| p.hooks.clone())
                .unwrap_or_default(),
            _state: PhantomData,
        })
    }

    /// Check that the network bridge exists, transitioning to `BridgeReady` on success.
    pub fn validate_bridge(self) -> anyhow::Result<ClusterConfig<BridgeReady>> {
        if !bridge_exists(&self.bridge) {
            anyhow::bail!(
                "Bridge {} not found. Run 'cluster-ctl net-up' first.",
                self.bridge
            );
        }
        Ok(self.into_state())
    }

    /// Validate bridge for MicroVM, or skip validation for Docker/Local backends.
    pub fn validate_or_skip_bridge(self) -> anyhow::Result<ClusterConfig<BridgeReady>> {
        match self.backend_kind {
            BackendKind::Microvm => self.validate_bridge(),
            BackendKind::Docker | BackendKind::Local => Ok(self.into_state()),
        }
    }
}

/// Methods available on any config state (both Unchecked and BridgeReady).
impl<S> ClusterConfig<S> {
    #[cfg(debug_assertions)]
    pub fn assert_project_fields_available(&self) {
        debug_assert!(
            !self.remote_dir.is_empty()
                && !is_sentinel_field(&self.remote_dir)
                && !self.binary_name.is_empty()
                && !is_sentinel_field(&self.binary_name)
                && !self.cargo_package.is_empty()
                && !is_sentinel_field(&self.cargo_package),
            "host-config-only ClusterConfig carries sentinel project fields; this code path requires a project-rooted ClusterConfig"
        );
    }

    #[cfg(not(debug_assertions))]
    pub fn assert_project_fields_available(&self) {}

    fn vm_defs_for_ids(subnet: &str, max_vms: u8, vm_ids: &[u8]) -> anyhow::Result<Vec<VmDef>> {
        vm_ids
            .iter()
            .copied()
            .map(|id| {
                if id == 0 || id > max_vms {
                    anyhow::bail!("vm-{id} is outside the configured pool 1..={max_vms}");
                }
                Ok(VmDef {
                    name: VmName(format!("vm-{id}")),
                    ip: IpAddr(format!("{subnet}.{id}")),
                    index: id,
                })
            })
            .collect()
    }

    /// Transition to a different typestate.
    fn into_state<T>(self) -> ClusterConfig<T> {
        ClusterConfig {
            bridge: self.bridge,
            subnet: self.subnet,
            prefix: self.prefix,
            host_ip: self.host_ip,
            vm_user: self.vm_user,
            remote_dir: self.remote_dir,
            binary_name: self.binary_name,
            cargo_package: self.cargo_package,
            ssh_key: self.ssh_key,
            udp_port: self.udp_port,
            steam_api_lib: self.steam_api_lib,
            steam_appid_file: self.steam_appid_file,
            assets_dir: self.assets_dir,
            log_file: self.log_file,
            state_dir: self.state_dir,
            cluster_name: self.cluster_name,
            requested_vm_count: self.requested_vm_count,
            max_vms: self.max_vms,
            vms: self.vms,
            backend: self.backend,
            backend_kind: self.backend_kind,
            lock_dir: self.lock_dir,
            ram_per_vm: self.ram_per_vm,
            host_ram_reserve: self.host_ram_reserve,
            tap_owner: self.tap_owner,
            exit_codes: self.exit_codes,
            heartbeat_stall_secs: self.heartbeat_stall_secs,
            hooks: self.hooks,
            _state: PhantomData,
        }
    }

    /// Replace the operational VM set with an explicit list of VM IDs.
    pub fn with_vm_ids(mut self, vm_ids: &[u8]) -> anyhow::Result<Self> {
        self.vms = Self::vm_defs_for_ids(&self.subnet, self.max_vms, vm_ids)?;
        Ok(self)
    }

    /// Look up a VM by name ("vm-3") or index ("3").
    pub fn find_vm(&self, target: &str) -> Option<&VmDef> {
        if let Ok(i) = target.parse::<u8>() {
            return self.vms.iter().find(|vm| vm.index == i);
        }
        self.vms.iter().find(|vm| vm.name == target)
    }

    /// Return VMs currently leased by this cluster.
    pub fn claimed_vms(&self) -> Vec<VmDef> {
        let ids =
            crate::vm::lease::list_cluster_vms(&self.cluster_name, self.max_vms, &self.lock_dir);
        Self::vm_defs_for_ids(&self.subnet, self.max_vms, &ids).unwrap_or_default()
    }

    /// Parse a target string into a list of VMs.
    pub fn resolve_targets(
        &self,
        target: Option<&str>,
        continue_from: bool,
    ) -> anyhow::Result<Vec<VmDef>> {
        match target {
            None | Some("all") => Ok(self.vms.clone()),
            Some(t) => {
                let vm = self.find_vm(t).ok_or_else(|| {
                    let expected = self
                        .vms
                        .iter()
                        .map(|vm| format!("{} ({})", vm.name, vm.index))
                        .collect::<Vec<_>>()
                        .join(", ");
                    if expected.is_empty() {
                        anyhow::anyhow!("Unknown VM: {t} (this cluster has no active VM set)")
                    } else {
                        anyhow::anyhow!("Unknown VM: {t} (expected one of: {expected})")
                    }
                })?;
                if continue_from {
                    Ok(self
                        .vms
                        .iter()
                        .filter(|v| v.index >= vm.index)
                        .cloned()
                        .collect())
                } else {
                    Ok(vec![vm.clone()])
                }
            }
        }
    }
}

fn default_state_root() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|d| d.join(".local/state"))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        })
        .join("steampipe")
}

fn cluster_state_dir(cluster_name: &str) -> PathBuf {
    let base_state_dir = default_state_root();
    if cluster_name == "default" {
        base_state_dir
    } else {
        base_state_dir.join(cluster_name)
    }
}

fn build_backend(
    backend_kind: BackendKind,
    ssh_key: &Path,
    vm_user: &str,
    proj: Option<&ProjectConfig>,
) -> Backend {
    match backend_kind {
        BackendKind::Microvm => Backend::new_microvm(ssh_key, vm_user),
        BackendKind::Docker => {
            let docker_cfg = proj.and_then(|p| p.docker.as_ref());
            Backend::new_docker(
                docker_cfg
                    .and_then(|d| d.image.as_deref())
                    .unwrap_or("steamcmd/steamcmd:latest"),
                docker_cfg
                    .and_then(|d| d.network.as_deref())
                    .unwrap_or("steampipe-net"),
            )
        }
        BackendKind::Local => {
            let local_cfg = proj.and_then(|p| p.local.as_ref());
            let work_dir = local_cfg
                .and_then(|l| l.work_dir.as_deref())
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("steampipe-local"));
            Backend::new_local(work_dir)
        }
    }
}

pub fn module_ssh_key_candidates() -> Vec<PathBuf> {
    let config_dir = xdg_config_home_from_env(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .or_else(dirs::config_dir)
    .unwrap_or_else(|| {
        dirs::home_dir()
            .map(|home| home.join(".config"))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    });

    vec![
        config_dir.join("steampipe/cluster_key"),
        PathBuf::from("/etc/steampipe/cluster_key"),
    ]
}

fn sentinel(field: &str) -> String {
    #[cfg(debug_assertions)]
    {
        format!("__STEAMPIPE_SENTINEL_{field}__")
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = field;
        String::new()
    }
}

#[cfg(debug_assertions)]
fn is_sentinel_field(value: &str) -> bool {
    value.starts_with("__STEAMPIPE_SENTINEL_")
}

#[cfg(test)]
#[allow(dead_code)]
fn default_ssh_key_for_host_config() -> anyhow::Result<PathBuf> {
    default_ssh_key_for_host_config_from_candidates(host_config_ssh_key_candidates())
}

fn default_host_config_ssh_key_path() -> PathBuf {
    let candidates = host_config_ssh_key_candidates();
    candidates
        .iter()
        .find(|path| path.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
        .unwrap_or_else(|| PathBuf::from("/etc/steampipe/cluster_key"))
}

#[cfg(test)]
#[allow(dead_code)]
fn default_ssh_key_for_host_config_from_candidates(
    candidates: Vec<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = candidates.iter().find(|path| path.exists()) {
        return Ok(path.clone());
    }

    anyhow::bail!(
        "no SSH key found at any of: {}. Pass --ssh-key explicitly or place a key at one of those paths.",
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn host_config_ssh_key_candidates() -> Vec<PathBuf> {
    host_config_ssh_key_candidates_from_env(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

fn host_config_ssh_key_candidates_from_env(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(config_home) = xdg_config_home_from_env(xdg_config_home, home) {
        candidates.push(config_home.join("steampipe/cluster_key"));
    }
    if let Some(root) = std::env::var_os("STEAMPIPE_ETC_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        candidates.push(root.join("steampipe/cluster_key"));
    } else {
        candidates.push(PathBuf::from("/etc/steampipe/cluster_key"));
    }
    candidates
}

fn xdg_config_home_from_env(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
}

fn xdg_state_dir(cluster_name: &str) -> anyhow::Result<PathBuf> {
    xdg_state_dir_from_env(
        cluster_name,
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
    )
}

fn xdg_state_dir_from_env(
    cluster_name: &str,
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> anyhow::Result<PathBuf> {
    let base = xdg_state_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })
        .ok_or_else(|| anyhow::anyhow!("neither XDG_STATE_HOME nor HOME is set"))?;
    Ok(base.join("steampipe").join(cluster_name))
}

fn xdg_runtime_dir() -> anyhow::Result<PathBuf> {
    xdg_runtime_dir_from_env(std::env::var_os("XDG_RUNTIME_DIR"))
}

fn xdg_runtime_dir_from_env(
    xdg_runtime_dir: Option<std::ffi::OsString>,
) -> anyhow::Result<PathBuf> {
    xdg_runtime_dir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join("steampipe"))
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is not set"))
}

#[cfg(test)]
impl ClusterConfig<Unchecked> {
    /// Build a minimal config for testing. No filesystem access.
    pub fn for_test(vm_count: u8) -> Self {
        let vm_ids: Vec<u8> = (1..=vm_count).collect();
        Self::for_test_ids(&vm_ids)
    }

    /// Build a minimal config for testing from explicit VM IDs.
    pub fn for_test_ids(vm_ids: &[u8]) -> Self {
        let subnet = "10.0.100";
        let max_vms = vm_ids.iter().copied().max().unwrap_or(1);
        let vms = Self::vm_defs_for_ids(subnet, max_vms, vm_ids).expect("test vm ids");

        Self {
            bridge: "br-test".into(),
            subnet: subnet.into(),
            prefix: 24,
            host_ip: "10.0.100.254".into(),
            vm_user: "testuser".into(),
            remote_dir: "/home/testuser/game".into(),
            binary_name: "test-game".into(),
            cargo_package: "test-game".into(),
            ssh_key: PathBuf::from("/tmp/fake-key"),
            udp_port: 27100,
            steam_api_lib: PathBuf::from("/tmp/fake-steam.so"),
            steam_appid_file: "steam_appid.txt".into(),
            assets_dir: "assets".into(),
            log_file: "game.log".into(),
            state_dir: std::env::temp_dir().join("steampipe-test"),
            cluster_name: "test".into(),
            requested_vm_count: u8::try_from(vm_ids.len()).expect("test vm count"),
            max_vms,
            vms,
            backend: Backend::new_local(std::env::temp_dir()),
            backend_kind: BackendKind::Local,
            lock_dir: std::env::temp_dir().join("steampipe-test-locks"),
            ram_per_vm: 2048 * 1024 * 1024,
            host_ram_reserve: 2048 * 1024 * 1024,
            tap_owner: None,
            exit_codes: HashMap::new(),
            heartbeat_stall_secs: 30,
            hooks: HooksConfig::default(),
            _state: PhantomData,
        }
    }
}

/// Auto-detect project root by walking up from CWD looking for steampipe.toml.
pub fn detect_project_root() -> anyhow::Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("steampipe.toml").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            anyhow::bail!(
                "Could not find project root (no steampipe.toml found).\n\
                 Run 'cluster-ctl init' to create one, or use --project-root."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::nixos_module::NixosModuleConfig;

    #[test]
    fn find_vm_by_name() {
        let config = ClusterConfig::for_test(3);
        let vm = config.find_vm("vm-2").unwrap();
        assert_eq!(vm.index, 2);
        assert_eq!(&*vm.ip, "10.0.100.2");
    }

    #[test]
    fn find_vm_by_index() {
        let config = ClusterConfig::for_test(3);
        let vm = config.find_vm("3").unwrap();
        assert_eq!(&*vm.name, "vm-3");
    }

    #[test]
    fn find_vm_missing() {
        let config = ClusterConfig::for_test(3);
        assert!(config.find_vm("vm-5").is_none());
        assert!(config.find_vm("99").is_none());
    }

    #[test]
    fn resolve_targets_all() {
        let config = ClusterConfig::for_test(3);
        let targets = config.resolve_targets(None, false).unwrap();
        assert_eq!(targets.len(), 3);
    }

    #[test]
    fn resolve_targets_specific() {
        let config = ClusterConfig::for_test(3);
        let targets = config.resolve_targets(Some("vm-2"), false).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(&*targets[0].name, "vm-2");
    }

    #[test]
    fn resolve_targets_continue_from() {
        let config = ClusterConfig::for_test(5);
        let targets = config.resolve_targets(Some("vm-3"), true).unwrap();
        assert_eq!(targets.len(), 3); // vm-3, vm-4, vm-5
        assert_eq!(targets[0].index, 3);
    }

    #[test]
    fn resolve_targets_unknown_vm_errors() {
        let config = ClusterConfig::for_test(3);
        assert!(config.resolve_targets(Some("vm-99"), false).is_err());
    }

    #[test]
    fn resolve_targets_continue_from_non_contiguous_set() {
        let config = ClusterConfig::for_test_ids(&[2, 4, 6]);
        let targets = config.resolve_targets(Some("vm-4"), true).unwrap();
        assert_eq!(
            targets.iter().map(|vm| vm.index).collect::<Vec<_>>(),
            vec![4, 6]
        );
    }

    #[test]
    fn with_vm_ids_replaces_operational_vm_set() {
        let config = ClusterConfig::for_test(4).with_vm_ids(&[2, 4]).unwrap();
        assert_eq!(config.requested_vm_count, 4);
        assert_eq!(
            config.vms.iter().map(|vm| vm.index).collect::<Vec<_>>(),
            vec![2, 4]
        );
    }

    #[test]
    fn from_parts_defaults_when_no_project_config() {
        let root = std::env::temp_dir();
        let proj = Some(ProjectConfig {
            binary_name: Some("mygame".into()),
            cargo_package: Some("mygame".into()),
            vm_user: Some("nixos".into()),
            remote_dir: Some("/home/nixos/game".into()),
            ..Default::default()
        });
        let config = ClusterConfig::from_parts(&root, proj, 2, None, None, None).unwrap();
        assert_eq!(config.bridge, "br-cluster");
        assert_eq!(config.prefix, 24);
        assert_eq!(config.host_ip, "10.0.100.254");
        assert_eq!(config.cluster_name, "default");
        assert_eq!(config.vms.len(), 2);
    }

    #[test]
    fn from_parts_required_fields_error() {
        let root = std::env::temp_dir();
        // No binary_name → should fail
        let proj = Some(ProjectConfig {
            vm_user: Some("nixos".into()),
            remote_dir: Some("/home/nixos/game".into()),
            ..Default::default()
        });
        let result = ClusterConfig::from_parts(&root, proj, 1, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("binary_name"));
    }

    #[test]
    fn from_parts_vm_ip_generation() {
        let root = std::env::temp_dir();
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            network: Some(NetworkFileConfig {
                subnet: Some("192.168.50".into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let config = ClusterConfig::from_parts(&root, proj, 3, None, None, None).unwrap();
        assert_eq!(&*config.vms[0].ip, "192.168.50.1");
        assert_eq!(&*config.vms[2].ip, "192.168.50.3");
    }

    #[test]
    fn from_parts_cluster_name_override() {
        let root = std::env::temp_dir();
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            cluster: Some(ClusterSection {
                name: Some("from-toml".into()),
                state_dir: None,
            }),
            ..Default::default()
        });
        // CLI override wins
        let config =
            ClusterConfig::from_parts(&root, proj, 1, Some("cli-cluster"), None, None).unwrap();
        assert_eq!(config.cluster_name, "cli-cluster");
    }

    #[test]
    fn from_host_config_populates_vms_from_subnet() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            None,
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert_eq!(config.max_vms, 3);
        assert_eq!(config.requested_vm_count, 3);
        assert_eq!(config.vms.len(), 3);
        assert_eq!(&*config.vms[0].name, "vm-1");
        assert_eq!(&*config.vms[0].ip, "10.44.0.1");
        assert_eq!(&*config.vms[2].ip, "10.44.0.3");
    }

    #[test]
    fn from_host_config_uses_cluster_name_override() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            Some("special"),
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert_eq!(config.cluster_name, "special");
    }

    #[test]
    fn from_host_config_defaults_cluster_name_to_default() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert_eq!(config.cluster_name, "default");
    }

    #[test]
    fn from_host_config_defaults_backend_to_microvm() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            None,
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert!(matches!(config.backend_kind, BackendKind::Microvm));
    }

    #[test]
    fn from_host_config_max_vms_equals_vm_count() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert_eq!(config.max_vms, host.vm_count);
        assert_eq!(config.requested_vm_count, host.vm_count);
    }

    #[test]
    fn from_host_config_propagates_network_fields() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert_eq!(config.bridge, host.bridge);
        assert_eq!(config.subnet, host.subnet);
        assert_eq!(config.prefix, host.prefix);
        assert_eq!(config.host_ip, host.host_ip);
    }

    #[test]
    fn from_host_config_uses_module_ssh_key_when_cli_key_missing() {
        let mut host = sample_module_config();
        host.ssh_key = Some(PathBuf::from("/etc/steampipe/ssh/cluster_key"));
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            None,
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert_eq!(
            config.ssh_key,
            PathBuf::from("/etc/steampipe/ssh/cluster_key")
        );
    }

    #[test]
    fn from_host_config_cli_ssh_key_overrides_module_ssh_key() {
        let mut host = sample_module_config();
        host.ssh_key = Some(PathBuf::from("/etc/steampipe/ssh/cluster_key"));
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert_eq!(config.ssh_key, ssh_key.path());
    }

    #[test]
    fn from_host_config_uses_module_vm_user_when_present() {
        let mut host = sample_module_config();
        host.vm_user = Some("cluster".into());
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert_eq!(config.vm_user, "cluster");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn from_host_config_sentinel_fields_in_debug_have_distinct_marker() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert!(config.binary_name.contains("SENTINEL_binary_name"));
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn from_host_config_sentinel_fields_in_release_are_empty() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        assert!(config.binary_name.is_empty());
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "host-config-only ClusterConfig carries sentinel project fields")]
    fn from_host_config_sentinel_fields_panic_on_assert() {
        let host = sample_module_config();
        let ssh_key = fake_ssh_key();
        let tempdir = tempfile::tempdir().unwrap();
        let state_dir = tempdir.path().join("state");
        let lock_dir = tempdir.path().join("locks");
        let config = ClusterConfig::from_host_config(
            &host,
            host.vm_count,
            None,
            Some(BackendKind::Local),
            Some(ssh_key.path()),
            Some(&state_dir),
            Some(&lock_dir),
        )
        .unwrap();

        config.assert_project_fields_available();
    }

    #[test]
    fn default_ssh_key_for_host_config_errors_with_all_candidates_listed() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_home = tempdir.path().join("config");
        let expected_user = config_home.join("steampipe/cluster_key");
        let expected_system = PathBuf::from("/etc/steampipe/cluster_key");

        let candidates = host_config_ssh_key_candidates_from_env(
            Some(config_home.into_os_string()),
            Some(tempdir.path().join("home").into_os_string()),
        );
        let error = default_ssh_key_for_host_config_from_candidates(candidates).unwrap_err();
        let message = error.to_string();

        assert!(message.contains(&expected_user.display().to_string()));
        assert!(message.contains(&expected_system.display().to_string()));
    }

    #[test]
    fn xdg_state_dir_prefers_xdg_state_home() {
        let path = xdg_state_dir_from_env(
            "default",
            Some("/tmp/state-home".into()),
            Some("/tmp/home".into()),
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/tmp/state-home/steampipe/default"));
    }

    #[test]
    fn xdg_state_dir_falls_back_to_home_local_state() {
        let path = xdg_state_dir_from_env("default", None, Some("/tmp/home".into())).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/home/.local/state/steampipe/default")
        );
    }

    #[test]
    fn xdg_state_dir_errors_when_no_env_present() {
        let error = xdg_state_dir_from_env("default", None, None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("neither XDG_STATE_HOME nor HOME is set")
        );
    }

    // ── New config struct tests ──��──────────────────────────────────────

    #[test]
    fn test_profile_deserialize() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [profile.smoke]
            players = 2
            max_runs = 5
            timeout = 120
            hard_timeout = 600
            network = "lan"
            display = "headless"
            chaos_profile = "wifi"
            heartbeat_stall_secs = 45
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let profiles = proj.profile.unwrap();
        let smoke = profiles.get("smoke").unwrap();
        assert_eq!(smoke.players, Some(2));
        assert_eq!(smoke.max_runs, Some(5));
        assert_eq!(smoke.timeout, Some(120));
        assert_eq!(smoke.hard_timeout, Some(600));
        assert_eq!(smoke.network.as_deref(), Some("lan"));
        assert_eq!(smoke.display.as_deref(), Some("headless"));
        assert_eq!(smoke.chaos_profile.as_deref(), Some("wifi"));
        assert_eq!(smoke.heartbeat_stall_secs, Some(45));
    }

    #[test]
    fn chaos_profile_deserialize() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [chaos.unstable-wifi]
            latency = 80
            jitter = 20
            loss = 2.0

            [chaos.satellite]
            latency = 600
            jitter = 50
            loss = 0.5
            rate = 1000
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let chaos = proj.chaos.unwrap();
        let wifi = chaos.get("unstable-wifi").unwrap();
        assert_eq!(wifi.latency, Some(80));
        assert_eq!(wifi.jitter, Some(20));
        assert_eq!(wifi.loss, Some(2.0));
        assert!(wifi.rate.is_none());

        let sat = chaos.get("satellite").unwrap();
        assert_eq!(sat.latency, Some(600));
        assert_eq!(sat.rate, Some(1000));
    }

    #[test]
    fn exit_codes_deserialize() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [exit_codes]
            0 = "SUCCESS"
            10 = "MY_WATCHDOG"
            42 = "CUSTOM_ERROR"
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let codes = proj.exit_codes.unwrap();
        assert_eq!(codes.get("0").unwrap(), "SUCCESS");
        assert_eq!(codes.get("10").unwrap(), "MY_WATCHDOG");
        assert_eq!(codes.get("42").unwrap(), "CUSTOM_ERROR");
    }

    #[test]
    fn hooks_config_deserialize() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [hooks]
            on_complete = "echo done {pass_rate}"
            on_failure = "curl http://hook"
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let hooks = proj.hooks.unwrap();
        assert_eq!(hooks.on_complete.as_deref(), Some("echo done {pass_rate}"));
        assert_eq!(hooks.on_failure.as_deref(), Some("curl http://hook"));
    }

    #[test]
    fn heartbeat_stall_secs_in_config() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"
            heartbeat_stall_secs = 45
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(proj.heartbeat_stall_secs, Some(45));
    }

    #[test]
    fn from_parts_exit_codes_parsed() {
        let root = std::env::temp_dir();
        let mut exit_codes = HashMap::new();
        exit_codes.insert("42".into(), "CUSTOM".into());
        exit_codes.insert("invalid".into(), "IGNORED".into()); // non-numeric key
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            exit_codes: Some(exit_codes),
            ..Default::default()
        });
        let config = ClusterConfig::from_parts(&root, proj, 1, None, None, None).unwrap();
        assert_eq!(config.exit_codes.get(&42).unwrap(), "CUSTOM");
        assert!(!config.exit_codes.contains_key(&0)); // "invalid" key was skipped
    }

    #[test]
    fn from_parts_heartbeat_stall_secs_override() {
        let root = std::env::temp_dir();
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            heartbeat_stall_secs: Some(60),
            ..Default::default()
        });
        let config = ClusterConfig::from_parts(&root, proj, 1, None, None, None).unwrap();
        assert_eq!(config.heartbeat_stall_secs, 60);
    }

    #[test]
    fn from_parts_heartbeat_stall_secs_default() {
        let root = std::env::temp_dir();
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            ..Default::default()
        });
        let config = ClusterConfig::from_parts(&root, proj, 1, None, None, None).unwrap();
        assert_eq!(config.heartbeat_stall_secs, 30);
    }

    #[test]
    fn from_parts_hooks_carried_through() {
        let root = std::env::temp_dir();
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            hooks: Some(HooksConfig {
                on_complete: Some("echo done".into()),
                on_failure: None,
            }),
            ..Default::default()
        });
        let config = ClusterConfig::from_parts(&root, proj, 1, None, None, None).unwrap();
        assert_eq!(config.hooks.on_complete.as_deref(), Some("echo done"));
        assert!(config.hooks.on_failure.is_none());
    }

    #[test]
    fn multiple_profiles_deserialize() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [profile.smoke]
            players = 2
            max_runs = 5

            [profile.stress]
            players = 8
            max_runs = 100
            timeout = 600
            hard_timeout = 1800
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let profiles = proj.profile.unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles.get("smoke").unwrap().max_runs, Some(5));
        assert_eq!(profiles.get("stress").unwrap().players, Some(8));
        assert_eq!(profiles.get("stress").unwrap().timeout, Some(600));
        assert_eq!(profiles.get("stress").unwrap().hard_timeout, Some(1800));
    }

    #[test]
    fn for_test_has_new_fields() {
        let config = ClusterConfig::for_test(2);
        assert_eq!(config.heartbeat_stall_secs, 30);
        assert!(config.exit_codes.is_empty());
        assert!(config.hooks.on_complete.is_none());
    }

    // ── Config template tests ───────────────────────────────────────────

    #[test]
    fn config_template_includes_new_sections() {
        let template = generate_config_template();
        assert!(
            template.contains("[exit_codes]"),
            "template missing exit_codes section"
        );
        assert!(
            template.contains("[profile.smoke]"),
            "template missing profile section"
        );
        assert!(
            template.contains("[chaos.unstable-wifi]"),
            "template missing chaos section"
        );
        assert!(
            template.contains("[hooks]"),
            "template missing hooks section"
        );
        assert!(
            template.contains("heartbeat_stall_secs"),
            "template missing heartbeat_stall_secs"
        );
        assert!(
            template.contains("on_complete"),
            "template missing on_complete hook"
        );
        assert!(
            template.contains("on_failure"),
            "template missing on_failure hook"
        );
    }

    // ── TOML deserialization edge cases ──────────────────────────────────

    #[test]
    fn empty_profiles_section() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"
            [profile]
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert!(proj.profile.unwrap().is_empty());
    }

    #[test]
    fn empty_chaos_section() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"
            [chaos]
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert!(proj.chaos.unwrap().is_empty());
    }

    #[test]
    fn profile_with_all_fields() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [profile.full]
            players = 4
            max_runs = 50
            timeout = 600
            hard_timeout = 1800
            shutdown_timeout = 10
            network = "steam"
            display = "sway"
            vm_args = "--auto-join --headless"
            host_args = "--auto-host-steam"
            no_build = true
            no_deploy = false
            no_stop_on_failure = true
            capture_on_failure = true
            screenshot_backend = "vnc"
            visual_validator = "validator --image $STEAMPIPE_VISUAL_IMAGE"
            filter_pattern = "FAIL|PASS"
            output_file = "/tmp/out.txt"
            chaos_profile = "satellite"
            heartbeat_stall_secs = 60
            output_format = "junit"
            on_complete = "echo done"
            on_failure = "echo fail"
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let full = proj.profile.unwrap().get("full").unwrap().clone();
        assert_eq!(full.players, Some(4));
        assert_eq!(full.max_runs, Some(50));
        assert_eq!(full.timeout, Some(600));
        assert_eq!(full.hard_timeout, Some(1800));
        assert_eq!(full.shutdown_timeout, Some(10));
        assert_eq!(full.network.as_deref(), Some("steam"));
        assert_eq!(full.display.as_deref(), Some("sway"));
        assert_eq!(full.vm_args.as_deref(), Some("--auto-join --headless"));
        assert_eq!(full.host_args.as_deref(), Some("--auto-host-steam"));
        assert_eq!(full.no_build, Some(true));
        assert_eq!(full.no_deploy, Some(false));
        assert_eq!(full.no_stop_on_failure, Some(true));
        assert_eq!(full.capture_on_failure, Some(true));
        assert_eq!(full.screenshot_backend.as_deref(), Some("vnc"));
        assert_eq!(
            full.visual_validator.as_deref(),
            Some("validator --image $STEAMPIPE_VISUAL_IMAGE")
        );
        assert_eq!(full.filter_pattern.as_deref(), Some("FAIL|PASS"));
        assert_eq!(full.output_file.as_deref(), Some("/tmp/out.txt"));
        assert_eq!(full.chaos_profile.as_deref(), Some("satellite"));
        assert_eq!(full.heartbeat_stall_secs, Some(60));
        assert_eq!(full.output_format.as_deref(), Some("junit"));
        assert_eq!(full.on_complete.as_deref(), Some("echo done"));
        assert_eq!(full.on_failure.as_deref(), Some("echo fail"));
    }

    /// Per-profile `[profile.<name>.env]` table deserialises into the
    /// `TestProfile.env` field — required so the `cluster-ctl` runner can
    /// forward those entries to both peers without the user having to set
    /// any process-level env var.
    #[test]
    fn profile_env_table_deserialize() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [profile.iso]
            [profile.iso.env]
            THESPAN_SESSION_ID = "fixloop-uifull-1v1-lan"
            RUST_LOG = "info"
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let iso = proj.profile.unwrap().get("iso").unwrap().clone();
        let env = iso.env.unwrap();
        assert_eq!(
            env.get("THESPAN_SESSION_ID").map(String::as_str),
            Some("fixloop-uifull-1v1-lan")
        );
        assert_eq!(env.get("RUST_LOG").map(String::as_str), Some("info"));
    }

    #[test]
    fn profile_display_weston_gpu() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [profile.gpu]
            display = "weston-gpu"
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let gpu = proj.profile.unwrap().get("gpu").unwrap().clone();
        assert_eq!(gpu.display.as_deref(), Some("weston-gpu"));
    }

    fn sample_module_config() -> NixosModuleConfig {
        NixosModuleConfig {
            schema_version: 1,
            vm_count: 3,
            bridge: "br-module".into(),
            subnet: "10.44.0".into(),
            prefix: 24,
            host_ip: "10.44.0.254".into(),
            tap_owner: Some("can".into()),
            login_state_dir: PathBuf::from("/srv/steam-logins"),
            guard_data_path: Some(PathBuf::from("/srv/steam-guard/machine_tokens.json")),
            credentials_path: Some(PathBuf::from("/etc/steampipe/credentials.toml")),
            accounts: Vec::new(),
            runners_dir: None,
            login_runners_dir: None,
            ssh_key: None,
            vm_user: None,
        }
    }

    fn fake_ssh_key() -> tempfile::NamedTempFile {
        tempfile::NamedTempFile::new().expect("fake ssh key")
    }

    #[test]
    fn profile_display_sway_gpu() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [profile.gpu]
            display = "sway-gpu"
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let gpu = proj.profile.unwrap().get("gpu").unwrap().clone();
        assert_eq!(gpu.display.as_deref(), Some("sway-gpu"));
    }

    #[test]
    fn chaos_profile_partial_fields() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [chaos.minimal]
            latency = 50
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let minimal = proj.chaos.unwrap().get("minimal").unwrap().clone();
        assert_eq!(minimal.latency, Some(50));
        assert!(minimal.jitter.is_none());
        assert!(minimal.loss.is_none());
        assert!(minimal.rate.is_none());
    }

    #[test]
    fn exit_codes_with_zero() {
        let toml_str = r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"

            [exit_codes]
            0 = "OK"
            255 = "CRASH"
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let codes = proj.exit_codes.unwrap();
        assert_eq!(codes.len(), 2);
        assert_eq!(codes.get("0").unwrap(), "OK");
        assert_eq!(codes.get("255").unwrap(), "CRASH");
    }

    #[test]
    fn from_parts_exit_codes_empty_when_missing() {
        let root = std::env::temp_dir();
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            ..Default::default()
        });
        let config = ClusterConfig::from_parts(&root, proj, 1, None, None, None).unwrap();
        assert!(config.exit_codes.is_empty());
    }

    #[test]
    fn into_state_preserves_new_fields() {
        let root = std::env::temp_dir();
        let mut exit_codes = HashMap::new();
        exit_codes.insert("10".into(), "CUSTOM".into());
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            exit_codes: Some(exit_codes),
            heartbeat_stall_secs: Some(99),
            hooks: Some(HooksConfig {
                on_complete: Some("echo".into()),
                on_failure: Some("alert".into()),
            }),
            ..Default::default()
        });
        let unchecked =
            ClusterConfig::from_parts(&root, proj, 1, None, Some(BackendKind::Local), None)
                .unwrap();
        // Transition to BridgeReady (skip validation for non-microvm)
        let ready = unchecked.validate_or_skip_bridge().unwrap();
        assert_eq!(ready.exit_codes.get(&10).unwrap(), "CUSTOM");
        assert_eq!(ready.heartbeat_stall_secs, 99);
        assert_eq!(ready.hooks.on_complete.as_deref(), Some("echo"));
        assert_eq!(ready.hooks.on_failure.as_deref(), Some("alert"));
    }

    #[test]
    fn default_project_config_all_none() {
        let proj = ProjectConfig::default();
        assert!(proj.profile.is_none());
        assert!(proj.chaos.is_none());
        assert!(proj.exit_codes.is_none());
        assert!(proj.heartbeat_stall_secs.is_none());
        assert!(proj.hooks.is_none());
    }

    // ── resolve_project_path ────────────────────────────────────────────

    #[test]
    fn resolve_project_path_leaves_absolute_alone() {
        assert_eq!(
            resolve_project_path(Path::new("/proj"), Path::new("/abs/path")),
            PathBuf::from("/abs/path")
        );
    }

    #[test]
    fn resolve_project_path_joins_relative() {
        assert_eq!(
            resolve_project_path(Path::new("/proj"), Path::new("rel/path")),
            PathBuf::from("/proj/rel/path")
        );
    }

    // ── validate_scene_name ─────────────────────────────────────────────

    #[test]
    fn validate_scene_name_accepts_alphanumeric() {
        assert!(validate_scene_name("main-menu_01").is_ok());
    }

    #[test]
    fn validate_scene_name_rejects_empty() {
        let err = validate_scene_name("").unwrap_err();
        assert!(err.contains("cannot be empty"), "{err}");
    }

    #[test]
    fn validate_scene_name_rejects_dot() {
        let err = validate_scene_name(".").unwrap_err();
        assert!(err.contains("must match"), "{err}");
    }

    #[test]
    fn validate_scene_name_rejects_dot_dot() {
        let err = validate_scene_name("..").unwrap_err();
        assert!(err.contains("must match"), "{err}");
    }

    #[test]
    fn validate_scene_name_rejects_path_traversal() {
        let err = validate_scene_name("a/../b").unwrap_err();
        assert!(err.contains("must match"), "{err}");
    }

    #[test]
    fn validate_scene_name_rejects_shell_metacharacters() {
        let err = validate_scene_name("foo;rm -rf /").unwrap_err();
        assert!(err.contains("must match"), "{err}");
    }

    #[test]
    fn validate_scene_name_rejects_too_long() {
        let long = "a".repeat(129);
        let err = validate_scene_name(&long).unwrap_err();
        assert!(err.contains("too long"), "{err}");
    }

    #[test]
    fn validate_scene_name_accepts_max_length() {
        let max = "a".repeat(128);
        assert!(validate_scene_name(&max).is_ok());
    }

    // ── vm_defs_for_ids ─────────────────────────────────────────────────

    #[test]
    fn vm_defs_for_ids_rejects_zero() {
        let err = ClusterConfig::<Unchecked>::vm_defs_for_ids("10.0.100", 7, &[0])
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn vm_defs_for_ids_rejects_above_max() {
        let err = ClusterConfig::<Unchecked>::vm_defs_for_ids("10.0.100", 7, &[8])
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn vm_defs_for_ids_accepts_valid_range() {
        let vms = ClusterConfig::<Unchecked>::vm_defs_for_ids("10.0.100", 7, &[1, 3, 5]).unwrap();
        assert_eq!(vms.len(), 3);
        assert_eq!(&*vms[0].name, "vm-1");
        assert_eq!(&*vms[0].ip, "10.0.100.1");
        assert_eq!(&*vms[1].ip, "10.0.100.3");
        assert_eq!(&*vms[2].ip, "10.0.100.5");
    }

    #[test]
    fn vm_defs_for_ids_duplicates_are_not_rejected() {
        // vm_defs_for_ids does not validate uniqueness; duplicates produce
        // identical VmDef entries. This test documents the current behavior.
        let vms = ClusterConfig::<Unchecked>::vm_defs_for_ids("10.0.100", 7, &[1, 1]).unwrap();
        assert_eq!(vms.len(), 2);
        assert_eq!(vms[0].index, vms[1].index);
    }

    // ── from_parts clamping ─────────────────────────────────────────────

    #[test]
    fn from_parts_clamps_requested_vm_count_to_max_vms() {
        let root = std::env::temp_dir();
        let proj = Some(ProjectConfig {
            binary_name: Some("g".into()),
            cargo_package: Some("g".into()),
            vm_user: Some("u".into()),
            remote_dir: Some("/r".into()),
            max_vms: Some(3),
            ..Default::default()
        });
        // Request 7 VMs but max_vms is 3
        let config = ClusterConfig::from_parts(&root, proj, 7, None, None, None).unwrap();
        assert_eq!(config.vms.len(), 3);
        assert_eq!(config.max_vms, 3);
    }

    // ── generate_yh_cluster_config ──────────────────────────────────────

    #[test]
    fn generate_yh_cluster_config_produces_valid_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_str = generate_yh_cluster_config(tmp.path(), Some(3), None, None, None, None, None, None);
        let parsed: toml::Value = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed["vm_count"].as_integer(), Some(3));
        assert!(parsed["project_root"].as_str().unwrap().ends_with(tmp.path().display().to_string().as_str()));
    }

    #[test]
    fn generate_yh_cluster_config_with_all_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let lock_dir = tmp.path().join("locks");
        let key = tmp.path().join("key");
        let creds = tmp.path().join("creds.toml");
        let toml_str = generate_yh_cluster_config(
            tmp.path(),
            Some(7),
            Some("tournament"),
            Some(BackendKind::Docker),
            Some(&state_dir),
            Some(&lock_dir),
            Some(&key),
            Some(&creds),
        );
        let parsed: toml::Value = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed["vm_count"].as_integer(), Some(7));
        assert_eq!(parsed["cluster"].as_str(), Some("tournament"));
        assert_eq!(parsed["backend"].as_str(), Some("docker"));
        assert!(parsed["state_dir"].as_str().unwrap().ends_with("state"));
        assert!(parsed["lock_dir"].as_str().unwrap().ends_with("locks"));
        assert!(parsed["ssh_key"].as_str().unwrap().ends_with("key"));
        assert!(parsed["project_root"].as_str().is_some());
    }

    // ── Visual config validation ────────────────────────────────────────

    #[test]
    fn visual_validator_kind_inferred_from_visual_block() {
        let proj: ProjectConfig = toml::from_str(r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"
            [visual]
            golden_dir = "golden"
        "#).unwrap();
        assert_eq!(proj.effective_validator_kind(None), ValidatorKind::Golden);
    }

    #[test]
    fn visual_validator_kind_shell_requires_validator() {
        let proj: ProjectConfig = toml::from_str(r#"
            binary_name = "g"
            cargo_package = "g"
            vm_user = "u"
            remote_dir = "/r"
            [profile.p]
            validator_kind = "shell"
        "#).unwrap();
        let profile = proj.profile.as_ref().unwrap().get("p");
        assert_eq!(proj.effective_validator_kind(profile), ValidatorKind::Shell);
    }
}
