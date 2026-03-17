mod cli;
mod config;
mod deploy;
mod logs;
mod net;
mod run;
mod ssh;
mod state;
mod status;
mod steam;
mod test;
mod vm;

use clap::Parser;
use cli::{Cli, Commands};
use config::{ClusterConfig, detect_project_root};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let project_root = match &cli.project_root {
        Some(p) => p.clone(),
        None => detect_project_root()?,
    };

    let mut config = ClusterConfig::new(&project_root);
    if let Some(key) = &cli.ssh_key {
        config.ssh_key = key.clone();
    }

    state::ensure_state_dir(&config.state_dir)?;

    match cli.command {
        Commands::Status => status::run(&config).await?,
        Commands::Up { runners_dir } => vm::up(&config, &runners_dir).await?,
        Commands::Down => vm::down(&config),
        Commands::NetUp { nft } => net::up(&config, &nft)?,
        Commands::NetDown { nft } => net::down(&config, &nft)?,
        Commands::Deploy { no_build } => deploy::run(&config, &project_root, no_build).await?,
        Commands::StopGame => run::stop_game(&config).await?,
        Commands::Logs { output } => logs::run(&config, &project_root, output).await?,
        Commands::SteamStart { target } => {
            steam::start(&config, target.as_deref()).await?;
        }
        Commands::SteamCheck { target, runners_dir } => {
            steam::check(&config, target.as_deref(), &runners_dir).await?;
        }
        Commands::SteamLogin {
            target,
            continue_from,
            login_runners_dir,
        } => {
            steam::login(&config, target.as_deref(), continue_from, &login_runners_dir).await?;
        }
        Commands::Run { extra_args } => {
            run_game(&config, &extra_args).await?;
        }
        Commands::Test {
            mode,
            network,
            players,
            max_runs,
            timeout,
            shutdown_timeout,
            stop_on_failure,
            deploy,
            build,
            headless,
            filter_pattern,
            output_file,
        } => {
            test::run(
                &config,
                &project_root,
                test::TestConfig {
                    mode,
                    network,
                    players,
                    max_runs,
                    timeout: std::time::Duration::from_secs(timeout),
                    shutdown_timeout: std::time::Duration::from_secs(shutdown_timeout),
                    stop_on_failure,
                    deploy,
                    build,
                    headless,
                    filter_pattern,
                    output_file,
                },
            )
            .await?;
        }
    }

    Ok(())
}

async fn run_game(config: &ClusterConfig, extra_args: &[String]) -> anyhow::Result<()> {
    let ssh = ssh::SshClient::new(&config.ssh_key, &config.vm_user);

    println!("==> Starting chessbender on {} VMs...", config.vms.len());

    let mut tasks = tokio::task::JoinSet::new();
    for vm in &config.vms {
        let ssh = ssh.clone();
        let name = vm.name.clone();
        let ip = vm.ip.clone();
        let remote_dir = config.remote_dir.clone();
        let host_ip = config.host_ip.clone();
        let udp_port = config.udp_port;
        let extra = extra_args.join(" ");

        tasks.spawn(async move {
            // Start weston + Steam + chessbender
            let cmd = format!(
                r#"
                export XDG_RUNTIME_DIR=/tmp/runtime-chessbender
                mkdir -p $XDG_RUNTIME_DIR
                if ! pgrep -x weston >/dev/null; then
                    weston --backend=headless --xwayland --no-config >/dev/null 2>&1 &
                    sleep 2
                fi
                export WAYLAND_DISPLAY=wayland-1
                export DISPLAY=:0
                if ! pgrep -x steam >/dev/null; then
                    steam -silent -cef-disable-gpu >/dev/null 2>&1 &
                    sleep 5
                fi
                cd {remote_dir}
                if pgrep -x chessbender >/dev/null; then
                    echo "already-running"
                else
                    nohup ./chessbender --headless --auto-join-tournament --udp-addr {host_ip}:{udp_port} --auto-play {extra} > game.log 2>&1 &
                    echo "started"
                fi
                "#,
            );
            let result = ssh.run(&ip, &cmd).await;
            let status = result.stdout.trim().to_string();
            println!("  {name}: {status}");
        });
    }

    while let Some(result) = tasks.join_next().await {
        result?;
    }
    println!("==> Done");
    Ok(())
}
