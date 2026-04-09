//! Cluster configuration: VM definitions, project config, and typestate bridge validation.
//!
//! The main type is [`ClusterConfig`] which holds all settings for a cluster session.
//! It uses a typestate pattern (`Unchecked` → `BridgeReady`) to enforce that the
//! network bridge is validated before starting VMs.

use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use std::collections::HashMap;

use crate::backend::Backend;
use clap::ValueEnum;

/// Which backend to use for running test instances.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ValueEnum)]
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
#[derive(Debug, Clone, Default, Deserialize)]
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
    /// Exit code → label mapping (keys are string integers, e.g. "10" = "PHASE_WATCHDOG").
    pub exit_codes: Option<HashMap<String, String>>,
    /// Heartbeat stall threshold in seconds (default: 30).
    pub heartbeat_stall_secs: Option<u64>,
    /// Notification hooks.
    pub hooks: Option<HooksConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DockerConfig {
    pub image: Option<String>,
    pub network: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub work_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClusterSection {
    pub name: Option<String>,
    /// Absolute path override for state directory (share cluster state across projects).
    pub state_dir: Option<String>,
}

/// A named test profile with all test parameters optional (CLI overrides profile).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TestProfile {
    pub players: Option<u8>,
    pub max_runs: Option<u32>,
    pub timeout: Option<u64>,
    pub shutdown_timeout: Option<u64>,
    pub network: Option<String>,
    pub display: Option<String>,
    pub vm_args: Option<String>,
    pub host_args: Option<String>,
    pub no_build: Option<bool>,
    pub no_deploy: Option<bool>,
    pub no_stop_on_failure: Option<bool>,
    pub capture_on_failure: Option<bool>,
    pub filter_pattern: Option<String>,
    pub output_file: Option<String>,
    pub chaos_profile: Option<String>,
    pub heartbeat_stall_secs: Option<u64>,
    pub output_format: Option<String>,
    pub on_complete: Option<String>,
    pub on_failure: Option<String>,
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
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[allow(dead_code)]
pub struct ChaosSchedule {
    pub delay_secs: Option<u64>,
    pub profiles: Vec<String>,
    pub interval_secs: Option<u64>,
}

/// Notification hooks executed after test runs.
#[derive(Debug, Clone, Default, Deserialize)]
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
# timeout = 120
# network = "lan"

# [profile.stress]
# players = 8
# max_runs = 100
# timeout = 600

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
        ssh_key: cli_ssh_key
            .map(|p| p.display().to_string())
            .or_else(|| {
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
#[derive(Debug, Clone)]
pub struct Unchecked;
/// Bridge existence has been verified.
#[derive(Debug, Clone)]
pub struct BridgeReady;

/// Cluster-wide configuration.
///
/// The type parameter `S` tracks whether the network bridge has been validated:
/// - `ClusterConfig<Unchecked>` (default from `new()`) — bridge not yet verified
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
    /// Build config from steampipe.toml, requiring project-specific fields.
    pub fn new(
        project_root: &Path,
        vm_count: u8,
        cluster_name: Option<&str>,
        backend_override: Option<BackendKind>,
    ) -> anyhow::Result<Self> {
        let proj = load_project_config(project_root);
        Self::from_parts(project_root, proj, vm_count, cluster_name, backend_override, None)
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
        let cluster_name = cluster_name
            .map(String::from)
            .or_else(|| proj.as_ref().and_then(|p| p.cluster.as_ref()?.name.clone()))
            .unwrap_or_else(|| "default".into());

        let base_state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .map(|d| d.join(".local/state"))
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
            })
            .join("steampipe");

        let state_dir = proj
            .as_ref()
            .and_then(|p| p.cluster.as_ref()?.state_dir.clone())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if cluster_name == "default" {
                    base_state_dir
                } else {
                    base_state_dir.join(&cluster_name)
                }
            });

        let max_vms = proj.as_ref().and_then(|p| p.max_vms).unwrap_or(7);
        let vm_count = vm_count.clamp(1, max_vms);
        let subnet = proj
            .as_ref()
            .and_then(|p| p.network.as_ref()?.subnet.clone())
            .unwrap_or_else(|| "10.0.100".into());

        let vms = (1..=vm_count)
            .map(|i| VmDef {
                name: VmName(format!("vm-{i}")),
                ip: IpAddr(format!("{subnet}.{i}")),
                index: i,
            })
            .collect();

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
            .unwrap_or_else(crate::lease::default_lock_dir);

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

    /// Look up a VM by name ("vm-3") or index ("3").
    pub fn find_vm(&self, target: &str) -> Option<&VmDef> {
        if let Ok(i) = target.parse::<u8>() {
            return self.vms.iter().find(|vm| vm.index == i);
        }
        self.vms.iter().find(|vm| vm.name == target)
    }

    /// Return VMs currently leased by this cluster, or fall back to `self.vms`
    /// if no leases are held (backward compat for setups without leasing).
    pub fn leased_vms(&self) -> Vec<VmDef> {
        let max_vms = self.vms.iter().map(|v| v.index).max().unwrap_or(7);
        let ids = crate::lease::list_cluster_vms(&self.cluster_name, max_vms, &self.lock_dir);
        if ids.is_empty() {
            return self.vms.clone();
        }
        ids.iter()
            .map(|&id| VmDef {
                name: VmName(format!("vm-{id}")),
                ip: IpAddr(format!("{}.{id}", self.subnet)),
                index: id,
            })
            .collect()
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
                    let max = self.vms.len();
                    anyhow::anyhow!("Unknown VM: {t} (expected vm-1..vm-{max} or 1..{max})")
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

#[cfg(test)]
impl ClusterConfig<Unchecked> {
    /// Build a minimal config for testing. No filesystem access.
    pub fn for_test(vm_count: u8) -> Self {
        let subnet = "10.0.100";
        let vms = (1..=vm_count)
            .map(|i| VmDef {
                name: VmName(format!("vm-{i}")),
                ip: IpAddr(format!("{subnet}.{i}")),
                index: i,
            })
            .collect();

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
        "#;
        let proj: ProjectConfig = toml::from_str(toml_str).unwrap();
        let profiles = proj.profile.unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles.get("smoke").unwrap().max_runs, Some(5));
        assert_eq!(profiles.get("stress").unwrap().players, Some(8));
        assert_eq!(profiles.get("stress").unwrap().timeout, Some(600));
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
        assert!(template.contains("[exit_codes]"), "template missing exit_codes section");
        assert!(template.contains("[profile.smoke]"), "template missing profile section");
        assert!(template.contains("[chaos.unstable-wifi]"), "template missing chaos section");
        assert!(template.contains("[hooks]"), "template missing hooks section");
        assert!(template.contains("heartbeat_stall_secs"), "template missing heartbeat_stall_secs");
        assert!(template.contains("on_complete"), "template missing on_complete hook");
        assert!(template.contains("on_failure"), "template missing on_failure hook");
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
            shutdown_timeout = 10
            network = "steam"
            display = "sway"
            vm_args = "--auto-join --headless"
            host_args = "--auto-host-steam"
            no_build = true
            no_deploy = false
            no_stop_on_failure = true
            capture_on_failure = true
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
        assert_eq!(full.shutdown_timeout, Some(10));
        assert_eq!(full.network.as_deref(), Some("steam"));
        assert_eq!(full.display.as_deref(), Some("sway"));
        assert_eq!(full.vm_args.as_deref(), Some("--auto-join --headless"));
        assert_eq!(full.host_args.as_deref(), Some("--auto-host-steam"));
        assert_eq!(full.no_build, Some(true));
        assert_eq!(full.no_deploy, Some(false));
        assert_eq!(full.no_stop_on_failure, Some(true));
        assert_eq!(full.capture_on_failure, Some(true));
        assert_eq!(full.filter_pattern.as_deref(), Some("FAIL|PASS"));
        assert_eq!(full.output_file.as_deref(), Some("/tmp/out.txt"));
        assert_eq!(full.chaos_profile.as_deref(), Some("satellite"));
        assert_eq!(full.heartbeat_stall_secs, Some(60));
        assert_eq!(full.output_format.as_deref(), Some("junit"));
        assert_eq!(full.on_complete.as_deref(), Some("echo done"));
        assert_eq!(full.on_failure.as_deref(), Some("echo fail"));
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
}
