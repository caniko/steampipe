use std::path::{Path, PathBuf};

use crate::config::{ClusterConfig, VmDef, par_each_vm};
use crate::ssh::SshClient;

/// Run `cargo build --release -p chessbender`.
pub async fn build_release(project_root: &Path) -> anyhow::Result<()> {
    println!("==> Building release binary...");
    let status = tokio::process::Command::new("cargo")
        .args(["build", "--release", "-p", "chessbender"])
        .current_dir(project_root)
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("cargo build failed");
    }
    Ok(())
}

/// Rsync binary + steam lib + appid + assets to a set of VMs in parallel.
/// Returns the names of VMs that failed.
pub async fn deploy_to_vms(
    ssh: &SshClient,
    vms: &[VmDef],
    config: &ClusterConfig,
    project_root: &Path,
) -> anyhow::Result<Vec<String>> {
    let binary = project_root.join("target/release/chessbender");
    let assets = project_root.join("assets");
    let steam_lib = config.steam_api_lib.clone();
    let appid_file = project_root.join(&config.steam_appid_file);
    let remote_dir = config.remote_dir.clone();

    if !binary.exists() {
        anyhow::bail!("Binary not found: {}", binary.display());
    }
    if !assets.exists() {
        anyhow::bail!("Assets directory not found: {}", assets.display());
    }

    let results = par_each_vm(vms, |vm| {
        let ssh = ssh.clone();
        let binary = binary.clone();
        let steam_lib = steam_lib.clone();
        let appid_file = appid_file.clone();
        let assets = assets.clone();
        let remote_dir = remote_dir.clone();
        async move {
            ssh.run(&vm.ip, &format!("mkdir -p {remote_dir}")).await;
            let sources: Vec<&Path> = vec![
                binary.as_path(),
                steam_lib.as_path(),
                appid_file.as_path(),
                assets.as_path(),
            ];
            ssh.rsync(&sources, &vm.ip, &format!("{remote_dir}/")).await?;
            ssh.run(&vm.ip, &format!("chmod +x {remote_dir}/chessbender"))
                .await;
            anyhow::Ok(vm.name)
        }
    })
    .await?;

    let mut failed = Vec::new();
    for result in results {
        match result {
            Ok(name) => println!("  {name}: deployed"),
            Err(e) => {
                eprintln!("  FAILED: {e}");
                failed.push(e.to_string());
            }
        }
    }
    Ok(failed)
}

/// Run the `deploy` subcommand: build and rsync binary + assets to all VMs.
pub async fn run(config: &ClusterConfig, project_root: &Path, no_build: bool) -> anyhow::Result<()> {
    if !no_build {
        build_release(project_root).await?;
    }

    if !config.steam_api_lib.exists() {
        anyhow::bail!("Steam API lib not found: {}", config.steam_api_lib.display());
    }

    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);
    println!("==> Deploying to {} VMs...", config.vms.len());

    let failed = deploy_to_vms(&ssh, &config.vms, config, project_root).await?;
    if failed.is_empty() {
        println!("==> All VMs deployed");
    } else {
        anyhow::bail!("Deploy failed for {} VMs", failed.len());
    }
    Ok(())
}
