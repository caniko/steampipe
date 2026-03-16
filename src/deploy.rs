use std::path::Path;

use crate::config::ClusterConfig;
use crate::ssh::SshClient;

/// Run the `deploy` subcommand: build and rsync binary + assets to all VMs.
pub async fn run(config: &ClusterConfig, project_root: &Path, no_build: bool) -> anyhow::Result<()> {
    let binary = project_root.join("target/release/chessbender");
    let assets = project_root.join("assets");

    if !no_build {
        println!("==> Building release binary...");
        let status = tokio::process::Command::new("cargo")
            .args(["build", "--release", "-p", "chessbender"])
            .current_dir(project_root)
            .status()
            .await?;
        if !status.success() {
            anyhow::bail!("cargo build failed");
        }
    }

    // Validate required files
    if !binary.exists() {
        anyhow::bail!("Binary not found: {}", binary.display());
    }
    if !config.steam_api_lib.exists() {
        anyhow::bail!("Steam API lib not found: {}", config.steam_api_lib.display());
    }
    if !assets.exists() {
        anyhow::bail!("Assets directory not found: {}", assets.display());
    }

    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);

    println!("==> Deploying to {} VMs...", config.vms.len());
    let mut tasks = tokio::task::JoinSet::new();
    for vm in &config.vms {
        let ssh = ssh.clone();
        let name = vm.name.clone();
        let ip = vm.ip.clone();
        let remote_dir = config.remote_dir.clone();
        let binary = binary.clone();
        let steam_lib = config.steam_api_lib.clone();
        let assets = assets.clone();
        let appid_file = project_root.join(&config.steam_appid_file);

        tasks.spawn(async move {
            // Ensure remote directory exists
            ssh.run(&ip, &format!("mkdir -p {remote_dir}")).await;

            // rsync binary, steam lib, appid, and assets
            let sources: Vec<&Path> = vec![
                binary.as_path(),
                steam_lib.as_path(),
                appid_file.as_path(),
                assets.as_path(),
            ];
            ssh.rsync(&sources, &ip, &format!("{remote_dir}/")).await?;

            // Make binary executable
            ssh.run(&ip, &format!("chmod +x {remote_dir}/chessbender")).await;

            anyhow::Ok(name)
        });
    }

    let mut failed = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result? {
            Ok(name) => println!("  {name}: deployed"),
            Err(e) => {
                eprintln!("  FAILED: {e}");
                failed.push(e.to_string());
            }
        }
    }

    if failed.is_empty() {
        println!("==> All VMs deployed");
    } else {
        anyhow::bail!("Deploy failed for {} VMs", failed.len());
    }
    Ok(())
}
