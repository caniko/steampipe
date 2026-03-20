mod accounts;
mod backend;
mod capture;
mod cli;
mod config;
mod credentials;
mod deploy;
mod doctor;
mod history;
mod logs;
mod netem;
mod net;
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

use clap::Parser;
use cli::{Cli, Commands};
use config::{BackendKind, ClusterConfig, detect_project_root};

/// Require a runners_dir for the microvm backend, or return a dummy path for others.
fn require_runners_dir(runners_dir: Option<PathBuf>, backend_kind: BackendKind) -> anyhow::Result<PathBuf> {
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
    match &cli.command {
        Commands::Completions { shell } => {
            cli::print_completions(*shell);
            return Ok(());
        }
        _ => {}
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

    let vm_count = cli.vm_count
        .ok_or_else(|| anyhow::anyhow!("--vm-count is required (e.g. --vm-count 7)"))?;
    let mut config = ClusterConfig::new(&project_root, vm_count, cli.cluster.as_deref(), cli.backend);
    if let Some(key) = &cli.ssh_key {
        config.ssh_key = key.clone();
    }

    let creds = cli
        .credentials
        .as_deref()
        .map(|path| credentials::load(path, None))
        .transpose()?;

    state::ensure_state_dir(&config.state_dir)?;

    match cli.command {
        // Commands that require a validated bridge (or skip for non-microvm)
        Commands::Up { runners_dir } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            let validated = config.validate_or_skip_bridge()?;
            vm::up(&validated, &runners_dir).await?;
        }
        Commands::Restart { target, runners_dir } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            let validated = config.validate_or_skip_bridge()?;
            vm::restart(&validated, target.as_deref(), &runners_dir).await?;
        }
        Commands::SteamCheck { target, runners_dir } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            let validated = config.validate_or_skip_bridge()?;
            steam::check(&validated, target.as_deref(), &runners_dir).await?;
        }
        Commands::SteamLogin {
            target,
            continue_from,
            login_runners_dir,
        } => {
            let login_runners_dir = require_runners_dir(login_runners_dir, config.backend_kind)?;
            let validated = config.validate_or_skip_bridge()?;
            steam::login(&validated, target.as_deref(), continue_from, &login_runners_dir, creds.as_ref()).await?;
        }

        // Commands that work on any config state
        Commands::Status => status::run(&config).await?,
        Commands::Down => vm::down(&config),
        Commands::NetUp { nft } => net::up(&config, &nft)?,
        Commands::NetDown { nft } => net::down(&config, &nft)?,
        Commands::Deploy { no_build, verify } => deploy::run(&config, &project_root, no_build, verify).await?,
        Commands::StopGame => run::stop_game(&config).await?,
        Commands::Logs { output, follow, tail } => {
            if follow {
                logs::follow(&config, tail).await?;
            } else {
                logs::run(&config, &project_root, output).await?;
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
            stop_on_failure,
            deploy,
            build,
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
                    stop_on_failure,
                    deploy,
                    build,
                    filter_pattern,
                    output_file,
                    capture_on_failure,
                },
            )
            .await?;
        }

        // New commands
        Commands::Doctor { fix } => doctor::run(&config, fix)?,
        Commands::Netem { target, latency, jitter, loss, rate } => {
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
        Commands::Init | Commands::Completions { .. } => unreachable!(),
    }

    Ok(())
}
