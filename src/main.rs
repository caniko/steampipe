mod accounts;
mod backend;
mod bisect;
mod capture;
mod cli;
mod config;
mod credentials;
mod deploy;
mod history;
mod hooks;
mod lease;
mod logs;
mod mcp;
mod net;
mod netem;
mod output;
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

use clap::Parser;
use cli::{Cli, Commands, HistoryAction, NetworkMode, DisplayMode};
use config::{BackendKind, ClusterConfig, detect_project_root};
use output::OutputFormat;

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

/// Resolve a test config field: CLI explicit > profile > default.
macro_rules! resolve {
    ($cli:expr, $profile:expr, $field:ident, $default:expr) => {
        $cli.unwrap_or_else(|| {
            $profile
                .as_ref()
                .and_then(|p| p.$field.clone())
                .unwrap_or($default)
        })
    };
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Commands that don't need project root or config
    if matches!(&cli.command, Commands::Mcp) {
        return mcp::serve().await;
    }
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
        | Commands::Test { .. }
        | Commands::Bisect { .. } => {
            config.vms = config.leased_vms();
        }
        _ => {}
    }

    match cli.command {
        // Commands that require a validated bridge (or skip for non-microvm)
        Commands::Up { runners_dir } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            net::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            let started = vm::up(&validated, &runners_dir).await?;
            println!("==> {} VM(s) running", started.len());
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
            display,
        } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            net::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            steam::check(&validated, target.as_deref(), &runners_dir, display).await?;
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
            deploy::run(&config, &project_root, no_build, verify, true).await?
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
        Commands::SteamStart { target, display } => {
            steam::start(&config, target.as_deref(), display).await?;
        }
        Commands::Run { display, extra_args } => {
            run::start_game(&config, display, &extra_args).await?;
        }
        Commands::Test {
            profile,
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
            display,
            chaos_profile,
            heartbeat_stall_secs,
            output_format,
            on_complete,
            on_failure,
            strace,
        } => {
            // Load test profile if specified
            let prof = profile
                .as_deref()
                .and_then(|name| config::lookup_test_profile(&project_root, name));
            if profile.is_some() && prof.is_none() {
                eprintln!(
                    "Warning: test profile '{}' not found in steampipe.toml",
                    profile.as_deref().unwrap_or("")
                );
            }

            // Resolve network mode
            let resolved_network = network
                .or_else(|| {
                    prof.as_ref()
                        .and_then(|p| p.network.as_deref())
                        .and_then(|s| match s {
                            "lan" => Some(NetworkMode::Lan),
                            "steam" => Some(NetworkMode::Steam),
                            _ => None,
                        })
                })
                .unwrap_or(NetworkMode::Lan);

            // Resolve display mode
            let resolved_display = display
                .or_else(|| {
                    prof.as_ref()
                        .and_then(|p| p.display.as_deref())
                        .and_then(|s| match s {
                            "headless" => Some(DisplayMode::Headless),
                            "weston" => Some(DisplayMode::Weston),
                            "sway" => Some(DisplayMode::Sway),
                            "weston-gpu" => Some(DisplayMode::WestonGpu),
                            "sway-gpu" => Some(DisplayMode::SwayGpu),
                            _ => None,
                        })
                })
                .unwrap_or(DisplayMode::Weston);

            // Resolve output format
            let resolved_format = output_format
                .or_else(|| {
                    prof.as_ref()
                        .and_then(|p| p.output_format.as_deref())
                        .and_then(|s| match s {
                            "junit" => Some(OutputFormat::Junit),
                            "jsonl" => Some(OutputFormat::Jsonl),
                            _ => None,
                        })
                })
                .unwrap_or(OutputFormat::Text);

            // Resolve chaos profile
            let chaos = chaos_profile
                .as_deref()
                .or(prof.as_ref().and_then(|p| p.chaos_profile.as_deref()))
                .and_then(|name| config::lookup_chaos_profile(&project_root, name));

            // Resolve hooks: CLI > profile > config hooks
            let resolved_on_complete = on_complete
                .or_else(|| prof.as_ref().and_then(|p| p.on_complete.clone()))
                .or_else(|| config.hooks.on_complete.clone());
            let resolved_on_failure = on_failure
                .or_else(|| prof.as_ref().and_then(|p| p.on_failure.clone()))
                .or_else(|| config.hooks.on_failure.clone());

            test::run(
                &config,
                &project_root,
                test::TestConfig {
                    network: resolved_network,
                    players: resolve!(players, prof, players, 8),
                    vm_args: vm_args.or_else(|| prof.as_ref().and_then(|p| p.vm_args.clone())),
                    host_args: host_args.or_else(|| prof.as_ref().and_then(|p| p.host_args.clone())),
                    max_runs: resolve!(max_runs, prof, max_runs, 1),
                    timeout: std::time::Duration::from_secs(resolve!(timeout, prof, timeout, 300)),
                    shutdown_timeout: std::time::Duration::from_secs(
                        resolve!(shutdown_timeout, prof, shutdown_timeout, 5),
                    ),
                    stop_on_failure: !no_stop_on_failure
                        && !prof
                            .as_ref()
                            .and_then(|p| p.no_stop_on_failure)
                            .unwrap_or(false),
                    deploy: !no_deploy
                        && !prof
                            .as_ref()
                            .and_then(|p| p.no_deploy)
                            .unwrap_or(false),
                    build: !no_build
                        && !prof
                            .as_ref()
                            .and_then(|p| p.no_build)
                            .unwrap_or(false),
                    filter_pattern: filter_pattern
                        .or_else(|| prof.as_ref().and_then(|p| p.filter_pattern.clone())),
                    output_file: output_file.or_else(|| {
                        prof.as_ref()
                            .and_then(|p| p.output_file.as_ref())
                            .map(PathBuf::from)
                    }),
                    capture_on_failure: capture_on_failure
                        || prof
                            .as_ref()
                            .and_then(|p| p.capture_on_failure)
                            .unwrap_or(false),
                    display: resolved_display,
                    verbose: true,
                    chaos,
                    heartbeat_stall_secs: heartbeat_stall_secs
                        .or_else(|| prof.as_ref().and_then(|p| p.heartbeat_stall_secs))
                        .unwrap_or(config.heartbeat_stall_secs),
                    output_format: resolved_format,
                    on_complete: resolved_on_complete,
                    on_failure: resolved_on_failure,
                    exit_codes: config.exit_codes.clone(),
                    strace,
                },
            )
            .await?;
        }

        Commands::Bisect {
            good,
            bad,
            network,
            players,
            timeout,
            runs_per_step,
            pass_threshold,
            display,
            vm_args,
            host_args,
        } => {
            bisect::run(
                &config,
                &project_root,
                bisect::BisectConfig {
                    good_sha: good,
                    bad_sha: bad,
                    network,
                    players,
                    timeout: std::time::Duration::from_secs(timeout),
                    runs_per_step,
                    pass_threshold,
                    display,
                    vm_args,
                    host_args,
                },
            )
            .await?;
        }

        // New commands
        Commands::Netem {
            target,
            latency,
            jitter,
            loss,
            rate,
            chaos_profile,
        } => {
            // Resolve chaos profile if specified
            let (lat, jit, los, rat) = if let Some(ref name) = chaos_profile {
                let profile = config::lookup_chaos_profile(&project_root, name)
                    .ok_or_else(|| anyhow::anyhow!("Chaos profile '{name}' not found in steampipe.toml"))?;
                (profile.latency, profile.jitter, profile.loss, profile.rate)
            } else {
                (latency, jitter, loss, rate)
            };
            println!("==> Applying network emulation...");
            netem::apply(&config, &target, lat, jit, los, rat)?;
            println!("==> Done");
        }
        Commands::NetemShow => netem::show(&config)?,
        Commands::NetemReset { target } => {
            println!("==> Resetting network emulation...");
            netem::reset(&config, target.as_deref())?;
            println!("==> Done");
        }
        Commands::History {
            action,
            last,
            clear,
            format,
            output,
        } => {
            if clear {
                history::clear(&config.state_dir)?;
            } else if let Some(fmt) = format {
                history::export(&config.state_dir, last, fmt, output.as_deref(), &config.exit_codes)?;
            } else {
                match action {
                    None => history::show(&config.state_dir, last, &config.exit_codes)?,
                    Some(HistoryAction::Trends { window, last: n }) => {
                        history::show_trends(&config.state_dir, window, n, &config.exit_codes)?;
                    }
                    Some(HistoryAction::Flaky { last: n }) => {
                        history::show_flakiness(&config.state_dir, n, &config.exit_codes)?;
                    }
                }
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
        Commands::Init | Commands::YhConfig { .. } | Commands::Completions { .. } | Commands::Mcp => {
            unreachable!()
        }
    }

    Ok(())
}
