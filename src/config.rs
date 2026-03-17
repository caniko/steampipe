use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ssh::SshClient;

// ── Newtypes for domain-specific string primitives ──

/// A VM name (e.g. "vm-3"). Prevents accidental use as an IP or MAC.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VmName(pub String);

/// An IPv4 address string (e.g. "10.0.100.3").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpAddr(pub String);

macro_rules! impl_str_newtype {
    ($ty:ident) => {
        impl Deref for $ty {
            type Target = str;
            fn deref(&self) -> &str { &self.0 }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str { &self.0 }
        }
        impl AsRef<std::path::Path> for $ty {
            fn as_ref(&self) -> &std::path::Path { self.0.as_ref() }
        }
        impl std::borrow::Borrow<str> for $ty {
            fn borrow(&self) -> &str { &self.0 }
        }
        impl PartialEq<str> for $ty {
            fn eq(&self, other: &str) -> bool { self.0 == other }
        }
        impl PartialEq<&str> for $ty {
            fn eq(&self, other: &&str) -> bool { self.0 == *other }
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
    pub network: Option<NetworkFileConfig>,
    pub cluster: Option<ClusterSection>,
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
# All values shown are defaults. Uncomment and modify as needed.

# The game binary name (as built by cargo)
# binary_name = "my-game"

# The cargo package to build (for `cargo build -p`)
# cargo_package = "my-game"

# SSH user on VMs
# vm_user = "player"

# Directory on VMs where the binary + assets are deployed
# remote_dir = "/home/player/game"

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
"#.to_string()
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
    _state: PhantomData<S>,
}

impl ClusterConfig<Unchecked> {
    /// Build config with defaults, optionally merged with steampipe.toml.
    pub fn new(project_root: &Path, vm_count: u8, cluster_name: Option<&str>) -> Self {
        let proj = load_project_config(project_root);

        let cluster_name = cluster_name
            .map(String::from)
            .or_else(|| proj.as_ref().and_then(|p| p.cluster.as_ref()?.name.clone()))
            .unwrap_or_else(|| "default".into());

        let base_state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .expect("no home directory")
                    .join(".local/state")
            })
            .join("steampipe");

        let state_dir = if cluster_name == "default" {
            base_state_dir
        } else {
            base_state_dir.join(&cluster_name)
        };

        let max_vms = proj.as_ref().and_then(|p| p.max_vms).unwrap_or(7);
        let vm_count = vm_count.clamp(1, max_vms);
        let subnet = proj.as_ref()
            .and_then(|p| p.network.as_ref()?.subnet.clone())
            .unwrap_or_else(|| "10.0.100".into());

        let vms = (1..=vm_count)
            .map(|i| VmDef {
                name: VmName(format!("vm-{i}")),
                ip: IpAddr(format!("{subnet}.{i}")),
                index: i,
            })
            .collect();

        Self {
            bridge: proj.as_ref().and_then(|p| p.network.as_ref()?.bridge.clone()).unwrap_or_else(|| "br-cluster".into()),
            subnet,
            prefix: proj.as_ref().and_then(|p| p.network.as_ref()?.prefix).unwrap_or(24),
            host_ip: proj.as_ref().and_then(|p| p.network.as_ref()?.host_ip.clone()).unwrap_or_else(|| "10.0.100.254".into()),
            vm_user: proj.as_ref().and_then(|p| p.vm_user.clone()).unwrap_or_else(|| "chessbender".into()),
            remote_dir: proj.as_ref().and_then(|p| p.remote_dir.clone()).unwrap_or_else(|| "/home/chessbender/chessbender".into()),
            binary_name: proj.as_ref().and_then(|p| p.binary_name.clone()).unwrap_or_else(|| "chessbender".into()),
            cargo_package: proj.as_ref().and_then(|p| p.cargo_package.clone()).unwrap_or_else(|| "chessbender".into()),
            ssh_key: proj.as_ref()
                .and_then(|p| p.ssh_key.clone())
                .map(|k| project_root.join(k))
                .unwrap_or_else(|| project_root.join("nix/test-cluster/cluster_key")),
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
            _state: PhantomData,
        }
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
}

/// Methods available on any config state (both Unchecked and BridgeReady).
impl<S> ClusterConfig<S> {
    /// Create an `SshClient` from this config's key and user.
    pub fn ssh_client(&self) -> SshClient {
        SshClient::new(&self.ssh_key, &self.vm_user)
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
            vms: self.vms,
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

    /// Parse a target string into a list of VMs.
    pub fn resolve_targets(&self, target: Option<&str>, continue_from: bool) -> anyhow::Result<Vec<VmDef>> {
        match target {
            None | Some("all") => Ok(self.vms.clone()),
            Some(t) => {
                let vm = self
                    .find_vm(t)
                    .ok_or_else(|| {
                        let max = self.vms.len();
                        anyhow::anyhow!("Unknown VM: {t} (expected vm-1..vm-{max} or 1..{max})")
                    })?;
                if continue_from {
                    Ok(self.vms.iter().filter(|v| v.index >= vm.index).cloned().collect())
                } else {
                    Ok(vec![vm.clone()])
                }
            }
        }
    }

}

/// Auto-detect project root by walking up from CWD looking for Cargo.toml with workspace.
pub fn detect_project_root() -> anyhow::Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents = std::fs::read_to_string(&cargo_toml)?;
            if contents.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            anyhow::bail!("Could not find workspace root (no Cargo.toml with [workspace] found)");
        }
    }
}
