use crate::core::backend::Backend;
use crate::core::config::{ClusterConfig, VmDef, par_each_vm};
use crate::game::steam;
use crate::ui::cli::DisplayMode;

/// Kill game process on a set of instances in parallel.
pub async fn kill_games(backend: &Backend, vms: &[VmDef], binary_name: &str) {
    let results = par_each_vm(vms, |vm| {
        let backend = backend.clone();
        let binary_name = binary_name.to_owned();
        async move {
            backend
                .run_cmd(
                    &vm.ip,
                    &format!("pkill -x {binary_name} 2>/dev/null || true"),
                )
                .await;
        }
    })
    .await;
    let _ = results; // JoinErrors are non-fatal here
}

/// Run the `stop-game` subcommand: kill game on all instances.
pub async fn stop_game<S>(config: &ClusterConfig<S>, kill_steam: bool) -> anyhow::Result<()> {
    println!("==> Stopping game on all VMs...");
    kill_games(&config.backend, &config.vms, &config.binary_name).await;
    if kill_steam {
        println!("==> Stopping Steam and Weston...");
        let backend = config.backend.clone();
        let vm_user = config.vm_user.clone();
        par_each_vm(&config.vms, |vm| {
            let backend = backend.clone();
            let vm_user = vm_user.clone();
            async move {
                let _ = steam::graceful_stop_steam(&backend, &vm.ip, &vm_user).await;
                backend
                    .run_cmd(
                        &vm.ip,
                        "pkill -x weston 2>/dev/null; pkill -x sway 2>/dev/null; true",
                    )
                    .await;
            }
        })
        .await?;
    }
    println!("==> Done");
    Ok(())
}

/// Run the `run` subcommand: start compositor + Steam + game on all instances.
pub async fn start_game<S>(
    config: &ClusterConfig<S>,
    display: DisplayMode,
    extra_args: &[String],
) -> anyhow::Result<()> {
    // GPU preflight check
    if display.requires_gpu() {
        crate::vm::preflight::ensure_vm_gpu(&config.backend, &config.vms, display).await?;
    }

    let backend = config.backend.clone();
    let remote_dir = config.remote_dir.clone();
    let binary_name = config.binary_name.clone();
    let compositor = steam::compositor_setup(display, &config.vm_user);
    let extra = extra_args.join(" ");

    println!(
        "==> Starting game on {} VMs ({display})...",
        config.vms.len()
    );

    let results = par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        let remote_dir = remote_dir.clone();
        let binary_name = binary_name.clone();
        let compositor = compositor.clone();
        let extra = extra.clone();
        async move {
            let cmd = format!(
                "{compositor}\
                 if ! pgrep -x steam >/dev/null; then\n\
                     steam -silent -cef-disable-gpu >/dev/null 2>&1 &\n\
                     sleep 5\n\
                 fi\n\
                 cd {remote_dir}\n\
                 if pgrep -x {binary_name} >/dev/null; then\n\
                     echo \"already-running\"\n\
                 else\n\
                     nohup ./{binary_name} {extra} > game.log 2>&1 &\n\
                     echo \"started\"\n\
                 fi",
            );
            let result = backend.run_cmd(&vm.ip, &cmd).await;
            (vm.name, result.stdout.trim().to_string())
        }
    })
    .await?;

    for (name, status) in results {
        println!("  {name}: {status}");
    }
    println!("==> Done");
    Ok(())
}
