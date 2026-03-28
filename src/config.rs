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
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClusterSection {
    pub name: Option<String>,
    /// Absolute path override for state directory (share cluster state across projects).
    pub state_dir: Option<String>,
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
"#.to_string()
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
    _state: PhantomData<S>,
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
            _state: PhantomData,
        })
    }

    /// Check that the network bridge exists, transitioning to `BridgeReady` on success.
    pub fn validate_bridge(self) -> anyhow::Result<ClusterConfig<BridgeReady>> {
        if !std::process::Command::new("ip")
            .args(["link", "show", &self.bridge])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?
            .success()
        {
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
}
