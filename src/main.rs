mod accounts;
mod backend;
mod capture;
mod cli;
mod config;
mod credentials;
mod deploy;
mod doctor;
mod history;
mod lease;
mod logs;
mod net;
mod netem;
mod preflight;
mod resources;
mod run;
mod snapshot;
mod ssh;
mod state;
mod status;
mod steam;
mod test;
mod vm;
mod watch;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use clap::Parser;
use cli::{Cli, Commands};
use config::{BackendKind, ClusterConfig, detect_project_root};

/// Require a runners_dir for the microvm backend, or return a dummy path for others.
fn require_runners_dir(
    runners_dir: Option<PathBuf>,
    backend_kind: BackendKind,
) -> anyhow::Result<PathBuf> {
    match runners_dir {
        Some(p) => Ok(p),
        None if backend_kind == BackendKind::Microvm => {
            anyhow::bail!("--runners-dir is required for the microvm backend")
        }
        None => Ok(PathBuf::from("/dev/null")), // unused by non-microvm backends
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Commands that don't need project root or config
    if let Commands::Completions { shell } = &cli.command {
        cli::print_completions(*shell);
        return Ok(());
    }

    let project_root = match &cli.project_root {
        Some(p) => p.clone(),
        None => detect_project_root()?,
    };

    // Init doesn't need vm_count
    if matches!(&cli.command, Commands::Init) {
        let path = project_root.join("steampipe.toml");
        if path.exists() {
            anyhow::bail!("steampipe.toml already exists");
        }
        std::fs::write(&path, config::generate_config_template())?;
        println!("Created {}", path.display());
        return Ok(());
    }

    // YhConfig doesn't need vm_count either
    if let Commands::YhConfig { force } = &cli.command {
        let proj = config::load_project_config(&project_root);
        let cluster_name = cli
            .cluster
            .as_deref()
            .or_else(|| {
                proj.as_ref()
                    .and_then(|p| p.cluster.as_ref())
                    .and_then(|c| c.name.as_deref())
            })
            .unwrap_or("default");

        let stem = if cluster_name == "default" {
            "cluster"
        } else {
            cluster_name
        };
        let filename = format!("{stem}.yh.cluster.toml");
        let path = project_root.join(&filename);

        if path.exists() && !force {
            anyhow::bail!("{filename} already exists. Use --force to overwrite.");
        }

        let content = config::generate_yh_cluster_config(
            &project_root,
            cli.vm_count,
            cli.cluster.as_deref(),
            cli.backend,
            cli.state_dir.as_deref(),
            cli.lock_dir.as_deref(),
            cli.ssh_key.as_deref(),
            cli.credentials.as_deref(),
        );

        std::fs::write(&path, &content)?;
        println!("Created {}", path.display());
        return Ok(());
    }

    let vm_count = cli
        .vm_count
        .ok_or_else(|| anyhow::anyhow!("--vm-count is required (e.g. --vm-count 7)"))?;
    let mut config =
        ClusterConfig::new(&project_root, vm_count, cli.cluster.as_deref(), cli.backend)?;
    if let Some(key) = &cli.ssh_key {
        config.ssh_key = key.clone();
    }
    if let Some(dir) = &cli.state_dir {
        config.state_dir = dir.clone();
    }
    if let Some(dir) = &cli.lock_dir {
        config.lock_dir = dir.clone();
    }

    let creds = cli
        .credentials
        .as_deref()
        .map(|path| credentials::load(path, None))
        .transpose()?;

    state::ensure_state_dir(&config.state_dir)?;

    // For commands that operate on VMs, prefer leased VMs over the static 1..N list
    match &cli.command {
        Commands::Deploy { .. }
        | Commands::Run { .. }
        | Commands::StopGame { .. }
        | Commands::Test { .. } => {
            config.vms = config.leased_vms();
        }
        _ => {}
    }

    // Extract global args before consuming cli.command in the match
    let global_args = cli.global_args();

    match cli.command {
        // Commands that require a validated bridge (or skip for non-microvm)
        Commands::Up {
            runners_dir,
            detach,
        } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            net::ensure_bridge(&config, &project_root)?;

            if detach {
                // Spawn a daemon process that does the full up + hold workflow
                let exe = std::env::current_exe()?;
                let daemon_pid_file = config.state_dir.join("daemon.pid");
                let _ = std::fs::remove_file(&daemon_pid_file);

                let mut cmd = std::process::Command::new(exe);
                cmd.args(&global_args);
                cmd.arg("hold-leases");
                cmd.arg("--runners-dir").arg(&runners_dir);
                cmd.stdin(Stdio::null());
                cmd.stdout(Stdio::inherit());
                cmd.stderr(Stdio::inherit());

                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    cmd.process_group(0);
                }

                let child = cmd.spawn()?;
                let child_pid = child.id();

                // Poll for readiness (daemon writes PID file once VMs are up)
                let deadline = Instant::now() + Duration::from_secs(120);
                loop {
                    if daemon_pid_file.exists() {
                        println!("==> Detached (daemon PID {child_pid})");
                        break;
                    }
                    if Instant::now() > deadline {
                        let _ = std::process::Command::new("kill")
                            .args(["-TERM", &child_pid.to_string()])
                            .status();
                        anyhow::bail!("daemon did not become ready within 120s");
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            } else {
                // Foreground mode: hold leases until Ctrl+C
                let validated = config.validate_or_skip_bridge()?;
                let result = vm::up(&validated, &runners_dir).await?;

                println!(
                    "==> Holding {} VM(s). Press Ctrl+C to shut down.",
                    result.vms.len()
                );
                tokio::signal::ctrl_c().await?;

                println!();
                vm::down_vms(&validated, &result.vms);
                drop(result.leases);
            }
        }
        Commands::HoldLeases { runners_dir } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            let state_dir = config.state_dir.clone();
            let validated = config.validate_or_skip_bridge()?;
            let result = vm::up(&validated, &runners_dir).await?;

            // Signal readiness to parent via PID file
            state::write_pid(&state_dir, "daemon", std::process::id())?;

            println!(
                "==> Holding {} VM(s) (daemon mode, PID {})",
                result.vms.len(),
                std::process::id()
            );

            // Block until SIGTERM or Ctrl+C
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm = signal(SignalKind::terminate())?;
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = sigterm.recv() => {},
                }
            }
            #[cfg(not(unix))]
            {
                tokio::signal::ctrl_c().await?;
            }

            println!("\n==> Daemon shutting down...");
            vm::down_vms(&validated, &result.vms);
            state::remove_pid(&state_dir, "daemon");
            drop(result.leases);
        }
        Commands::Restart {
            target,
            runners_dir,
        } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            net::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            vm::restart(&validated, target.as_deref(), &runners_dir).await?;
        }
        Commands::SteamCheck {
            target,
            runners_dir,
        } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            net::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            steam::check(&validated, target.as_deref(), &runners_dir).await?;
        }
        Commands::SteamLogin {
            target,
            continue_from,
            login_runners_dir,
        } => {
            let login_runners_dir = require_runners_dir(login_runners_dir, config.backend_kind)?;
            net::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            steam::login(
                &validated,
                target.as_deref(),
                continue_from,
                &login_runners_dir,
                creds.as_ref(),
            )
            .await?;
        }

        // Commands that work on any config state
        Commands::Status => status::run(&config).await?,
        Commands::Down => vm::down(&config),
        Commands::NetUp { nft } => net::up(&config, &nft)?,
        Commands::NetDown { nft } => net::down(&config, &nft)?,
        Commands::Deploy { no_build, verify } => {
            deploy::run(&config, &project_root, no_build, verify).await?
        }
        Commands::StopGame { kill_steam } => run::stop_game(&config, kill_steam).await?,
        Commands::Logs {
            target,
            output,
            follow,
            lines,
            head,
            pattern,
        } => {
            if follow {
                let tail_lines = lines.unwrap_or(20);
                logs::follow(&config, tail_lines, target.as_deref()).await?;
            } else if output.is_some() {
                logs::run(&config, &project_root, output).await?;
            } else {
                let n = lines.unwrap_or(100);
                logs::print_stdout(&config, target.as_deref(), n, head, pattern.as_deref()).await?;
            }
        }
        Commands::SteamStart { target } => {
            steam::start(&config, target.as_deref()).await?;
        }
        Commands::Run { extra_args } => {
            run::start_game(&config, &extra_args).await?;
        }
        Commands::Test {
            network,
            players,
            vm_args,
            host_args,
            max_runs,
            timeout,
            shutdown_timeout,
            no_stop_on_failure,
            no_deploy,
            no_build,
            filter_pattern,
            output_file,
            capture_on_failure,
        } => {
            test::run(
                &config,
                &project_root,
                test::TestConfig {
                    network,
                    players,
                    vm_args,
                    host_args,
                    max_runs,
                    timeout: std::time::Duration::from_secs(timeout),
                    shutdown_timeout: std::time::Duration::from_secs(shutdown_timeout),
                    stop_on_failure: !no_stop_on_failure,
                    deploy: !no_deploy,
                    build: !no_build,
                    filter_pattern,
                    output_file,
                    capture_on_failure,
                },
            )
            .await?;
        }

        // New commands
        Commands::Doctor { fix } => doctor::run(&config, fix)?,
        Commands::Netem {
            target,
            latency,
            jitter,
            loss,
            rate,
        } => {
            println!("==> Applying network emulation...");
            netem::apply(&config, &target, latency, jitter, loss, rate)?;
            println!("==> Done");
        }
        Commands::NetemShow => netem::show(&config)?,
        Commands::NetemReset { target } => {
            println!("==> Resetting network emulation...");
            netem::reset(&config, target.as_deref())?;
            println!("==> Done");
        }
        Commands::History { last, clear } => {
            if clear {
                history::clear(&config.state_dir)?;
            } else {
                history::show(&config.state_dir, last)?;
            }
        }
        Commands::SnapshotSave { name } => snapshot::save(&config, &name)?,
        Commands::SnapshotRestore { name } => snapshot::restore(&config, &name)?,
        Commands::SnapshotList => snapshot::list(&config)?,
        Commands::SnapshotDelete { name } => snapshot::delete(&config, &name)?,
        Commands::Screenshot { output } => {
            let output_dir = output.unwrap_or_else(|| {
                let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
                project_root.join(format!("screenshots/{ts}"))
            });
            capture::screenshot_all(&config, &output_dir).await?;
        }
        Commands::Accounts => accounts::show(&config).await?,
        Commands::Watch { interval } => watch::run(&config, interval).await?,
        Commands::Init | Commands::YhConfig { .. } | Commands::Completions { .. } => {
            unreachable!()
        }
    }

    Ok(())
}
