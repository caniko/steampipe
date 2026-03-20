use std::path::Path;

use crate::backend::Backend;
use crate::config::{ClusterConfig, VmDef, par_each_vm};

/// Run `cargo build --release -p <package>`.
pub async fn build_release(project_root: &Path, cargo_package: &str) -> anyhow::Result<()> {
    println!("==> Building release binary...");
    let status = tokio::process::Command::new("cargo")
        .args(["build", "--release", "-p", cargo_package])
        .current_dir(project_root)
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("cargo build failed");
    }
    Ok(())
}

/// Upload binary + steam lib + appid + assets to a set of instances in parallel.
/// Returns the names of instances that failed.
pub async fn deploy_to_vms<S>(
    backend: &Backend,
    vms: &[VmDef],
    config: &ClusterConfig<S>,
    project_root: &Path,
) -> anyhow::Result<Vec<String>> {
    let binary = project_root.join(format!("target/release/{}", config.binary_name));
    let assets = project_root.join(&config.assets_dir);
    let steam_lib = config.steam_api_lib.clone();
    let appid_file = project_root.join(&config.steam_appid_file);
    let remote_dir = config.remote_dir.clone();
    let binary_name = config.binary_name.clone();

    if !binary.exists() {
        anyhow::bail!("Binary not found: {}", binary.display());
    }
    if !assets.exists() {
        anyhow::bail!("Assets directory not found: {}", assets.display());
    }

    let results = par_each_vm(vms, |vm| {
        let backend = backend.clone();
        let binary = binary.clone();
        let steam_lib = steam_lib.clone();
        let appid_file = appid_file.clone();
        let assets = assets.clone();
        let remote_dir = remote_dir.clone();
        let binary_name = binary_name.clone();
        async move {
            backend.run_cmd(&vm.ip, &format!("mkdir -p {remote_dir}")).await;
            let sources: [&Path; 4] = [
                binary.as_path(),
                steam_lib.as_path(),
                appid_file.as_path(),
                assets.as_path(),
            ];
            backend.upload(&sources, &vm.ip, &format!("{remote_dir}/")).await?;
            backend.run_cmd(&vm.ip, &format!("chmod +x {remote_dir}/{binary_name}"))
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

/// Verify deployed files via SHA-256 checksums.
pub async fn verify<S>(
    config: &ClusterConfig<S>,
    project_root: &Path,
) -> anyhow::Result<()> {
    use sha2::{Sha256, Digest};

    let backend = &config.backend;
    let binary_path = project_root.join(format!("target/release/{}", config.binary_name));

    // Compute local checksum of the binary
    let local_bytes = std::fs::read(&binary_path)?;
    let local_hash = format!("{:x}", Sha256::digest(&local_bytes));

    println!("==> Verifying deployments (binary SHA-256: {}...)", &local_hash[..12]);

    let remote_dir = config.remote_dir.clone();
    let binary_name = config.binary_name.clone();

    let results = par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        let remote_dir = remote_dir.clone();
        let binary_name = binary_name.clone();
        let local_hash = local_hash.clone();
        async move {
            let cmd = format!("sha256sum {remote_dir}/{binary_name} 2>/dev/null | cut -d' ' -f1");
            let result = backend.run_cmd(&vm.ip, &cmd).await;
            let remote_hash = result.stdout.trim().to_string();
            let matches = remote_hash == local_hash;
            (vm.name, matches, remote_hash)
        }
    })
    .await?;

    let mut all_ok = true;
    for (name, matches, remote_hash) in &results {
        if *matches {
            println!("  {name}: OK");
        } else {
            all_ok = false;
            if remote_hash.is_empty() {
                eprintln!("  {name}: MISSING (binary not found on VM)");
            } else {
                eprintln!("  {name}: MISMATCH (remote: {}...)", &remote_hash[..12.min(remote_hash.len())]);
            }
        }
    }

    if all_ok {
        println!("==> All VMs verified");
    } else {
        anyhow::bail!("Verification failed — re-run deploy");
    }
    Ok(())
}

/// Run the `deploy` subcommand: build and upload binary + assets to all instances.
pub async fn run<S>(config: &ClusterConfig<S>, project_root: &Path, no_build: bool, do_verify: bool) -> anyhow::Result<()> {
    if !no_build {
        build_release(project_root, &config.cargo_package).await?;
    }

    if !config.steam_api_lib.exists() {
        anyhow::bail!("Steam API lib not found: {}", config.steam_api_lib.display());
    }

    let backend = &config.backend;
    println!("==> Deploying to {} VMs...", config.vms.len());

    let failed = deploy_to_vms(backend, &config.vms, config, project_root).await?;
    if failed.is_empty() {
        println!("==> All VMs deployed");
    } else {
        anyhow::bail!("Deploy failed for {} VMs", failed.len());
    }

    if do_verify {
        verify(config, project_root).await?;
    }

    Ok(())
}
