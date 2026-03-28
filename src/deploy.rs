//! Binary + asset deployment to cluster VMs with content-addressable caching.

use std::path::Path;

use sha2::{Digest, Sha256};

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
        anyhow::bail!(
            "cargo build --release -p {cargo_package} failed (exit {})",
            status
        );
    }
    Ok(())
}

/// Compute the SHA-256 hash of a file, returned as a lowercase hex string.
pub fn compute_binary_hash(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    let hash = Sha256::digest(&bytes);
    Ok(hash.iter().map(|b| format!("{b:02x}")).collect())
}

/// Deploy a binary to a VM's content-addressable pool, skipping upload if the
/// hash already exists. Updates a symlink so the game launch path is unchanged.
async fn pool_deploy_binary(
    backend: &Backend,
    ip: &str,
    binary_path: &Path,
    hash: &str,
    remote_dir: &str,
    binary_name: &str,
) -> anyhow::Result<bool> {
    let pool_dir = format!("{remote_dir}/.pool/blobs");
    let blob_path = format!("{pool_dir}/{hash}");

    backend.run_cmd(ip, &format!("mkdir -p {pool_dir}")).await;

    // Check if this hash already exists in the pool
    let check = backend
        .run_cmd(ip, &format!("test -f {blob_path} && echo HIT"))
        .await;
    let cached = check.stdout.trim() == "HIT";

    if !cached {
        backend
            .upload(&[binary_path], ip, &format!("{pool_dir}/"))
            .await?;
        // Rename from the original filename to the hash
        let uploaded_name = binary_path
            .file_name()
            .expect("binary_path validated to exist, so it has a file name")
            .to_string_lossy();
        backend
            .run_cmd(ip, &format!("mv {pool_dir}/{uploaded_name} {blob_path}"))
            .await;
        backend.run_cmd(ip, &format!("chmod +x {blob_path}")).await;
    }

    // Atomic symlink update: binary_name -> .pool/blobs/{hash}
    backend
        .run_cmd(
            ip,
            &format!("ln -sfn .pool/blobs/{hash} {remote_dir}/{binary_name}"),
        )
        .await;

    Ok(cached)
}

/// Upload binary + steam lib + appid + assets to a set of instances in parallel.
/// Uses a content-addressable pool to skip binary uploads when the hash matches.
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

    // Hash the binary once before the parallel fan-out
    let hash = compute_binary_hash(&binary)?;
    println!("  binary hash: {}...", &hash[..12]);

    let results = par_each_vm(vms, |vm| {
        let backend = backend.clone();
        let binary = binary.clone();
        let steam_lib = steam_lib.clone();
        let appid_file = appid_file.clone();
        let assets = assets.clone();
        let remote_dir = remote_dir.clone();
        let binary_name = binary_name.clone();
        let hash = hash.clone();
        async move {
            backend
                .run_cmd(&vm.ip, &format!("mkdir -p {remote_dir}"))
                .await;

            // Pool-aware binary deploy
            let cached =
                pool_deploy_binary(&backend, &vm.ip, &binary, &hash, &remote_dir, &binary_name)
                    .await?;

            // Upload remaining assets normally
            let sources: [&Path; 3] = [steam_lib.as_path(), appid_file.as_path(), assets.as_path()];
            backend
                .upload(&sources, &vm.ip, &format!("{remote_dir}/"))
                .await?;

            anyhow::Ok((vm.name, cached))
        }
    })
    .await?;

    let mut failed = Vec::new();
    for result in results {
        match result {
            Ok((name, cached)) => {
                let status = if cached {
                    "deployed (binary cached)"
                } else {
                    "deployed"
                };
                println!("  {name}: {status}");
            }
            Err(e) => {
                eprintln!("  FAILED: {e}");
                failed.push(e.to_string());
            }
        }
    }
    Ok(failed)
}

/// Verify deployed files via SHA-256 checksums.
pub async fn verify<S>(config: &ClusterConfig<S>, project_root: &Path) -> anyhow::Result<()> {
    let backend = &config.backend;
    let binary_path = project_root.join(format!("target/release/{}", config.binary_name));

    let local_hash = compute_binary_hash(&binary_path)?;

    println!(
        "==> Verifying deployments (binary SHA-256: {}...)",
        &local_hash[..12]
    );

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
                eprintln!(
                    "  {name}: MISMATCH (remote: {}...)",
                    &remote_hash[..12.min(remote_hash.len())]
                );
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
pub async fn run<S>(
    config: &ClusterConfig<S>,
    project_root: &Path,
    no_build: bool,
    do_verify: bool,
) -> anyhow::Result<()> {
    if !no_build {
        build_release(project_root, &config.cargo_package).await?;
    }

    if !config.steam_api_lib.exists() {
        anyhow::bail!(
            "Steam API lib not found: {}",
            config.steam_api_lib.display()
        );
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
