mod core;
mod game;
mod harness;
mod mcp;
mod net;
mod ui;
mod vm;

use std::path::PathBuf;

use clap::Parser;
use core::config::{self, BackendKind, ClusterConfig, VisualConfig, detect_project_root};
use core::credentials;
use core::state;
use game::{accounts, capture, compositor, run, steam};
use harness::{bisect, history, output::OutputFormat, runner as test};
use net::{bridge, deploy, netem};
use ui::cli::{
    self, CaptureMode, Cli, Commands, DisplayMode, HistoryAction, NetworkMode, ScreenshotBackend,
    VisualAction,
};
use ui::{logs, watch};
use vm::{lifecycle, preflight, snapshot, status};

/// Resolve the env-var table passed to both peers for this test run.
///
/// Order: profile `env` table merged on top of any cluster-wide defaults (none
/// today), then the auto-set `THESPAN_SESSION_ID` if the consumer didn't pin
/// one explicitly. The auto-default is `<cluster-name>-<runner-pid>` so two
/// concurrent fix-loops on the same host always get distinct ids without the
/// caller having to think about it. Production runs that don't enable the
/// chessbender `session-isolation` Cargo feature simply ignore the env var.
fn resolve_run_env(
    profile: Option<&core::config::TestProfile>,
    config: &core::config::ClusterConfig,
) -> std::collections::BTreeMap<String, String> {
    let mut env: std::collections::BTreeMap<String, String> =
        profile.and_then(|p| p.env.clone()).unwrap_or_default();
    env.entry("THESPAN_SESSION_ID".to_string())
        .or_insert_with(|| format!("{}-{}", config.cluster_name, std::process::id()));
    env
}

fn resolve_screenshot_backend_from_profile(
    profile: Option<&core::config::TestProfile>,
) -> ScreenshotBackend {
    profile
        .and_then(|p| p.screenshot_backend.as_deref())
        .and_then(|s| match s {
            "grim" | "wayland" => Some(ScreenshotBackend::Grim),
            "vnc" => Some(ScreenshotBackend::Vnc),
            _ => None,
        })
        .unwrap_or(ScreenshotBackend::Grim)
}

fn select_visual_validator(project_config: core::config::ProjectConfig) -> anyhow::Result<String> {
    project_config.warn_if_legacy_visual_validator_shadowed(None);
    let validators: Vec<String> = project_config
        .profile
        .unwrap_or_default()
        .into_values()
        .filter_map(|profile| profile.visual_validator)
        .collect();
    match validators.as_slice() {
        [validator] => Ok(validator.clone()),
        [] => anyhow::bail!(
            "--validate requires exactly one profile visual_validator in steampipe.toml"
        ),
        _ => anyhow::bail!(
            "--validate found multiple profile visual_validator values; use a test profile for disambiguation"
        ),
    }
}

#[derive(Debug, Clone)]
enum ValidatorPlan {
    Shell(String),
    Golden(VisualConfig),
    None,
}

fn select_validator_plan(
    project_root: &std::path::Path,
    profile: Option<&core::config::TestProfile>,
) -> anyhow::Result<ValidatorPlan> {
    let Some(mut project_config) = config::load_project_config(project_root) else {
        return Ok(ValidatorPlan::None);
    };
    project_config.warn_if_legacy_visual_validator_shadowed(None);
    match project_config.effective_validator_kind(profile) {
        core::config::ValidatorKind::Shell => profile
            .and_then(|profile| profile.visual_validator.clone())
            .map(ValidatorPlan::Shell)
            .ok_or_else(|| anyhow::anyhow!("validator_kind = \"shell\" requires visual_validator")),
        core::config::ValidatorKind::Golden => {
            let visual = project_config.visual.as_mut().ok_or_else(|| {
                anyhow::anyhow!("validator_kind = \"golden\" requires a [visual] block")
            })?;
            visual.resolve_paths(project_root);
            Ok(ValidatorPlan::Golden(visual.clone()))
        }
        core::config::ValidatorKind::None => Ok(ValidatorPlan::None),
    }
}

fn load_visual_config(project_root: &std::path::Path) -> anyhow::Result<VisualConfig> {
    let mut project_config = config::load_project_config(project_root)
        .ok_or_else(|| anyhow::anyhow!("visual command requires steampipe.toml"))?;
    let visual = project_config
        .visual
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("visual command requires a [visual] block"))?;
    visual.resolve_paths(project_root);
    Ok(visual.clone())
}

fn print_visual_scenes(project_root: &std::path::Path) -> anyhow::Result<()> {
    let visual = load_visual_config(project_root)?;

    println!("Name | Window | Backend | Tolerance | Golden");
    println!("--- | --- | --- | --- | ---");
    for scene in &visual.scene {
        let golden_path = visual.golden_path(&scene.name)?;
        let _diff_paths = visual.diff_path(&scene.name, "manual")?;
        let window = scene
            .window
            .as_ref()
            .map(|window| {
                window
                    .app_id
                    .as_ref()
                    .map(|app_id| format!("app_id={app_id}"))
                    .or_else(|| window.title.as_ref().map(|title| format!("title={title}")))
                    .or_else(|| window.class.as_ref().map(|class| format!("class={class}")))
                    .unwrap_or_else(|| "-".into())
            })
            .unwrap_or_else(|| "-".into());
        let backend = scene
            .backend
            .or(visual.default_backend)
            .map(|backend| backend.to_string())
            .unwrap_or_else(|| "-".into());
        let tolerance = scene
            .tolerance
            .as_ref()
            .or(visual.default_tolerance.as_ref())
            .map(|tolerance| {
                let ssim = tolerance
                    .ssim
                    .map(|ssim| format!("{ssim:.3}"))
                    .unwrap_or_else(|| "-".into());
                let max_delta = tolerance
                    .max_pixel_delta
                    .map(|delta| delta.to_string())
                    .unwrap_or_else(|| "-".into());
                format!("ssim={ssim}, max_pixel_delta={max_delta}")
            })
            .unwrap_or_else(|| "-".into());
        let golden = if golden_path.exists() {
            "yes"
        } else {
            "missing"
        };
        println!(
            "{} | {window} | {backend} | {tolerance} | {golden}",
            scene.name
        );
    }
    Ok(())
}

fn resolve_visual_scene<'a>(
    visual: &'a VisualConfig,
    scene_name: &str,
) -> anyhow::Result<&'a core::config::Scene> {
    visual
        .scene(scene_name)
        .ok_or_else(|| anyhow::anyhow!("visual scene '{scene_name}' not found"))
}

fn scene_capture_backend(visual: &VisualConfig, scene: &core::config::Scene) -> ScreenshotBackend {
    scene
        .backend
        .or(visual.default_backend)
        .map(|backend| match backend {
            core::config::VisualBackend::Grim => ScreenshotBackend::Grim,
            core::config::VisualBackend::Vnc => ScreenshotBackend::Vnc,
        })
        .unwrap_or(ScreenshotBackend::Vnc)
}

fn parse_record_args(args: &[String]) -> anyhow::Result<(Option<&str>, &str)> {
    match args {
        [scene] => Ok((None, scene.as_str())),
        [vm, scene] => Ok((Some(vm.as_str()), scene.as_str())),
        _ => anyhow::bail!("visual record expects <scene> or <vm> <scene>"),
    }
}

fn find_vm<'a, S>(
    config: &'a ClusterConfig<S>,
    target: Option<&str>,
) -> anyhow::Result<&'a core::config::VmDef> {
    match target {
        Some(target) => {
            let normalized = target
                .strip_prefix("vm-")
                .map(|id| format!("vm-{id}"))
                .unwrap_or_else(|| {
                    if target.bytes().all(|b| b.is_ascii_digit()) {
                        format!("vm-{target}")
                    } else {
                        target.to_string()
                    }
                });
            config
                .vms
                .iter()
                .find(|vm| vm.name.0 == normalized)
                .ok_or_else(|| anyhow::anyhow!("VM '{target}' not found"))
        }
        None => config
            .vms
            .first()
            .ok_or_else(|| anyhow::anyhow!("no VMs configured")),
    }
}

async fn record_visual_scene<S>(
    config: &ClusterConfig<S>,
    project_root: &std::path::Path,
    args: &[String],
    force: bool,
) -> anyhow::Result<()> {
    let visual = load_visual_config(project_root)?;
    let (vm_target, scene_name) = parse_record_args(args)?;
    let scene = resolve_visual_scene(&visual, scene_name)?;
    let vm = find_vm(config, vm_target)?;
    let backend = scene_capture_backend(&visual, scene);
    let temp_dir = std::env::temp_dir().join(format!(
        "steampipe-visual-record-{}-{}",
        std::process::id(),
        scene.name
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let options = capture::ScreenshotOptions {
        backend,
        window_target: scene.window.clone(),
        ..Default::default()
    };
    let (actual, _) = capture::capture_single_vm(config, vm, &temp_dir, &options).await?;
    let golden = game::visual::record_scene(&visual, scene, &actual, force)?;
    println!(
        "Recorded {} from {} to {}",
        scene.name,
        vm.name,
        golden.display()
    );
    let _ = std::fs::remove_dir_all(temp_dir);
    Ok(())
}

async fn capture_visual_scene<S>(
    config: &ClusterConfig<S>,
    project_root: &std::path::Path,
    scene_name: &str,
    vm_target: Option<&str>,
    output: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let visual = load_visual_config(project_root)?;
    let scene = resolve_visual_scene(&visual, scene_name)?;
    let vm = find_vm(config, vm_target)?;
    let backend = scene_capture_backend(&visual, scene);
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let run_label = format!("manual-{ts}");
    let output_dir = output
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| project_root.join(format!("screenshots/{run_label}")));

    if backend == ScreenshotBackend::Vnc {
        capture::ensure_wayvnc(&config.backend, vm, &config.vm_user, true).await?;
    }

    crate::game::scene::runner::run_scene_named(
        &config.backend,
        vm,
        &config.vm_user,
        &visual,
        scene_name,
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))?;

    let golden = visual
        .golden_path(scene_name)
        .ok()
        .filter(|path| path.exists())
        .map(|_| visual.clone());
    let options = capture::ScreenshotOptions {
        backend,
        validator: None,
        golden,
        scene: Some(scene_name.to_string()),
        run_label: Some(run_label),
        window_target: scene.window.clone(),
        allow_vnc_input: true,
    };
    let (image_path, visual_results) =
        capture::capture_single_vm(config, vm, &output_dir, &options).await?;
    println!(
        "Captured {} on {} to {}",
        scene.name,
        vm.name,
        image_path.display()
    );
    for result in visual_results {
        println!(
            "{}: {} (ssim {:.5}, max_delta {}, result {})",
            result.scene,
            if result.passed { "passed" } else { "failed" },
            result.ssim,
            result.max_delta,
            result.result_path.display()
        );
    }
    Ok(())
}

fn bless_visual_scene(
    project_root: &std::path::Path,
    scene_name: &str,
    run_label: Option<&str>,
    yes: bool,
) -> anyhow::Result<()> {
    let visual = load_visual_config(project_root)?;
    let scene = resolve_visual_scene(&visual, scene_name)?;
    let run_label =
        run_label.ok_or_else(|| anyhow::anyhow!("visual bless requires --from <run-label>"))?;
    let golden = game::visual::bless_scene(&visual, scene, run_label, yes)?;
    println!(
        "Blessed {} from {run_label} to {}",
        scene.name,
        golden.display()
    );
    Ok(())
}

fn diff_visual_scene(
    project_root: &std::path::Path,
    scene_name: &str,
    run_label: Option<&str>,
) -> anyhow::Result<()> {
    let visual = load_visual_config(project_root)?;
    let scene = resolve_visual_scene(&visual, scene_name)?;
    let run_label =
        run_label.ok_or_else(|| anyhow::anyhow!("visual diff requires --from <run-label>"))?;
    let engine = game::visual::Engine::new(visual.clone());
    let result = game::visual::diff_scene(&visual, &engine, scene, run_label)?;
    println!(
        "{}: {} (ssim {:.5}, max_delta {}, result {})",
        result.scene,
        if result.passed { "passed" } else { "failed" },
        result.ssim,
        result.max_delta,
        result.result_path.display()
    );
    if result.passed {
        Ok(())
    } else {
        anyhow::bail!("visual diff failed for {}", result.scene)
    }
}

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
        ui::cli::print_completions(*shell);
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

    if let Commands::Visual {
        action: VisualAction::List,
    } = &cli.command
    {
        print_visual_scenes(&project_root)?;
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
            bridge::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            let started = lifecycle::up(&validated, &runners_dir).await?;
            println!("==> {} VM(s) running", started.len());

            // Auto-login Steam if credentials were provided
            if let Some(creds) = &creds {
                steam::auto_login(&validated, &started, creds).await?;
            }
        }
        Commands::Restart {
            target,
            runners_dir,
        } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            bridge::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            lifecycle::restart(&validated, target.as_deref(), &runners_dir).await?;
        }
        Commands::SteamCheck {
            target,
            runners_dir,
            display,
        } => {
            let runners_dir = require_runners_dir(runners_dir, config.backend_kind)?;
            bridge::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            steam::check(&validated, target.as_deref(), &runners_dir, display).await?;
        }
        Commands::GpuPreflight {
            target,
            runners_dir,
            json,
        } => {
            if let Some(runners_dir) = runners_dir {
                let validated = config.validate_or_skip_bridge()?;
                preflight::gpu_preflight_with_boot(
                    &validated,
                    target.as_deref(),
                    &runners_dir,
                    json,
                )
                .await?;
            } else {
                preflight::gpu_preflight(&config, target.as_deref(), json).await?;
            }
        }
        Commands::SteamLogin {
            target,
            continue_from,
            login_runners_dir,
        } => {
            let login_runners_dir = require_runners_dir(login_runners_dir, config.backend_kind)?;
            bridge::ensure_bridge(&config, &project_root)?;
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
        Commands::SteamGuard { target, code } => {
            bridge::ensure_bridge(&config, &project_root)?;
            let validated = config.validate_or_skip_bridge()?;
            steam::guard(&validated, target.as_str(), code.as_deref(), creds.as_ref()).await?;
        }

        // Commands that work on any config state
        Commands::Status => status::run(&config).await?,
        Commands::CompositorStatus => compositor::status_all(&config).await?,
        Commands::Down => lifecycle::down(&config),
        Commands::NetUp { nft } => bridge::up(&config, &nft)?,
        Commands::NetDown { nft } => bridge::down(&config, &nft)?,
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
        Commands::Run {
            display,
            extra_args,
        } => {
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
            hard_timeout,
            shutdown_timeout,
            no_stop_on_failure,
            no_deploy,
            no_build,
            filter_pattern,
            output_file,
            capture_on_failure,
            capture_mode,
            capture_interval,
            scenes,
            scene_vm,
            display,
            chaos_profile,
            heartbeat_stall_secs,
            output_format,
            on_complete,
            on_failure,
            record_video,
            strace,
            ..
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

            let resolved_screenshot_backend =
                resolve_screenshot_backend_from_profile(prof.as_ref());
            let validator_plan = select_validator_plan(&project_root, prof.as_ref())?;
            let (visual_validator, visual_config) = match validator_plan {
                ValidatorPlan::Shell(validator) => (Some(validator), None),
                ValidatorPlan::Golden(visual) => (None, Some(visual)),
                ValidatorPlan::None => (None, None),
            };

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

            test::run(&config, &project_root, {
                let resolved_timeout_secs = resolve!(timeout, prof, timeout, 300);
                test::TestConfig {
                    network: resolved_network,
                    players: resolve!(players, prof, players, 8),
                    vm_args: vm_args.or_else(|| prof.as_ref().and_then(|p| p.vm_args.clone())),
                    host_args: host_args
                        .or_else(|| prof.as_ref().and_then(|p| p.host_args.clone())),
                    max_runs: resolve!(max_runs, prof, max_runs, 1),
                    timeout: std::time::Duration::from_secs(resolved_timeout_secs),
                    hard_timeout: test::resolve_hard_timeout(
                        std::time::Duration::from_secs(resolved_timeout_secs),
                        hard_timeout
                            .or_else(|| prof.as_ref().and_then(|p| p.hard_timeout))
                            .map(std::time::Duration::from_secs),
                    ),
                    shutdown_timeout: std::time::Duration::from_secs(resolve!(
                        shutdown_timeout,
                        prof,
                        shutdown_timeout,
                        5
                    )),
                    stop_on_failure: !no_stop_on_failure
                        && !prof
                            .as_ref()
                            .and_then(|p| p.no_stop_on_failure)
                            .unwrap_or(false),
                    deploy: !no_deploy && !prof.as_ref().and_then(|p| p.no_deploy).unwrap_or(false),
                    build: !no_build && !prof.as_ref().and_then(|p| p.no_build).unwrap_or(false),
                    filter_pattern: filter_pattern
                        .or_else(|| prof.as_ref().and_then(|p| p.filter_pattern.clone())),
                    output_file: output_file.or_else(|| {
                        prof.as_ref()
                            .and_then(|p| p.output_file.as_ref())
                            .map(PathBuf::from)
                    }),
                    capture_mode: capture_mode.unwrap_or_else(|| {
                        if capture_on_failure
                            || prof
                                .as_ref()
                                .and_then(|p| p.capture_on_failure)
                                .unwrap_or(false)
                        {
                            CaptureMode::Failure
                        } else {
                            CaptureMode::Off
                        }
                    }),
                    capture_interval_secs: capture_interval,
                    screenshot_backend: resolved_screenshot_backend,
                    visual_validator,
                    visual_config,
                    scenes,
                    scene_vm,
                    record_video,
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
                    env: resolve_run_env(prof.as_ref(), &config),
                }
            })
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
                let profile =
                    config::lookup_chaos_profile(&project_root, name).ok_or_else(|| {
                        anyhow::anyhow!("Chaos profile '{name}' not found in steampipe.toml")
                    })?;
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
                history::export(
                    &config.state_dir,
                    last,
                    fmt,
                    output.as_deref(),
                    &config.exit_codes,
                )?;
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
        Commands::Screenshot {
            output,
            scene,
            screenshot_backend,
            validate,
        } => {
            let output_dir = output.unwrap_or_else(|| {
                let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
                project_root.join(format!("screenshots/{ts}"))
            });
            let (validator, golden) = if validate {
                match select_validator_plan(&project_root, None)? {
                    ValidatorPlan::Shell(validator) => (Some(validator), None),
                    ValidatorPlan::Golden(visual) => (None, Some(visual)),
                    ValidatorPlan::None => {
                        let project_config = config::load_project_config(&project_root)
                            .ok_or_else(|| anyhow::anyhow!("--validate requires steampipe.toml"))?;
                        (Some(select_visual_validator(project_config)?), None)
                    }
                }
            } else {
                (None, None)
            };
            let window_target = golden.as_ref().and_then(|visual| {
                scene.as_deref().and_then(|scene_name| {
                    visual
                        .scene
                        .iter()
                        .find(|scene| scene.name == scene_name)
                        .and_then(|scene| scene.window.clone())
                })
            });
            let options = capture::ScreenshotOptions {
                backend: screenshot_backend,
                validator,
                golden,
                scene,
                run_label: None,
                window_target,
                allow_vnc_input: false,
            };
            capture::screenshot_all(&config, &output_dir, &options).await?;
        }
        Commands::Accounts => accounts::show(&config).await?,
        Commands::Visual { action } => match action {
            VisualAction::List => unreachable!(),
            VisualAction::Capture { scene, vm, output } => {
                capture_visual_scene(
                    &config,
                    &project_root,
                    &scene,
                    vm.as_deref(),
                    output.as_deref(),
                )
                .await?
            }
            VisualAction::Record { args, force } => {
                record_visual_scene(&config, &project_root, &args, force).await?
            }
            VisualAction::Bless { scene, from, yes } => {
                bless_visual_scene(&project_root, &scene, from.as_deref(), yes)?
            }
            VisualAction::Diff { scene, from } => {
                diff_visual_scene(&project_root, &scene, from.as_deref())?
            }
            VisualAction::Report {
                run_label,
                output,
                single_file,
            } => {
                let index = ui::report::generate(
                    &config,
                    &project_root,
                    run_label.as_deref(),
                    output.as_deref(),
                    single_file,
                )?;
                println!("{}", index.display());
            }
        },
        Commands::CleanLogins {
            target,
            login_state_dir,
        } => {
            steam::clean_logins(&config.vms, login_state_dir.as_deref(), target.as_deref())?;
        }
        Commands::Watch { interval } => watch::run(&config, interval).await?,
        Commands::Doctor { fix } => preflight::doctor(&config, fix).await?,
        Commands::Init
        | Commands::YhConfig { .. }
        | Commands::Completions { .. }
        | Commands::Mcp => {
            unreachable!()
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn profile_with_backend(backend: &str) -> core::config::TestProfile {
        core::config::TestProfile {
            screenshot_backend: Some(backend.into()),
            ..Default::default()
        }
    }

    #[test]
    fn profile_screenshot_backend_resolves_vnc_and_grim_aliases() {
        assert!(matches!(
            resolve_screenshot_backend_from_profile(Some(&profile_with_backend("vnc"))),
            ScreenshotBackend::Vnc
        ));
        assert!(matches!(
            resolve_screenshot_backend_from_profile(Some(&profile_with_backend("wayland"))),
            ScreenshotBackend::Grim
        ));
        assert!(matches!(
            resolve_screenshot_backend_from_profile(None),
            ScreenshotBackend::Grim
        ));
    }

    #[test]
    fn select_visual_validator_requires_exactly_one_profile_validator() {
        let none = core::config::ProjectConfig {
            profile: Some(HashMap::new()),
            ..Default::default()
        };
        let err = select_visual_validator(none).unwrap_err().to_string();
        assert!(err.contains("exactly one profile visual_validator"));

        let mut one_profiles = HashMap::new();
        one_profiles.insert(
            "uifull".into(),
            core::config::TestProfile {
                visual_validator: Some("validator-one".into()),
                ..Default::default()
            },
        );
        let one = core::config::ProjectConfig {
            profile: Some(one_profiles),
            ..Default::default()
        };
        assert_eq!(select_visual_validator(one).unwrap(), "validator-one");

        let mut multiple_profiles = HashMap::new();
        multiple_profiles.insert(
            "a".into(),
            core::config::TestProfile {
                visual_validator: Some("validator-a".into()),
                ..Default::default()
            },
        );
        multiple_profiles.insert(
            "b".into(),
            core::config::TestProfile {
                visual_validator: Some("validator-b".into()),
                ..Default::default()
            },
        );
        let multiple = core::config::ProjectConfig {
            profile: Some(multiple_profiles),
            ..Default::default()
        };
        let err = select_visual_validator(multiple).unwrap_err().to_string();
        assert!(err.contains("multiple profile visual_validator"));
    }
}
