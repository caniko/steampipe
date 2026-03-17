use std::path::{Path, PathBuf};

/// A single VM in the cluster.
#[derive(Debug, Clone)]
pub struct VmDef {
    pub name: String,
    pub ip: String,
    pub mac: String,
    pub index: u8,
}

/// Cluster-wide configuration. Defaults match `nix/test-cluster/default.nix`.
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub bridge: String,
    pub subnet: String,
    pub prefix: u8,
    pub host_ip: String,
    pub vm_user: String,
    pub remote_dir: String,
    pub ssh_key: PathBuf,
    pub udp_port: u16,
    pub steam_api_lib: PathBuf,
    pub steam_appid_file: String,
    pub state_dir: PathBuf,
    pub vms: Vec<VmDef>,
}

impl ClusterConfig {
    /// Build config with defaults, using the given project root for relative paths.
    /// `vm_count` determines how many VMs are in the cluster (clamped to 1..=7).
    pub fn new(project_root: &Path, vm_count: u8) -> Self {
        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .expect("no home directory")
                    .join(".local/state")
            })
            .join("chessbender-cluster");

        let vm_count = vm_count.clamp(1, 7);
        let vms = (1..=vm_count)
            .map(|i| VmDef {
                name: format!("vm-{i}"),
                ip: format!("10.0.100.{i}"),
                mac: format!("52:54:00:cb:00:0{i}"),
                index: i,
            })
            .collect();

        Self {
            bridge: "br-cluster".into(),
            subnet: "10.0.100".into(),
            prefix: 24,
            host_ip: "10.0.100.254".into(),
            vm_user: "chessbender".into(),
            remote_dir: "/home/chessbender/chessbender".into(),
            ssh_key: project_root.join("nix/test-cluster/cluster_key"),
            udp_port: 27100,
            steam_api_lib: project_root.join(
                "vendor/steamworks-rs/steamworks-sys/lib/steam/redistributable_bin/linux64/libsteam_api.so",
            ),
            steam_appid_file: "steam_appid.txt".into(),
            state_dir,
            vms,
        }
    }

    /// Look up a VM by name ("vm-3") or index ("3"). Returns None if not found.
    pub fn find_vm(&self, target: &str) -> Option<&VmDef> {
        // Try as plain number first
        if let Ok(i) = target.parse::<u8>() {
            return self.vms.iter().find(|vm| vm.index == i);
        }
        self.vms.iter().find(|vm| vm.name == target)
    }

    /// Parse a target string into a list of VMs.
    /// Accepts: "all", "3", "vm-3", or with continue_from: "3" means vm-3 through vm-7.
    pub fn resolve_targets(&self, target: Option<&str>, continue_from: bool) -> anyhow::Result<Vec<&VmDef>> {
        match target {
            None | Some("all") => Ok(self.vms.iter().collect()),
            Some(t) => {
                let vm = self
                    .find_vm(t)
                    .ok_or_else(|| {
                        let max = self.vms.len();
                        anyhow::anyhow!("Unknown VM: {t} (expected vm-1..vm-{max} or 1..{max})")
                    })?;
                if continue_from {
                    Ok(self.vms.iter().filter(|v| v.index >= vm.index).collect())
                } else {
                    Ok(vec![vm])
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
