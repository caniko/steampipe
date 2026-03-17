use std::path::{Path, PathBuf};

use crate::config::{ClusterConfig, VmDef, par_each_vm};
use crate::ssh::SshClient;

/// Capture a screenshot from a VM using grim (Wayland screenshot tool).
pub async fn screenshot_vm(ssh: &SshClient, vm: &VmDef, output_dir: &Path) -> anyhow::Result<PathBuf> {
    let remote_path = "/tmp/screenshot.png";
    let cmd = format!(
        "export XDG_RUNTIME_DIR=/tmp/runtime-$(whoami); \
         export WAYLAND_DISPLAY=wayland-1; \
         grim {remote_path} 2>/dev/null && echo OK || echo FAIL"
    );
    let result = ssh.run(&vm.ip, &cmd).await;
    if !result.stdout.trim().contains("OK") {
        anyhow::bail!("{}: screenshot failed (is weston/sway running?)", vm.name);
    }

    // Download via rsync
    let local_path = output_dir.join(format!("{}.png", vm.name));
    let sources = [Path::new(remote_path)];
    ssh.rsync(&sources, &vm.ip, local_path.to_str().unwrap_or(".")).await?;

    Ok(local_path)
}

/// Capture screenshots from all VMs in parallel.
pub async fn screenshot_all<S>(
    config: &ClusterConfig<S>,
    output_dir: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(output_dir)?;
    let ssh = config.ssh_client();

    println!("==> Capturing screenshots...");

    let results = par_each_vm(&config.vms, |vm| {
        let ssh = ssh.clone();
        let output_dir = output_dir.to_owned();
        async move {
            match screenshot_vm(&ssh, &vm, &output_dir).await {
                Ok(path) => (vm.name, Ok(path)),
                Err(e) => (vm.name, Err(e)),
            }
        }
    })
    .await?;

    for (name, result) in results {
        match result {
            Ok(path) => println!("  {name}: {}", path.display()),
            Err(e) => eprintln!("  {name}: {e}"),
        }
    }
    println!("==> Done");
    Ok(())
}

/// Capture screenshots on test failure. Called from test.rs.
pub async fn capture_on_failure<S>(
    config: &ClusterConfig<S>,
    output_dir: &Path,
    run_num: u32,
) {
    let screenshot_dir = output_dir.join(format!("screenshots/run-{run_num}"));
    if let Err(e) = screenshot_all(config, &screenshot_dir).await {
        eprintln!("  Screenshot capture failed: {e}");
    }
}
