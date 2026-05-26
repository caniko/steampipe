use std::io::{self, BufRead, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::core::backend::{Backend, microvm_log_path};
use crate::core::config::{BridgeReady, ClusterConfig, Unchecked, VmDef, par_each_vm};
use crate::core::credentials::{CredentialsMap, VmCredentials};
use crate::core::nixos_module::{Discovery, NixosModuleConfig, RowStatus};
use crate::vm::lease;

const LOGIN_SSH_TIMEOUT_SECS: u32 = 180;

/// Shell snippet that ensures weston (headless) is running and exports display vars.
/// Append your own commands after this to run under the compositor.
pub fn weston_setup(vm_user: &str) -> String {
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p $XDG_RUNTIME_DIR
chmod 700 $XDG_RUNTIME_DIR 2>/dev/null || true
if pgrep -x sway >/dev/null; then
    pkill -x sway 2>/dev/null || true
    sleep 1
fi
if ! pgrep -x weston >/dev/null; then
    rm -f "$XDG_RUNTIME_DIR"/wayland-* "$XDG_RUNTIME_DIR"/wayland-*.lock 2>/dev/null || true
    nohup weston --backend=headless --no-config >"$XDG_RUNTIME_DIR/weston.log" 2>&1 &
    sleep 2
fi
if ! pgrep -x weston >/dev/null; then
    echo "weston failed to start"
    if [ -f "$XDG_RUNTIME_DIR/weston.log" ]; then
        tail -n 80 "$XDG_RUNTIME_DIR/weston.log"
    fi
    exit 1
fi
export WAYLAND_DISPLAY=wayland-1
"#
    )
}

/// Shell snippet that ensures sway is running and exports display vars.
pub fn sway_setup(vm_user: &str) -> String {
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p $XDG_RUNTIME_DIR
chmod 700 $XDG_RUNTIME_DIR 2>/dev/null || true
unset WAYLAND_DISPLAY WAYLAND_SOCKET DISPLAY
if pgrep -x weston >/dev/null; then
    pkill -x weston 2>/dev/null || true
    sleep 1
fi
if ! pgrep -x sway >/dev/null; then
    rm -f "$XDG_RUNTIME_DIR"/wayland-* "$XDG_RUNTIME_DIR"/wayland-*.lock 2>/dev/null || true
    nohup env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        sway >"$XDG_RUNTIME_DIR/sway.log" 2>&1 &
    sleep 2
fi
if ! pgrep -x sway >/dev/null; then
    echo "sway failed to start"
    if [ -f "$XDG_RUNTIME_DIR/sway.log" ]; then
        tail -n 80 "$XDG_RUNTIME_DIR/sway.log"
    fi
    exit 1
fi
export WAYLAND_DISPLAY=wayland-1
for socket in "$XDG_RUNTIME_DIR"/sway-ipc.*.sock; do
    if [ -S "$socket" ]; then
        export SWAYSOCK="$socket"
        break
    fi
done
if command -v swaymsg >/dev/null && [ -n "${{SWAYSOCK:-}}" ]; then
    if ! swaymsg -t get_outputs >/dev/null 2>&1; then
        echo "sway IPC failed readiness check"
        if [ -f "$XDG_RUNTIME_DIR/sway.log" ]; then
            tail -n 80 "$XDG_RUNTIME_DIR/sway.log"
        fi
        exit 1
    fi
fi
"#
    )
}

/// Shell snippet that ensures weston is running on the virtio-gpu DRM device.
///
/// Uses the GL renderer so Wayland clients can create a virgl-backed EGL
/// display. Pixman can start the compositor, but Mesa's virtio/virgl Wayland
/// platform fails to initialize against a pixman Weston session and falls back
/// to llvmpipe when the virgl driver is not forced.
pub fn weston_gpu_setup(vm_user: &str) -> String {
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p $XDG_RUNTIME_DIR
chmod 700 $XDG_RUNTIME_DIR 2>/dev/null || true
if pgrep -x sway >/dev/null; then
    pkill -x sway 2>/dev/null || true
    sleep 1
fi
if ! pgrep -x weston >/dev/null; then
    rm -f "$XDG_RUNTIME_DIR"/wayland-* "$XDG_RUNTIME_DIR"/wayland-*.lock 2>/dev/null || true
    nohup weston --renderer=gl --no-config >"$XDG_RUNTIME_DIR/weston.log" 2>&1 &
    sleep 2
fi
if ! pgrep -x weston >/dev/null; then
    echo "weston failed to start"
    if [ -f "$XDG_RUNTIME_DIR/weston.log" ]; then
        tail -n 80 "$XDG_RUNTIME_DIR/weston.log"
    fi
    exit 1
fi
export WAYLAND_DISPLAY=wayland-1
"#
    )
}

/// Shell snippet that ensures sway is running on the virtio-gpu DRM device.
pub fn sway_gpu_setup(vm_user: &str) -> String {
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p $XDG_RUNTIME_DIR
chmod 700 $XDG_RUNTIME_DIR 2>/dev/null || true
unset WAYLAND_DISPLAY WAYLAND_SOCKET DISPLAY
if pgrep -x weston >/dev/null; then
    pkill -x weston 2>/dev/null || true
    sleep 1
fi
if ! pgrep -x sway >/dev/null; then
    rm -f "$XDG_RUNTIME_DIR"/wayland-* "$XDG_RUNTIME_DIR"/wayland-*.lock 2>/dev/null || true
    nohup env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WLR_BACKENDS=drm WLR_RENDERER=gles2 WLR_LIBINPUT_NO_DEVICES=1 \
        sway >"$XDG_RUNTIME_DIR/sway.log" 2>&1 &
    sleep 2
fi
if ! pgrep -x sway >/dev/null; then
    echo "sway failed to start"
    if [ -f "$XDG_RUNTIME_DIR/sway.log" ]; then
        tail -n 80 "$XDG_RUNTIME_DIR/sway.log"
    fi
    exit 1
fi
export WAYLAND_DISPLAY=wayland-1
for socket in "$XDG_RUNTIME_DIR"/sway-ipc.*.sock; do
    if [ -S "$socket" ]; then
        export SWAYSOCK="$socket"
        break
    fi
done
if command -v swaymsg >/dev/null && [ -n "${{SWAYSOCK:-}}" ]; then
    if ! swaymsg -t get_outputs >/dev/null 2>&1; then
        echo "sway IPC failed readiness check"
        if [ -f "$XDG_RUNTIME_DIR/sway.log" ]; then
            tail -n 80 "$XDG_RUNTIME_DIR/sway.log"
        fi
        exit 1
    fi
fi
"#
    )
}

/// Return the compositor setup snippet for a display mode (empty for Headless).
pub fn compositor_setup(mode: crate::cli::DisplayMode, vm_user: &str) -> String {
    match mode {
        crate::cli::DisplayMode::Headless => String::new(),
        crate::cli::DisplayMode::Weston => weston_setup(vm_user),
        crate::cli::DisplayMode::Sway => sway_setup(vm_user),
        crate::cli::DisplayMode::WestonGpu => weston_gpu_setup(vm_user),
        crate::cli::DisplayMode::SwayGpu => sway_gpu_setup(vm_user),
    }
}

/// Return a compositor snippet suitable for Steam itself.
///
/// Headless game runs still need a headless Sway session for Steam
/// login and IPC readiness; the game process can still receive `--headless`.
pub fn steam_compositor_setup(mode: crate::cli::DisplayMode, vm_user: &str) -> String {
    match mode {
        crate::cli::DisplayMode::Headless => sway_setup(vm_user),
        _ => compositor_setup(mode, vm_user),
    }
}

/// Shell snippet that starts Steam silently if not already running.
pub const STEAM_START_SILENT: &str = r#"
if pgrep -x steam >/dev/null; then
    echo "already-running"
else
    if [ -n "${SWAYSOCK:-}" ] && command -v swaymsg >/dev/null; then
        swaymsg exec "sh -lc 'steam -silent -cef-disable-gpu >/tmp/steampipe-steam-start.log 2>&1'" >/dev/null
    else
        nohup steam -silent -cef-disable-gpu >/tmp/steampipe-steam-start.log 2>&1 &
    fi
    echo "started"
fi
"#;

pub const STEAM_READY_LOG_PATTERN: &str = "Logged On.*processing complete";

fn ensure_steam_script(vm_user: &str, compositor: &str) -> String {
    let log = format!("/home/{vm_user}/.local/share/Steam/logs/connection_log.txt");
    let ready_pattern = STEAM_READY_LOG_PATTERN;
    format!(
        r#"{compositor}
log="{log}"
mkdir -p "$(dirname "$log")"
wait_for_steam_ready() {{
    for i in $(seq 1 60); do
        if grep -q '{ready_pattern}' "$log" 2>/dev/null; then
            echo STEAM_READY
            return 0
        fi
        sleep 1
    done
    return 1
}}

if pgrep -x steam >/dev/null; then
    if wait_for_steam_ready; then
        exit 0
    fi
    echo "STALE_STEAM_RESTARTING"
    pkill -TERM -x steam 2>/dev/null || true
    sleep 3
    pkill -KILL -x steam 2>/dev/null || true
fi

: > "$log" 2>/dev/null || true
if [ -n "${{SWAYSOCK:-}}" ] && command -v swaymsg >/dev/null; then
    swaymsg exec "sh -lc 'steam -silent -cef-disable-gpu >/tmp/steampipe-steam-start.log 2>&1'" >/dev/null
else
    nohup steam -silent -cef-disable-gpu >/tmp/steampipe-steam-start.log 2>&1 &
fi
if wait_for_steam_ready; then
    exit 0
fi

echo STEAM_TIMEOUT
echo "--- steam processes ---"
pgrep -af steam || true
echo "--- connection log tail ---"
tail -n 80 "$log" 2>/dev/null || true
echo "--- steam start log tail ---"
tail -n 80 /tmp/steampipe-steam-start.log 2>/dev/null || true
if [ -n "${{SWAYSOCK:-}}" ] && command -v swaymsg >/dev/null; then
    echo "--- sway tree ---"
    swaymsg -t get_tree 2>/dev/null || true
fi
exit 1
"#
    )
}

/// Ensure compositor + Steam are running on an instance, waiting for Steam's connection log.
///
/// `compositor` is a shell snippet that starts the compositor (from [`compositor_setup`]).
/// Pass an empty string if the compositor is already running.
pub async fn ensure_steam(
    backend: &Backend,
    ip: &str,
    vm_user: &str,
    compositor: &str,
) -> anyhow::Result<()> {
    let cmd = ensure_steam_script(vm_user, compositor);
    let result = backend.run_cmd(ip, &cmd).await;
    if !result.success || !result.stdout.contains("STEAM_READY") {
        anyhow::bail!(
            "Steam failed on {ip}: stdout:\n{}\nstderr:\n{}",
            result.stdout,
            result.stderr
        );
    }
    Ok(())
}

/// Stop Steam gracefully so it can flush rotated session tokens to disk.
///
/// Sends `steam -shutdown` (Steam's IPC shutdown command), polls for up to 30 s
/// for the process to exit, then escalates through SIGTERM and SIGKILL.
/// Use this whenever Steam exits a successful session. Do not use this in
/// error-recovery paths where Steam is suspected wedged (see
/// [`ensure_steam_script`]).
pub async fn graceful_stop_steam(backend: &Backend, ip: &str, vm_user: &str) -> anyhow::Result<()> {
    let script = graceful_stop_steam_script(vm_user);
    let result = backend.run_cmd(ip, &script).await;
    if !result.success {
        anyhow::bail!(
            "graceful_stop_steam failed on {ip}: stdout: {} stderr: {}",
            result.stdout,
            result.stderr,
        );
    }
    Ok(())
}

fn graceful_stop_steam_script(vm_user: &str) -> String {
    format!(
        r#"export HOME=/home/{vm_user}
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
if ! pgrep -x steam >/dev/null 2>&1; then
    echo STEAM_ALREADY_STOPPED
    exit 0
fi
steam -shutdown >/dev/null 2>&1 || true
for i in $(seq 1 30); do
    if ! pgrep -x steam >/dev/null 2>&1; then
        echo "STEAM_GRACEFUL_EXIT_AT_$i"
        exit 0
    fi
    sleep 1
done
pkill -TERM -x steam 2>/dev/null || true
for i in $(seq 1 5); do
    if ! pgrep -x steam >/dev/null 2>&1; then
        echo "STEAM_TERM_EXIT_AT_$i"
        exit 0
    fi
    sleep 1
done
pkill -KILL -x steam 2>/dev/null || true
echo STEAM_KILLED
"#
    )
}

/// Start weston + Steam on target instances (uses already-running cluster instances).
pub async fn start<S>(
    config: &ClusterConfig<S>,
    target: Option<&str>,
    display: crate::cli::DisplayMode,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, false)?;
    let backend = config.backend.clone();

    if display.requires_gpu() {
        crate::vm::preflight::ensure_vm_gpu(&backend, &targets, display).await?;
    }

    println!(
        "==> Starting Steam on {} VM(s) ({display})...",
        targets.len()
    );
    let script = format!(
        "{}{}",
        steam_compositor_setup(display, &config.vm_user),
        STEAM_START_SILENT,
    );

    let results = par_each_vm(&targets, |vm| {
        let backend = backend.clone();
        let script = script.clone();
        async move {
            let result = backend.run_cmd(&vm.ip, &script).await;
            let status = result.stdout.trim().to_string();
            (vm.name, status)
        }
    })
    .await?;

    for (name, status) in results {
        println!("  {name}: {status}");
    }
    println!("==> Done");
    Ok(())
}

/// Run the `steam check` subcommand: boot each instance, verify Steam login + health.
/// Requires `BridgeReady` — starts and stops instances, which needs the bridge.
pub async fn check(
    config: &ClusterConfig<BridgeReady>,
    target: Option<&str>,
    runners_dir: &Path,
    display: crate::cli::DisplayMode,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, false)?;
    let backend = &config.backend;

    println!("==> Checking Steam setup on VMs (boot -> check -> shutdown)...");

    let mut passed = Vec::new();
    let mut failed = Vec::new();

    for vm in &targets {
        println!("\n── {} ({}) ──", vm.name, vm.ip);

        backend.stop_instance(config, vm);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        print!("  Boot: ");
        crate::core::admission::admit().await;
        if let Err(e) = backend.start_instance(config, vm, runners_dir) {
            println!("FAIL ({e})");
            failed.push(vm.name.clone());
            continue;
        }
        if !backend.wait_ready(&vm.ip, 30).await {
            println!("FAIL (SSH timeout)");
            backend.stop_instance(config, vm);
            failed.push(vm.name.clone());
            continue;
        }
        println!("OK");

        let compositor = steam_compositor_setup(display, &config.vm_user);
        let identity = backend
            .run_cmd(&vm.ip, &steam_identity_probe_script(&config.vm_user))
            .await;
        let (persona, has_cached_login) = parse_steam_identity(identity.stdout.trim());
        let steam_ready = ensure_steam(backend, &vm.ip, &config.vm_user, &compositor).await;

        if has_cached_login {
            println!("  Login: OK ({})", persona.as_deref().unwrap_or("unknown"));
        } else {
            println!("  Login: FAIL");
        }

        if let Err(error) = steam_ready {
            println!("  Steam: FAIL");
            println!("  Diagnostics: {error}");
            failed.push(vm.name.clone());
        } else if has_cached_login {
            println!("  Steam: OK");
            passed.push(vm.name.clone());
        } else {
            println!("  Steam: FAIL (ready but cached login metadata missing)");
            failed.push(vm.name.clone());
        }

        backend.stop_instance(config, vm);
    }

    println!("\n════════════════════════════════════════");
    if !passed.is_empty() {
        println!("  PASSED: {}", passed.join(" "));
    }
    if !failed.is_empty() {
        println!("  FAILED: {}", failed.join(" "));
        println!();
        println!(
            "  To fix 'not-logged-in': run `nix run .#cluster-steam-login` (or `cluster-ctl steam login`)"
        );
        anyhow::bail!("Some VMs failed checks");
    } else {
        println!("  All VMs ready for testing");
    }
    Ok(())
}

/// Boot each target VM, ensure Steam is logged on, gracefully shut down, and stop the VM.
/// Used to keep refresh tokens rotating ahead of expiry.
/// Requires `BridgeReady` — starts and stops instances.
pub async fn warm(
    config: &ClusterConfig<BridgeReady>,
    target: Option<&str>,
    continue_from: bool,
    runners_dir: &Path,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, continue_from)?;
    let backend = &config.backend;

    println!("==> Warming Steam on {} VM(s)...", targets.len());

    let mut warmed = Vec::new();
    let mut failed = Vec::new();

    for vm in &targets {
        println!("\n── {} ({}) ──", vm.name, vm.ip);

        backend.stop_instance(config, vm);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        print!("  Boot: ");
        crate::core::admission::admit().await;
        if let Err(e) = backend.start_instance(config, vm, runners_dir) {
            println!("FAIL ({e})");
            failed.push(vm.name.clone());
            continue;
        }
        if !backend.wait_ready(&vm.ip, 30).await {
            println!("FAIL (SSH timeout)");
            backend.stop_instance(config, vm);
            failed.push(vm.name.clone());
            continue;
        }
        println!("OK");

        let compositor = steam_compositor_setup(crate::cli::DisplayMode::Headless, &config.vm_user);
        match ensure_steam(backend, &vm.ip, &config.vm_user, &compositor).await {
            Ok(()) => println!("  Steam ready: OK"),
            Err(e) => {
                println!("  Steam ready: FAIL ({e})");
                backend.stop_instance(config, vm);
                failed.push(vm.name.clone());
                continue;
            }
        }

        if let Err(e) = graceful_stop_steam(backend, &vm.ip, &config.vm_user).await {
            println!("  Steam stop: WARN ({e})");
        }
        backend.stop_instance(config, vm);
        warmed.push(vm.name.clone());
    }

    println!("\n════════════════════════════════════════");
    if !warmed.is_empty() {
        println!("  WARMED: {}", warmed.join(" "));
    }
    if !failed.is_empty() {
        println!("  FAILED: {}", failed.join(" "));
        anyhow::bail!("Some VMs failed to warm");
    }
    Ok(())
}

fn steam_identity_probe_script(vm_user: &str) -> String {
    format!(
        r#"HOME=/home/{vm_user}
LOGIN_FILE="$HOME/.local/share/Steam/config/loginusers.vdf"
if [ -f "$LOGIN_FILE" ]; then
    PERSONA=$(grep -oP '"PersonaName"\s+"\K[^"]+' "$LOGIN_FILE" 2>/dev/null | head -1)
    STEAMID=$(grep -oP '^\s*"\K[0-9]{{17}}' "$LOGIN_FILE" 2>/dev/null | head -1)
    if [ -n "$STEAMID" ]; then
        echo "LOGIN:${{PERSONA:-unknown}}:$STEAMID"
        exit 0
    fi
fi
echo "NO_LOGIN""#
    )
}

fn parse_steam_identity(output: &str) -> (Option<String>, bool) {
    if let Some(rest) = output.strip_prefix("LOGIN:") {
        let persona = rest
            .split(':')
            .next()
            .filter(|value| !value.is_empty() && *value != "unknown")
            .map(ToOwned::to_owned);
        (persona, true)
    } else {
        (None, false)
    }
}

/// Run the `steam login` subcommand: interactive per-instance Steam login wizard.
/// Requires `BridgeReady` — boots instances to perform login.
pub async fn login(
    config: &ClusterConfig<BridgeReady>,
    target: Option<&str>,
    continue_from: bool,
    login_runners_dir: &Path,
    creds: Option<&CredentialsMap>,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, continue_from)?;

    let mut failed = 0usize;
    for vm in &targets {
        let vm_creds = creds.and_then(|c| c.get::<str>(&vm.name));
        if let Err(e) = login_single_vm(config, vm, login_runners_dir, vm_creds).await {
            eprintln!("Error with {}: {e}", vm.name);
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed}/{} Steam login target(s) failed", targets.len());
    }
    Ok(())
}

/// Submit a Steam Guard code to a running SteamCMD login session, then finish
/// syncing and validating the GUI Steam session.
pub async fn guard<S>(
    config: &ClusterConfig<S>,
    target: &str,
    code: Option<&str>,
    creds: Option<&CredentialsMap>,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(Some(target), false)?;
    if targets.len() != 1 {
        anyhow::bail!("`steam guard` requires exactly one target VM");
    }
    let vm = &targets[0];
    let Some(vm_creds) = creds.and_then(|c| c.get::<str>(&vm.name)) else {
        anyhow::bail!(
            "credentials for {} are required to finish Steam GUI validation",
            vm.name
        );
    };
    let backend = &config.backend;
    if !backend.is_reachable(&vm.ip).await {
        anyhow::bail!("{} is not reachable over SSH", vm.name);
    }
    let password_secrets = [&vm_creds.steam_pass as &str];

    match wait_for_steamcmd_bootstrap(
        backend,
        vm,
        &config.vm_user,
        &config.state_dir,
        &password_secrets,
    )
    .await?
    {
        SteamCmdWait::Complete => {
            println!(
                "==> SteamCMD already completed for {}; resuming validation...",
                vm.name
            );
            finish_steam_login(
                backend,
                vm,
                vm_creds,
                &config.vm_user,
                &config.state_dir,
                &password_secrets,
            )
            .await?;
            shutdown_steam_login_vm(backend, config, vm).await;
            return Ok(());
        }
        SteamCmdWait::GuardRequired => {}
    }

    let code = match code {
        Some(code) => code.trim().to_owned(),
        None => read_guard_code_from_stdin()?,
    };
    if code.is_empty() {
        anyhow::bail!("Steam Guard code cannot be empty");
    }

    println!("==> Submitting Steam Guard code to {}...", vm.name);
    let submit = backend
        .run_cmd(
            &vm.ip,
            &steamcmd_guard_submit_script(&config.vm_user, &code),
        )
        .await;
    let secrets = [&vm_creds.steam_pass as &str, code.as_str()];
    if !submit.success || !steamcmd_guard_submitted(&submit.stdout) {
        let stdout = redact_sensitive(&submit.stdout, &secrets);
        let stderr = redact_sensitive(&submit.stderr, &secrets);
        if stdout.trim().is_empty() && stderr.trim().is_empty() {
            let tail =
                vm_log_tail(&config.state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
            anyhow::bail!(
                "{}: Steam Guard submit produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
                vm.name
            );
        }
        anyhow::bail!(
            "{}: Steam Guard submit failed. stdout: {} stderr: {}",
            vm.name,
            stdout,
            stderr
        );
    }

    match wait_for_steamcmd_guard_completion(
        backend,
        vm,
        &config.vm_user,
        &config.state_dir,
        &secrets,
    )
    .await?
    {
        SteamCmdWait::Complete => {}
        SteamCmdWait::GuardRequired => {
            anyhow::bail!(
                "{}: Steam Guard code was rejected or superseded by a newer code. The VM was left running; retry with the latest code.",
                vm.name
            );
        }
    }
    finish_steam_login(
        backend,
        vm,
        vm_creds,
        &config.vm_user,
        &config.state_dir,
        &secrets,
    )
    .await?;

    shutdown_steam_login_vm(backend, config, vm).await;

    Ok(())
}

async fn shutdown_steam_login_vm<S>(backend: &Backend, config: &ClusterConfig<S>, vm: &VmDef) {
    println!("  Shutting down Steam and VM...");
    let _ = graceful_stop_steam(backend, &vm.ip, &config.vm_user).await;
    backend
        .run_cmd(
            &vm.ip,
            "pkill -x wayvnc 2>/dev/null; pkill -x sway 2>/dev/null; pkill -x tmux 2>/dev/null",
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    backend.stop_instance(config, vm);
    lease::remove_claim(vm.index, &config.lock_dir);
    println!("  {} done.", vm.name);
}

fn read_guard_code_from_stdin() -> anyhow::Result<String> {
    println!("Steam Guard code:");
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    Ok(code.trim().to_owned())
}

async fn login_single_vm(
    config: &ClusterConfig<BridgeReady>,
    vm: &VmDef,
    login_runners_dir: &Path,
    creds: Option<&VmCredentials>,
) -> anyhow::Result<()> {
    let automated = creds.is_some();
    let backend = &config.backend;

    println!();
    println!("══════════════════════════════════════════════════");
    println!("  Steam login for {} ({})", vm.name, vm.ip);
    if automated {
        println!("  Mode: automated (credentials from TOML)");
    } else {
        println!("  Mode: interactive (VNC)");
        println!("  Press Ctrl+C at any time to cancel and shut down the VM");
    }
    println!("══════════════════════════════════════════════════");

    backend.stop_instance(config, vm);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    println!("  Booting VM...");
    crate::core::admission::admit().await;
    let pid = backend.start_instance(config, vm, login_runners_dir)?;
    lease::write_claim(vm.index, &config.lock_dir, &config.cluster_name, pid)?;

    let config_for_cleanup = config.clone();
    let vm_for_cleanup = vm.clone();
    let cleanup = move || {
        config_for_cleanup
            .backend
            .stop_instance(&config_for_cleanup, &vm_for_cleanup);
        lease::remove_claim(vm_for_cleanup.index, &config_for_cleanup.lock_dir);
    };

    print!("  Waiting for SSH... ");
    if !backend.wait_ready(&vm.ip, LOGIN_SSH_TIMEOUT_SECS).await {
        println!("timeout after {LOGIN_SSH_TIMEOUT_SECS}s");
        print_vm_log_tail(&config.state_dir, vm);
        cleanup();
        anyhow::bail!("SSH timeout for {}", vm.name);
    }
    println!("ready");

    let outcome = if let Some(creds) = creds {
        // Automated login via SteamCMD state bootstrap + GUI validation.
        automated_login(backend, vm, creds, &config.vm_user, &config.state_dir).await?
    } else {
        // Interactive VNC-based login (original flow)
        interactive_login(backend, vm, &config.vm_user).await?
    };

    if outcome == LoginOutcome::LeftRunning {
        println!("  {} left running for Steam login completion.", vm.name);
        println!("  After completing Steam login, validate with `cluster-ctl steam accounts`.");
        println!("  Stop the login VM with `cluster-ctl down` when you are done.");
        return Ok(());
    }

    println!("  Shutting down Steam and VM...");
    let _ = graceful_stop_steam(backend, &vm.ip, &config.vm_user).await;
    backend
        .run_cmd(
            &vm.ip,
            "pkill -x wayvnc 2>/dev/null; pkill -x sway 2>/dev/null",
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    backend.stop_instance(config, vm);
    lease::remove_claim(vm.index, &config.lock_dir);
    println!("  {} done.", vm.name);

    Ok(())
}

pub fn vm_log_tail(state_dir: &Path, vm: &VmDef) -> Option<String> {
    let path = microvm_log_path(state_dir, vm);
    let log = read_vm_log_lossy(&path).ok()?;
    // vm.log is host-side hypervisor output; remote command stdout/stderr is
    // still redacted separately before being included in user-facing errors.
    Some(vm_log_tail_lines(&log).join("\n"))
}

fn print_vm_log_tail(state_dir: &Path, vm: &VmDef) {
    let path = microvm_log_path(state_dir, vm);
    let log = match read_vm_log_lossy(&path) {
        Ok(log) => log,
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                println!(
                    "  VM log unavailable: {} (log file does not exist; backend may not have started the VM)",
                    path.display()
                );
            } else {
                println!("  VM log unavailable: {} ({err})", path.display());
            }
            return;
        }
    };
    let tail = vm_log_tail_lines(&log).join("\n");
    if tail.is_empty() {
        println!(
            "  VM log unavailable: {} (log file is empty)",
            path.display()
        );
    } else {
        println!("  --- {} tail ---", path.display());
        for line in tail.lines() {
            println!("  {line}");
        }
        println!("  --- end VM log tail ---");
    }
}

fn read_vm_log_lossy(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn vm_log_tail_lines(log: &str) -> Vec<&str> {
    log.lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Auto-login Steam on already-running VMs using credentials.
/// Called after `vm::up()` to transparently establish Steam sessions.
pub async fn auto_login<S>(
    config: &ClusterConfig<S>,
    vms: &[VmDef],
    creds: &CredentialsMap,
) -> anyhow::Result<()> {
    // Filter to VMs that have credentials
    let with_creds: Vec<_> = vms
        .iter()
        .filter_map(|vm| creds.get::<str>(&vm.name).map(|c| (vm.clone(), c.clone())))
        .collect();

    if with_creds.is_empty() {
        return Ok(());
    }

    println!("==> Auto-login Steam on {} VM(s)...", with_creds.len());
    let backend = config.backend.clone();
    let vm_user = config.vm_user.clone();
    let state_dir = config.state_dir.clone();

    let results = par_each_vm(vms, |vm| {
        let backend = backend.clone();
        let vm_user = vm_user.clone();
        let state_dir = state_dir.clone();
        let creds = creds.clone();
        async move {
            let Some(vm_creds) = creds.get::<str>(&vm.name) else {
                return (vm.name, None);
            };
            match automated_login(&backend, &vm, vm_creds, &vm_user, &state_dir).await {
                Ok(LoginOutcome::Completed) => (vm.name, Some(true)),
                Ok(LoginOutcome::LeftRunning) => {
                    eprintln!(
                        "  {}: Steam Guard required; finish with `steam guard`",
                        vm.name
                    );
                    (vm.name, Some(false))
                }
                Err(e) => {
                    eprintln!("  {}: login failed: {e}", vm.name);
                    (vm.name, Some(false))
                }
            }
        }
    })
    .await?;

    let mut ok = 0;
    let mut fail = 0;
    for (name, result) in &results {
        match result {
            Some(true) => {
                println!("  {name}: logged in");
                ok += 1;
            }
            Some(false) => fail += 1,
            None => {} // no creds for this VM
        }
    }

    if fail > 0 {
        eprintln!(
            "==> {ok}/{} Steam logins succeeded ({fail} failed)",
            ok + fail
        );
    } else {
        println!("==> All {ok} Steam logins succeeded");
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginOutcome {
    Completed,
    LeftRunning,
}

/// Automated login: bootstraps account state with SteamCMD, starts Steam GUI,
/// waits for GUI-side session evidence, and optionally activates a game key.
async fn automated_login(
    backend: &Backend,
    vm: &VmDef,
    creds: &VmCredentials,
    vm_user: &str,
    state_dir: &Path,
) -> anyhow::Result<LoginOutcome> {
    println!("  Logging in as {}...", creds.steam_user);
    println!("  Starting SteamCMD bootstrap session...");

    let steamcmd = steamcmd_bootstrap_command(vm_user, &creds.steam_user, &creds.steam_pass);
    let started = backend.run_cmd(&vm.ip, &steamcmd).await;
    if !started.success || !steamcmd_started(&started.stdout) {
        let stdout = redact_sensitive(&started.stdout, &[&creds.steam_pass]);
        let stderr = redact_sensitive(&started.stderr, &[&creds.steam_pass]);
        if stdout.trim().is_empty() && stderr.trim().is_empty() {
            let tail = vm_log_tail(state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
            anyhow::bail!(
                "{}: SteamCMD bootstrap produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
                vm.name
            );
        }
        anyhow::bail!(
            "{}: SteamCMD bootstrap failed to start. stdout: {} stderr: {}",
            vm.name,
            stdout,
            stderr
        );
    }

    let wait =
        wait_for_steamcmd_bootstrap(backend, vm, vm_user, state_dir, &[&creds.steam_pass]).await?;
    if wait == SteamCmdWait::GuardRequired {
        println!("  Steam Guard required for {}.", creds.steam_user);
        println!(
            "  Complete from another shell with: cluster-ctl steam guard {} --code <CODE>",
            vm.name
        );
        println!(
            "  Or use the project wrapper: nix run .#cluster-steam-guard -- {} --credentials <credentials.toml> --code <CODE>",
            vm.name
        );
        return Ok(LoginOutcome::LeftRunning);
    }

    finish_steam_login(backend, vm, creds, vm_user, state_dir, &[&creds.steam_pass]).await?;
    Ok(LoginOutcome::Completed)
}

fn steamcmd_bootstrap_command(vm_user: &str, steam_user: &str, steam_pass: &str) -> String {
    let home = format!("/home/{vm_user}");
    let user = shell_escape(steam_user);
    let pass = shell_escape(steam_pass);
    format!(
        r#"export HOME='{home}'
SESSION="steampipe-steamcmd-login"
LOG="$HOME/.local/share/Steam/logs/steampipe_steamcmd_login.log"
STATUS="$HOME/.local/share/Steam/logs/steampipe_steamcmd_status"
RUNNER="$HOME/.local/share/Steam/logs/steampipe_steamcmd_login.sh"
mkdir -p "$HOME/.steam/steamcmd" "$HOME/.local/share/Steam/config" "$(dirname "$LOG")"
if ! command -v tmux >/dev/null 2>&1; then
    echo STEAMCMD_TMUX_MISSING
    exit 1
fi
tmux kill-session -t "$SESSION" 2>/dev/null || true
: > "$LOG"
rm -f "$STATUS"
cat > "$RUNNER" <<'STEAMPIPE_STEAMCMD_LOGIN'
#!/bin/sh
export HOME='{home}'
steamcmd +login '{user}' '{pass}' +quit
code=$?
echo "$code" > '{home}/.local/share/Steam/logs/steampipe_steamcmd_status'
echo "STEAMCMD_EXIT:$code"
exit "$code"
STEAMPIPE_STEAMCMD_LOGIN
chmod 700 "$RUNNER"
tmux new-session -d -s "$SESSION" "$RUNNER"
tmux pipe-pane -o -t "$SESSION" "cat >> '$LOG'"
echo STEAMCMD_STARTED"#
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SteamCmdWait {
    Complete,
    GuardRequired,
}

async fn wait_for_steamcmd_bootstrap(
    backend: &Backend,
    vm: &VmDef,
    vm_user: &str,
    state_dir: &Path,
    secrets: &[&str],
) -> anyhow::Result<SteamCmdWait> {
    let wait = backend
        .run_cmd(&vm.ip, &steamcmd_wait_script(vm_user))
        .await;
    let stdout = redact_sensitive(&wait.stdout, secrets);
    let stderr = redact_sensitive(&wait.stderr, secrets);
    if steamcmd_guard_required(&stdout) {
        return Ok(SteamCmdWait::GuardRequired);
    }
    if wait.success && steamcmd_bootstrap_succeeded(&stdout) {
        return Ok(SteamCmdWait::Complete);
    }
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        let tail = vm_log_tail(state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
        anyhow::bail!(
            "{}: SteamCMD bootstrap wait produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
            vm.name
        );
    }
    anyhow::bail!(
        "{}: SteamCMD bootstrap failed. stdout: {} stderr: {}",
        vm.name,
        stdout,
        stderr
    );
}

async fn wait_for_steamcmd_guard_completion(
    backend: &Backend,
    vm: &VmDef,
    vm_user: &str,
    state_dir: &Path,
    secrets: &[&str],
) -> anyhow::Result<SteamCmdWait> {
    let wait = backend
        .run_cmd(&vm.ip, &steamcmd_guard_completion_wait_script(vm_user))
        .await;
    let stdout = redact_sensitive(&wait.stdout, secrets);
    let stderr = redact_sensitive(&wait.stderr, secrets);
    if steamcmd_guard_required(&stdout) {
        return Ok(SteamCmdWait::GuardRequired);
    }
    if wait.success && steamcmd_bootstrap_succeeded(&stdout) {
        return Ok(SteamCmdWait::Complete);
    }
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        let tail = vm_log_tail(state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
        anyhow::bail!(
            "{}: Steam Guard completion wait produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
            vm.name
        );
    }
    anyhow::bail!(
        "{}: SteamCMD bootstrap failed. stdout: {} stderr: {}",
        vm.name,
        stdout,
        stderr
    );
}

fn steamcmd_wait_script(vm_user: &str) -> String {
    format!(
        r#"HOME=/home/{vm_user}
SESSION="steampipe-steamcmd-login"
LOG="$HOME/.local/share/Steam/logs/steampipe_steamcmd_login.log"
STATUS="$HOME/.local/share/Steam/logs/steampipe_steamcmd_status"
for i in $(seq 1 300); do
    if [ -f "$STATUS" ]; then
        code="$(cat "$STATUS" 2>/dev/null || true)"
        if [ "$code" = "0" ]; then
            echo STEAMCMD_BOOTSTRAP_OK
            exit 0
        fi
        echo "STEAMCMD_EXIT:$code"
        echo "--- steamcmd log tail ---"
        tail -n 80 "$LOG" 2>/dev/null || true
        exit 1
    fi
    if grep -q 'Steam Guard code:' "$LOG" 2>/dev/null; then
        echo STEAMCMD_GUARD_REQUIRED
        exit 0
    fi
    if ! tmux has-session -t "$SESSION" 2>/dev/null; then
        echo STEAMCMD_SESSION_ENDED_WITHOUT_STATUS
        echo "SteamCMD has no running login session and no successful status; rerun \`steam login\` to start a new login."
        echo "--- steamcmd log tail ---"
        tail -n 80 "$LOG" 2>/dev/null || true
        exit 1
    fi
    sleep 1
done
echo STEAMCMD_WAIT_TIMEOUT
echo "--- steamcmd log tail ---"
tail -n 80 "$LOG" 2>/dev/null || true
exit 1"#
    )
}

fn steamcmd_guard_submit_script(vm_user: &str, code: &str) -> String {
    let code = shell_escape(code);
    format!(
        r#"HOME=/home/{vm_user}
SESSION="steampipe-steamcmd-login"
if ! tmux has-session -t "$SESSION" 2>/dev/null; then
    echo STEAMCMD_SESSION_NOT_FOUND
    exit 1
fi
LOG="$HOME/.local/share/Steam/logs/steampipe_steamcmd_login.log"
echo STEAMPIPE_GUARD_SUBMIT_MARKER >> "$LOG"
tmux send-keys -t "$SESSION" '{code}' Enter
echo STEAMCMD_GUARD_SUBMITTED"#
    )
}

fn steamcmd_guard_completion_wait_script(vm_user: &str) -> String {
    format!(
        r#"HOME=/home/{vm_user}
SESSION="steampipe-steamcmd-login"
LOG="$HOME/.local/share/Steam/logs/steampipe_steamcmd_login.log"
STATUS="$HOME/.local/share/Steam/logs/steampipe_steamcmd_status"
after_submit_marker() {{
    awk '
        /^STEAMPIPE_GUARD_SUBMIT_MARKER$/ {{ seen = 1; next }}
        seen {{ print }}
    ' "$LOG" 2>/dev/null
}}
for i in $(seq 1 300); do
    if [ -f "$STATUS" ]; then
        code="$(cat "$STATUS" 2>/dev/null || true)"
        if [ "$code" = "0" ]; then
            echo STEAMCMD_BOOTSTRAP_OK
            exit 0
        fi
        echo "STEAMCMD_EXIT:$code"
        echo "--- steamcmd log tail ---"
        tail -n 80 "$LOG" 2>/dev/null || true
        exit 1
    fi
    if after_submit_marker | grep -Eq 'That Steam Guard code was invalid|Steam Guard code:'; then
        echo STEAMCMD_GUARD_REQUIRED
        exit 0
    fi
    if ! tmux has-session -t "$SESSION" 2>/dev/null; then
        echo STEAMCMD_SESSION_ENDED_WITHOUT_STATUS
        echo "SteamCMD has no running login session and no successful status; rerun \`steam login\` to start a new login."
        echo "--- steamcmd log tail ---"
        tail -n 80 "$LOG" 2>/dev/null || true
        exit 1
    fi
    sleep 1
done
echo STEAMCMD_WAIT_TIMEOUT
echo "--- steamcmd log tail ---"
tail -n 80 "$LOG" 2>/dev/null || true
exit 1"#
    )
}

async fn finish_steam_login(
    backend: &Backend,
    vm: &VmDef,
    creds: &VmCredentials,
    vm_user: &str,
    state_dir: &Path,
    secrets: &[&str],
) -> anyhow::Result<()> {
    println!("  Syncing SteamCMD login state into Steam GUI profile...");
    let sync = backend
        .run_cmd(&vm.ip, &steamcmd_state_sync_script(vm_user))
        .await;
    if !sync.success || !steamcmd_sync_succeeded(&sync.stdout) {
        let stdout = redact_sensitive(&sync.stdout, secrets);
        let stderr = redact_sensitive(&sync.stderr, secrets);
        if stdout.trim().is_empty() && stderr.trim().is_empty() {
            let tail = vm_log_tail(state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
            anyhow::bail!(
                "{}: SteamCMD state sync produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
                vm.name
            );
        }
        anyhow::bail!(
            "{}: SteamCMD state sync failed. stdout: {} stderr: {}",
            vm.name,
            stdout,
            stderr
        );
    }

    println!("  Starting Steam GUI for session validation...");
    let validate = backend
        .run_cmd(
            &vm.ip,
            &steam_gui_validation_script(vm_user, &creds.steam_user, &creds.steam_pass),
        )
        .await;
    let output = redact_sensitive(validate.stdout.trim(), secrets);
    let stderr = redact_sensitive(validate.stderr.trim(), secrets);

    if !validate.success || !steam_login_succeeded(&output) {
        if output.is_empty() && stderr.is_empty() {
            let tail = vm_log_tail(state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
            anyhow::bail!(
                "{}: Steam GUI validation produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
                vm.name
            );
        }
        anyhow::bail!(
            "{}: Steam GUI validation failed ({}). stderr: {}. Use VNC fallback if Steam Guard requires GUI confirmation.",
            vm.name,
            output,
            stderr
        );
    }
    println!("  Login successful");

    if let Some(key) = &creds.game_key {
        println!("  Activating game key...");
        let escaped_key = shell_escape(key);
        let activate_cmd = format!(
            r#"steam steam://registerkey/{escaped_key} &
sleep 10
echo KEY_SUBMITTED"#,
        );
        let key_result = backend.run_cmd(&vm.ip, &activate_cmd).await;
        println!(
            "  Game key activation submitted ({})",
            key_result.stdout.trim()
        );
    }

    Ok(())
}

fn steamcmd_state_sync_script(vm_user: &str) -> String {
    format!(
        r#"HOME=/home/{vm_user}
GUI_STEAM="$HOME/.local/share/Steam"
GUI_CONFIG="$GUI_STEAM/config"
LOGIN_FILE="$GUI_CONFIG/loginusers.vdf"
mkdir -p "$GUI_CONFIG"
if [ -s "$LOGIN_FILE" ] && grep -Eq '"[0-9]{{17}}"' "$LOGIN_FILE"; then
    echo STEAMCMD_SYNC_SKIPPED_LOGIN_PRESENT
    exit 0
fi
copied=0
copy_file() {{
    src="$1"
    dest="$2"
    if [ -f "$src" ]; then
        mkdir -p "$dest"
        cp -f "$src" "$dest/"
        copied=$((copied + 1))
        echo "copied:${{src#$HOME/}}"
    fi
}}
for base in "$HOME/.steam/steamcmd" "$HOME/Steam" "$HOME/.local/share/Steam"; do
    copy_file "$base/config/loginusers.vdf" "$GUI_CONFIG"
    copy_file "$base/config/config.vdf" "$GUI_CONFIG"
    copy_file "$base/config/registry.vdf" "$GUI_CONFIG"
    copy_file "$base/registry.vdf" "$GUI_CONFIG"
    for ssfn in "$base"/ssfn*; do
        [ -f "$ssfn" ] || continue
        copy_file "$ssfn" "$GUI_STEAM"
    done
done
if [ "$copied" -eq 0 ]; then
    echo STEAMCMD_SYNC_NO_ARTIFACTS
    exit 1
fi
echo STEAMCMD_SYNC_OK"#
    )
}

fn steam_gui_validation_script(vm_user: &str, steam_user: &str, steam_pass: &str) -> String {
    let log_dir = format!("/home/{vm_user}/.local/share/Steam/logs");
    let log = format!("{log_dir}/connection_log.txt");
    let bootstrap_log = format!("{log_dir}/bootstrap_log.txt");
    let cef_log = format!("{log_dir}/cef_log.txt");
    let console_log = format!("{log_dir}/console-linux.txt");
    let updateui_log = format!("{log_dir}/updateui_child.txt");
    let stdout_log = format!("{log_dir}/steampipe_login_stdout.log");
    let compositor = sway_setup(vm_user);
    let user = shell_escape(steam_user);
    let pass = shell_escape(steam_pass);
    format!(
        r#"{compositor}
mkdir -p {log_dir}
: > {log} 2>/dev/null
: > {bootstrap_log} 2>/dev/null
: > {cef_log} 2>/dev/null
: > {console_log} 2>/dev/null
: > {updateui_log} 2>/dev/null
: > {stdout_log} 2>/dev/null
LOGIN_FILE="/home/{vm_user}/.local/share/Steam/config/loginusers.vdf"
valid_login_file() {{
    [ -s "$LOGIN_FILE" ] || return 1
    grep -Eq '"[0-9]{{17}}"' "$LOGIN_FILE" || return 1
    grep -Eq '"(PersonaName|AccountName)"' "$LOGIN_FILE" || return 1
}}
valid_gui_session_evidence() {{
    grep -q 'PollAuthSessionStatus succeeded and has refresh token' {log} 2>/dev/null || return 1
    grep -Eq "RecvMsgClientLogOnResponse\(\) : \[U:1:[0-9]+\] 'OK'" {log} 2>/dev/null || return 1
    valid_login_file
}}
echo "STEAM_BIN=$(command -v steam || true)"
steam -silent -login '{user}' '{pass}' -cef-disable-gpu >{stdout_log} 2>&1 &
echo "STEAM_START_PID=$!"
for i in $(seq 1 240); do
    if valid_login_file; then
        echo STEAM_LOGIN_OK
        exit 0
    fi
    if valid_gui_session_evidence; then
        echo STEAM_LOGIN_OK
        exit 0
    fi
    sleep 1
done
if grep -q 'Update complete, launching' {bootstrap_log} 2>/dev/null; then
    echo STEAM_LOGIN_RETRY_AFTER_UPDATE
    if pgrep -x steam >/dev/null 2>&1; then
        steam -shutdown >/dev/null 2>&1 || true
        for i in $(seq 1 30); do
            if ! pgrep -x steam >/dev/null 2>&1; then
                echo "STEAM_GRACEFUL_EXIT_AT_$i"
                break
            fi
            sleep 1
        done
        if pgrep -x steam >/dev/null 2>&1; then
            pkill -TERM -x steam 2>/dev/null || true
            for i in $(seq 1 5); do
                if ! pgrep -x steam >/dev/null 2>&1; then
                    echo "STEAM_TERM_EXIT_AT_$i"
                    break
                fi
                sleep 1
            done
        fi
        if pgrep -x steam >/dev/null 2>&1; then
            pkill -KILL -x steam 2>/dev/null || true
            echo STEAM_KILLED
        fi
    else
        echo STEAM_ALREADY_STOPPED
    fi
    : > {log} 2>/dev/null
    : > {cef_log} 2>/dev/null
    : > {stdout_log} 2>/dev/null
    steam -silent -login '{user}' '{pass}' -cef-disable-gpu >{stdout_log} 2>&1 &
    echo "STEAM_RETRY_PID=$!"
    for i in $(seq 1 240); do
        if valid_login_file; then
            echo STEAM_LOGIN_OK
            exit 0
        fi
        if valid_gui_session_evidence; then
            echo STEAM_LOGIN_OK
            exit 0
        fi
        sleep 1
    done
fi
echo STEAM_LOGIN_TIMEOUT
echo "--- steam processes ---"
if pgrep -x steam >/dev/null 2>&1; then
    echo "steam-running"
else
    echo "steam-not-running"
fi
if pgrep -x steamwebhelper >/dev/null 2>&1; then
    echo "steamwebhelper-running"
else
    echo "steamwebhelper-not-running"
fi
echo "--- log directory ---"
ls -la {log_dir} 2>/dev/null || true
echo "--- connection_log.txt tail ---"
tail -n 80 {log} 2>/dev/null || true
echo "--- bootstrap_log.txt tail ---"
tail -n 80 {bootstrap_log} 2>/dev/null || true
echo "--- cef_log.txt tail ---"
tail -n 80 {cef_log} 2>/dev/null || true
echo "--- console-linux.txt tail ---"
tail -n 80 {console_log} 2>/dev/null || true
echo "--- updateui_child.txt tail ---"
tail -n 80 {updateui_log} 2>/dev/null || true
echo "--- steam stdout/stderr tail ---"
tail -n 80 {stdout_log} 2>/dev/null || true"#,
    )
}

/// Interactive VNC-based login (original flow).
async fn interactive_login(
    backend: &Backend,
    vm: &VmDef,
    vm_user: &str,
) -> anyhow::Result<LoginOutcome> {
    println!("  Starting sway + wayvnc + Steam...");
    backend
        .run_cmd(
            &vm.ip,
            &format!(
                r#"
        export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
        mkdir -p $XDG_RUNTIME_DIR
        if ! pgrep -x sway >/dev/null; then
            nohup env WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 sway >$XDG_RUNTIME_DIR/sway.log 2>&1 &
            sleep 2
        fi
        export WAYLAND_DISPLAY=wayland-1
        if ! pgrep -x wayvnc >/dev/null; then
            nohup wayvnc 0.0.0.0 5900 >$XDG_RUNTIME_DIR/wayvnc.log 2>&1 &
            sleep 1
        fi
        if ! pgrep -x steam >/dev/null; then
            nohup steam -cef-disable-gpu >$XDG_RUNTIME_DIR/steam.log 2>&1 &
        fi
        "#
            ),
        )
        .await;

    println!();
    println!("  VNC into:  {}:5900", vm.ip);
    println!("  Complete the Steam login + Steam Guard in the VNC window.");
    println!();
    println!("  Commands:  [p] paste text into VM  [Enter] done  [Ctrl+C] cancel");
    println!();

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    loop {
        let Some(input) = read_stdin_line(&mut stdin)? else {
            println!("  stdin is nonblocking; leaving the VM running for manual VNC login.");
            println!(
                "  VNC into {}:5900, complete Steam login, then run `cluster-ctl steam accounts`.",
                vm.ip
            );
            println!("  Stop the login VM with `cluster-ctl down` when finished.");
            return Ok(LoginOutcome::LeftRunning);
        };
        let input = input.trim();

        if input.is_empty() {
            break;
        } else if input == "p" {
            println!("  Paste text (will be typed into focused VM window):");
            let Some(text) = read_stdin_line(&mut stdin)? else {
                println!("  stdin is nonblocking; paste was not sent.");
                println!("  Continue in VNC, then run `cluster-ctl steam accounts` to validate.");
                return Ok(LoginOutcome::LeftRunning);
            };
            let text = text.trim_end_matches('\n');
            if !text.is_empty() {
                let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
                backend.run_cmd(
                    &vm.ip,
                    &format!(
                        r#"export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}; export WAYLAND_DISPLAY=wayland-1; wtype -- "{escaped}""#
                    ),
                )
                .await;
                println!("  Typed into VM.");
            }
        }
    }

    Ok(LoginOutcome::Completed)
}

fn read_stdin_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut input = String::new();
    match reader.read_line(&mut input) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(input)),
        Err(error) if is_nonblocking_stdin_error(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_nonblocking_stdin_error(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock)
        || error.raw_os_error() == Some(11)
        || error
            .to_string()
            .contains("Resource temporarily unavailable")
}

fn steam_login_succeeded(output: &str) -> bool {
    output.lines().any(|line| line.trim() == "STEAM_LOGIN_OK")
}

fn steamcmd_started(output: &str) -> bool {
    output.lines().any(|line| line.trim() == "STEAMCMD_STARTED")
}

fn steamcmd_bootstrap_succeeded(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim() == "STEAMCMD_BOOTSTRAP_OK")
}

fn steamcmd_guard_required(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim() == "STEAMCMD_GUARD_REQUIRED")
}

fn steamcmd_guard_submitted(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim() == "STEAMCMD_GUARD_SUBMITTED")
}

fn steamcmd_sync_succeeded(output: &str) -> bool {
    output.lines().any(|line| {
        matches!(
            line.trim(),
            "STEAMCMD_SYNC_OK" | "STEAMCMD_SYNC_SKIPPED_LOGIN_PRESENT"
        )
    })
}

fn redact_sensitive(text: &str, secrets: &[&str]) -> String {
    let mut redacted = text.to_owned();
    for secret in secrets {
        if !secret.is_empty() {
            redacted = redacted.replace(secret, "[REDACTED]");
            let escaped = shell_escape(secret);
            if escaped != *secret {
                redacted = redacted.replace(&escaped, "[REDACTED]");
            }
        }
    }
    redacted
}

/// Escape single quotes for safe shell interpolation inside single-quoted strings.
fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Default directory where the NixOS module persists Steam login state.
const DEFAULT_LOGIN_STATE_DIR: &str = "/var/lib/steampipe/logins";

pub fn info(
    discovery: &Discovery,
    host: Option<&NixosModuleConfig>,
    cluster: Option<&ClusterConfig<Unchecked>>,
) -> anyhow::Result<()> {
    match (&discovery.selected, host) {
        (Some(path), Some(host)) => {
            let source_note = if path == Path::new("/etc/steampipe/module.json") {
                format!(
                    " (legacy NixOS-module path; schemaVersion {})",
                    host.schema_version
                )
            } else {
                format!(" (schemaVersion {})", host.schema_version)
            };
            println!("Host config source: {}{source_note}", path.display());
        }
        _ => println!("Host config source: no config discovered"),
    }
    println!();
    println!("Discovery trace:");
    for (index, row) in discovery.rows.iter().enumerate() {
        println!(
            "  {}. {:<52} {}",
            index + 1,
            row.label,
            render_row_status(row)
        );
    }

    let Some(host) = host else {
        return Ok(());
    };

    println!();
    println!("Cluster shape:");
    println!("  {:<15} {}", "vmCount", host.vm_count);
    println!(
        "  {:<15} {} ({}.0/{}, host {})",
        "bridge", host.bridge, host.subnet, host.prefix, host.host_ip
    );
    println!(
        "  {:<15} {}",
        "tapOwner",
        host.tap_owner.as_deref().unwrap_or("none")
    );
    println!(
        "  {:<15} {}",
        "loginStateDir",
        host.login_state_dir.display()
    );
    println!(
        "  {:<15} {}",
        "credentials",
        render_credentials_summary(host.credentials_path.as_deref())
    );

    println!();
    println!("Configured accounts ({}):", host.accounts.len());
    for account in &host.accounts {
        let login_state = render_login_state_summary(&host.login_state_dir.join(&account.vm));
        println!(
            "  {:<8} {:<20} login-state: {}",
            account.vm, account.steam_user, login_state
        );
    }

    if let Some(cluster) = cluster {
        println!();
        let leased = if cluster.vms.is_empty() {
            "(none)".to_string()
        } else {
            cluster
                .vms
                .iter()
                .map(|vm| vm.name.0.clone())
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!(
            "Leased cluster vms (cluster={}): {}",
            cluster.cluster_name, leased
        );
    }

    Ok(())
}

/// Remove persisted Steam login state from the host login state directory.
///
/// `target` selects which VMs to clean: `None` or `Some("all")` cleans every
/// VM subdirectory; a VM name like `"vm-3"` or bare number `"3"` cleans only
/// that one.
pub fn clean_logins(
    vms: &[VmDef],
    login_state_dir: Option<PathBuf>,
    target: Option<&str>,
    host: Option<&NixosModuleConfig>,
) -> anyhow::Result<()> {
    let dir = match login_state_dir {
        Some(path) => path,
        None => match host
            .cloned()
            .or_else(|| crate::core::nixos_module::load().ok().flatten())
        {
            Some(module) => module.login_state_dir,
            None => PathBuf::from(DEFAULT_LOGIN_STATE_DIR),
        },
    };

    if !dir.exists() {
        println!("Login state directory does not exist: {}", dir.display());
        return Ok(());
    }

    let work_set: Vec<String> = if !vms.is_empty() {
        vms.iter().map(|vm| vm.name.0.clone()).collect()
    } else if let Some(host) = host {
        host.accounts
            .iter()
            .map(|account| account.vm.clone())
            .collect()
    } else {
        Vec::new()
    };

    let targets: Vec<String> = match target {
        None | Some("all") => work_set,
        Some(t) => {
            let name = if t.starts_with("vm-") {
                t.to_string()
            } else {
                format!("vm-{t}")
            };
            if work_set.iter().any(|vm| vm == &name) {
                vec![name]
            } else {
                anyhow::bail!("unknown VM: {name}");
            }
        }
    };

    let mut cleaned = 0u32;
    for vm in &targets {
        let vm_dir = dir.join(vm);
        if !vm_dir.exists() {
            println!("  {}: no login state, skipping", vm);
            continue;
        }
        std::fs::remove_dir_all(&vm_dir)?;
        std::fs::create_dir_all(&vm_dir)?;
        println!("  {}: cleaned", vm);
        cleaned += 1;
    }

    if cleaned == 0 {
        println!("Nothing to clean.");
    } else {
        println!("==> Cleaned login state for {cleaned} VM(s)");
    }

    Ok(())
}

fn render_row_status(row: &crate::core::nixos_module::DiscoveryRow) -> String {
    match row.status {
        RowStatus::NotSet => "(not set)".into(),
        RowStatus::NotFound => "(not found)".into(),
        RowStatus::NoXdgHome => "(no XDG home)".into(),
        RowStatus::Selected => "(selected)".into(),
    }
}

fn render_credentials_summary(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return "none".into();
    };
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let mut details = vec!["exists".to_string()];
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                details.push(format!(
                    "mode {:04o}",
                    metadata.permissions().mode() & 0o7777
                ));
            }
            if let Ok(modified) = metadata.modified() {
                details.push(format!("mtime {}", format_date(modified)));
            }
            format!("{} ({})", path.display(), details.join(", "))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            format!("{} (missing)", path.display())
        }
        Err(error) => format!("{} (metadata error: {error})", path.display()),
    }
}

fn render_login_state_summary(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            if let Ok(modified) = metadata.modified() {
                format!("present ({})", format_date(modified))
            } else {
                "present".into()
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => "missing".into(),
        Err(error) => format!("metadata error: {error}"),
    }
}

fn format_date(timestamp: SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Local> = timestamp.into();
    datetime.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{IpAddr, VmName};

    #[test]
    fn shell_escape_no_quotes() {
        assert_eq!(shell_escape("password123"), "password123");
    }

    #[test]
    fn shell_escape_single_quotes() {
        assert_eq!(shell_escape("it's"), "it'\\''s");
    }

    #[test]
    fn shell_escape_empty() {
        assert_eq!(shell_escape(""), "");
    }

    #[test]
    fn vm_log_tail_tolerates_non_utf8_output() {
        let temp = tempfile::tempdir().unwrap();
        let vm = VmDef {
            name: VmName("vm-1".into()),
            ip: IpAddr("10.0.100.1".into()),
            index: 1,
        };
        let log_path = microvm_log_path(temp.path(), &vm);
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        std::fs::write(&log_path, b"boot line\nraw byte: \xff\nssh never came up\n").unwrap();

        let log = read_vm_log_lossy(&log_path).unwrap();

        assert!(log.contains("boot line"));
        assert!(log.contains("raw byte: \u{fffd}"));
        assert_eq!(
            vm_log_tail_lines(&log),
            vec!["boot line", "raw byte: \u{fffd}", "ssh never came up"]
        );
    }

    #[test]
    fn empty_run_cmd_output_falls_back_to_vm_log_tail() {
        let temp = tempfile::tempdir().unwrap();
        let vm = VmDef {
            name: VmName("vm-1".to_string()),
            ip: IpAddr("10.0.100.1".to_string()),
            index: 1,
        };
        let log_path = microvm_log_path(temp.path(), &vm);
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        std::fs::write(&log_path, b"line one\nline two\nlast line\n").unwrap();

        let tail = vm_log_tail(temp.path(), &vm).expect("tail");

        assert!(tail.contains("last line"));
        assert!(tail.contains("line one"));
    }

    #[test]
    fn steam_login_succeeded_requires_exact_marker_line() {
        assert!(steam_login_succeeded("noise\nSTEAM_LOGIN_OK\nmore noise"));
        assert!(!steam_login_succeeded("STEAM_LOGIN_TIMEOUT"));
        assert!(!steam_login_succeeded("NOT_STEAM_LOGIN_OK"));
    }

    #[test]
    fn steamcmd_sync_succeeded_requires_exact_marker_line() {
        assert!(steamcmd_sync_succeeded(
            "copied:.steam/steamcmd/config/config.vdf\nSTEAMCMD_SYNC_OK\n"
        ));
        assert!(steamcmd_sync_succeeded(
            "STEAMCMD_SYNC_SKIPPED_LOGIN_PRESENT\n"
        ));
        assert!(!steamcmd_sync_succeeded("STEAMCMD_SYNC_NO_ARTIFACTS"));
        assert!(!steamcmd_sync_succeeded("NOT_STEAMCMD_SYNC_OK"));
    }

    #[test]
    fn steamcmd_marker_parsers_require_exact_lines() {
        assert!(steamcmd_started("noise\nSTEAMCMD_STARTED\n"));
        assert!(steamcmd_bootstrap_succeeded("STEAMCMD_BOOTSTRAP_OK\n"));
        assert!(steamcmd_guard_required("x\nSTEAMCMD_GUARD_REQUIRED\n"));
        assert!(steamcmd_guard_submitted("STEAMCMD_GUARD_SUBMITTED\n"));
        assert!(!steamcmd_started("NOT_STEAMCMD_STARTED"));
        assert!(!steamcmd_guard_submitted("NOT_STEAMCMD_GUARD_SUBMITTED"));
    }

    #[test]
    fn graceful_stop_script_uses_steam_shutdown_first() {
        let script = graceful_stop_steam_script("chessbender");
        let shutdown_pos = script
            .find("steam -shutdown")
            .expect("steam -shutdown present");
        let pkill_pos = script
            .find("pkill -TERM -x steam")
            .expect("SIGTERM fallback present");
        assert!(
            shutdown_pos < pkill_pos,
            "steam -shutdown must precede pkill -TERM"
        );
        assert!(
            script.contains("pkill -KILL -x steam"),
            "SIGKILL fallback present"
        );
        assert!(script.contains("STEAM_ALREADY_STOPPED"));
    }

    #[test]
    fn steamcmd_command_is_redacted_in_diagnostics() {
        let command = steamcmd_bootstrap_command("chessbender", "account", "pa'ss guard $TOKEN");
        assert!(command.contains("steamcmd +login"));
        assert!(command.contains("'pa'\\''ss guard $TOKEN'"));

        let diagnostic = redact_sensitive(
            &format!("failed command: {command}"),
            &["pa'ss guard $TOKEN"],
        );
        assert!(!diagnostic.contains("pa'ss guard $TOKEN"));
        assert!(diagnostic.contains("[REDACTED]"));
    }

    #[test]
    fn steamcmd_command_uses_tmux_session_and_status_files() {
        let script = steamcmd_bootstrap_command("chessbender", "account", "password");
        assert!(script.contains("SESSION=\"steampipe-steamcmd-login\""));
        assert!(script.contains("tmux new-session -d"));
        assert!(script.contains("tmux pipe-pane"));
        assert!(script.contains("steampipe_steamcmd_status"));
        assert!(script.contains("STEAMCMD_STARTED"));
    }

    #[test]
    fn steamcmd_command_runs_credentials_from_script_not_tmux_shell_string() {
        let script = steamcmd_bootstrap_command("chessbender", "account", "pa$`ss");
        assert!(script.contains("cat > \"$RUNNER\" <<'STEAMPIPE_STEAMCMD_LOGIN'"));
        assert!(script.contains("steamcmd +login 'account' 'pa$`ss' +quit"));
        assert!(script.contains("tmux new-session -d -s \"$SESSION\" \"$RUNNER\""));
        assert!(!script.contains("tmux new-session -d -s \"$SESSION\" \"export HOME"));
    }

    #[test]
    fn steamcmd_guard_submit_uses_tmux_send_keys() {
        let script = steamcmd_guard_submit_script("chessbender", "AB'CDE");
        assert!(script.contains("tmux send-keys"));
        assert!(script.contains("'AB'\\''CDE'"));
        assert!(script.contains("STEAMPIPE_GUARD_SUBMIT_MARKER"));
        assert!(script.contains("STEAMCMD_GUARD_SUBMITTED"));
    }

    #[test]
    fn steamcmd_wait_detects_guard_prompt_and_success() {
        let script = steamcmd_wait_script("chessbender");
        assert!(script.contains("Steam Guard code:"));
        assert!(script.contains("STEAMCMD_GUARD_REQUIRED"));
        assert!(script.contains("STEAMCMD_BOOTSTRAP_OK"));
        assert!(script.contains("STEAMCMD_WAIT_TIMEOUT"));
    }

    #[test]
    fn steamcmd_wait_checks_terminal_status_before_guard_prompt() {
        let script = steamcmd_wait_script("chessbender");
        let status_pos = script.find("if [ -f \"$STATUS\" ]").unwrap();
        let guard_pos = script.find("grep -q 'Steam Guard code:'").unwrap();
        assert!(status_pos < guard_pos);
    }

    #[test]
    fn steamcmd_guard_completion_wait_uses_post_submit_prompt_detection() {
        let script = steamcmd_guard_completion_wait_script("chessbender");
        let status_pos = script.find("if [ -f \"$STATUS\" ]").unwrap();
        let guard_pos = script.find("after_submit_marker | grep").unwrap();
        assert!(status_pos < guard_pos);
        assert!(script.contains("That Steam Guard code was invalid|Steam Guard code:"));
        assert!(script.contains("SteamCMD has no running login session and no successful status"));
    }

    #[test]
    fn steamcmd_state_sync_targets_only_known_gui_artifacts() {
        let script = steamcmd_state_sync_script("chessbender");
        assert!(script.contains("$HOME/.steam/steamcmd"));
        assert!(script.contains("$HOME/.local/share/Steam"));
        assert!(script.contains("config/loginusers.vdf"));
        assert!(script.contains("config/config.vdf"));
        assert!(script.contains("registry.vdf"));
        assert!(script.contains("ssfn*"));
        assert!(script.contains("STEAMCMD_SYNC_OK"));
    }

    #[test]
    fn steamcmd_state_sync_skips_when_login_present() {
        let script = steamcmd_state_sync_script("chessbender");
        assert!(script.contains("STEAMCMD_SYNC_SKIPPED_LOGIN_PRESENT"));
        assert!(script.contains("[ -s \"$LOGIN_FILE\" ]"));
        assert!(script.contains("grep -Eq '\"[0-9]{17}\"' \"$LOGIN_FILE\""));
    }

    #[test]
    fn steam_gui_validation_requires_gui_loginusers_evidence() {
        let script = steam_gui_validation_script("chessbender", "account", "pa'ss");
        assert!(
            script.contains(
                "LOGIN_FILE=\"/home/chessbender/.local/share/Steam/config/loginusers.vdf\""
            )
        );
        assert!(script.contains("valid_login_file"));
        assert!(script.contains("grep -Eq '\"[0-9]{17}\"'"));
        assert!(script.contains("grep -Eq '\"(PersonaName|AccountName)\"'"));
        assert!(script.contains("steam -silent -login 'account' 'pa'\\''ss' -cef-disable-gpu"));
    }

    #[test]
    fn steam_gui_validation_accepts_refresh_token_evidence() {
        let script = steam_gui_validation_script("chessbender", "account", "password");
        assert!(script.contains("valid_gui_session_evidence"));
        assert!(script.contains("PollAuthSessionStatus succeeded and has refresh token"));
        assert!(script.contains("RecvMsgClientLogOnResponse\\(\\) : \\[U:1:[0-9]+\\] 'OK'"));
    }

    #[test]
    fn steam_gui_validation_no_longer_synthesizes_loginusers() {
        let script = steam_gui_validation_script("chessbender", "account", "password");
        let heredoc_marker = ["STEAMPIPE", "_LOGINUSERS"].concat();
        let synthesis_function = ["write_login_file", "_from_connection_log"].concat();
        assert!(!script.contains(&heredoc_marker));
        assert!(!script.contains(&synthesis_function));
    }

    #[test]
    fn steam_check_identity_probe_reads_cached_account_only() {
        let script = steam_identity_probe_script("chessbender");
        assert!(script.contains("loginusers.vdf"));
        assert!(script.contains("echo \"LOGIN:${PERSONA:-unknown}:$STEAMID\""));
        assert!(script.contains("echo \"NO_LOGIN\""));

        assert_eq!(
            parse_steam_identity("LOGIN:test_account:76561198000000000"),
            (Some("test_account".to_owned()), true)
        );
        assert_eq!(
            parse_steam_identity("LOGIN:unknown:76561198000000000"),
            (None, true)
        );
        assert_eq!(parse_steam_identity("NO_LOGIN"), (None, false));
    }

    #[test]
    fn redact_sensitive_ignores_empty_secret() {
        assert_eq!(redact_sensitive("abc", &["", "b"]), "a[REDACTED]c");
    }

    #[test]
    fn nonblocking_stdin_error_is_detected() {
        let error = io::Error::from_raw_os_error(11);
        assert!(is_nonblocking_stdin_error(&error));
    }

    #[test]
    fn read_stdin_line_reads_normal_input() {
        let mut input = io::Cursor::new("p\n");
        assert_eq!(read_stdin_line(&mut input).unwrap().as_deref(), Some("p\n"));
    }

    #[test]
    fn read_stdin_line_treats_eof_as_unavailable() {
        let mut input = io::Cursor::new("");
        assert_eq!(read_stdin_line(&mut input).unwrap(), None);
    }

    // ── Compositor setup scripts ────────────────────────────────────────

    #[test]
    fn weston_setup_uses_headless_backend() {
        let script = weston_setup("testuser");
        assert!(
            script.contains("--backend=headless"),
            "weston_setup must use headless backend"
        );
        assert!(script.contains("pkill -x sway"));
        assert!(script.contains("weston failed to start"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-testuser"));
    }

    #[test]
    fn ensure_steam_script_restarts_stale_steam_before_ready() {
        let script = ensure_steam_script("testuser", "export WAYLAND_DISPLAY=wayland-1\n");
        assert!(script.contains("wait_for_steam_ready"));
        assert!(script.contains("STALE_STEAM_RESTARTING"));
        assert!(script.contains("pkill -TERM -x steam"));
        assert!(script.contains("swaymsg exec"));
        assert!(script.contains("Logged On.*processing complete"));
        assert!(script.contains("connection log tail"));
        assert!(
            !script.contains("pgrep -x steam >/dev/null; then          echo STEAM_READY"),
            "must not accept a running Steam process as ready without log verification"
        );
    }

    #[test]
    fn weston_gpu_setup_no_headless_backend() {
        let script = weston_gpu_setup("testuser");
        assert!(
            !script.contains("--backend=headless"),
            "weston_gpu_setup must NOT use headless backend"
        );
        assert!(script.contains("weston"), "must still launch weston");
        assert!(script.contains("--no-config"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-testuser"));
    }

    #[test]
    fn weston_gpu_setup_uses_gl_renderer() {
        let script = weston_gpu_setup("testuser");
        assert!(
            script.contains("--renderer=gl"),
            "weston_gpu_setup must use the GL renderer so virgl-backed Wayland \
             clients can initialize EGL"
        );
    }

    #[test]
    fn sway_setup_script() {
        let script = sway_setup("testuser");
        assert!(script.contains("sway"));
        assert!(script.contains("pkill -x weston"));
        assert!(script.contains("WLR_BACKENDS=headless"));
        assert!(script.contains("WLR_LIBINPUT_NO_DEVICES=1"));
        assert!(script.contains("sway failed to start"));
        assert!(script.contains("SWAYSOCK"));
        assert!(script.contains("swaymsg -t get_outputs"));
        assert!(script.contains("unset WAYLAND_DISPLAY WAYLAND_SOCKET DISPLAY"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-testuser"));
    }

    #[test]
    fn sway_gpu_setup_sets_drm_backend() {
        let script = sway_gpu_setup("testuser");
        assert!(
            script.contains("WLR_BACKENDS=drm"),
            "sway_gpu_setup must set WLR_BACKENDS=drm"
        );
        assert!(
            script.contains("WLR_RENDERER=gles2"),
            "sway_gpu_setup must use a GPU compositor renderer so Vulkan Wayland clients get present modes"
        );
        assert!(script.contains("pkill -x weston"));
        assert!(script.contains("WLR_LIBINPUT_NO_DEVICES=1"));
        assert!(script.contains("sway failed to start"));
        assert!(script.contains("sway"));
        assert!(script.contains("unset WAYLAND_DISPLAY WAYLAND_SOCKET DISPLAY"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-testuser"));
    }

    #[test]
    fn compositor_setup_headless_returns_empty() {
        let script = compositor_setup(crate::cli::DisplayMode::Headless, "u");
        assert!(script.is_empty());
    }

    #[test]
    fn steam_compositor_setup_headless_uses_sway() {
        let script = steam_compositor_setup(crate::cli::DisplayMode::Headless, "u");
        assert!(script.contains("WLR_BACKENDS=headless"));
        assert!(script.contains("sway"));
        assert!(script.contains("SWAYSOCK"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
    }

    #[test]
    fn compositor_setup_weston_uses_headless() {
        let script = compositor_setup(crate::cli::DisplayMode::Weston, "u");
        assert!(script.contains("--backend=headless"));
    }

    #[test]
    fn compositor_setup_sway() {
        let script = compositor_setup(crate::cli::DisplayMode::Sway, "u");
        assert!(script.contains("sway"));
        assert!(script.contains("WLR_BACKENDS=headless"));
    }

    #[test]
    fn compositor_setup_weston_gpu_no_headless() {
        let script = compositor_setup(crate::cli::DisplayMode::WestonGpu, "u");
        assert!(script.contains("weston"));
        assert!(!script.contains("--backend=headless"));
    }

    #[test]
    fn compositor_setup_sway_gpu_drm() {
        let script = compositor_setup(crate::cli::DisplayMode::SwayGpu, "u");
        assert!(script.contains("WLR_BACKENDS=drm"));
    }

    #[test]
    fn gpu_setup_uses_correct_user_in_runtime_dir() {
        let weston = weston_gpu_setup("chessbender");
        assert!(weston.contains("/tmp/runtime-chessbender"));
        let sway = sway_gpu_setup("chessbender");
        assert!(sway.contains("/tmp/runtime-chessbender"));
    }

    #[test]
    fn all_compositor_setups_create_runtime_dir() {
        for mode in [
            crate::cli::DisplayMode::Weston,
            crate::cli::DisplayMode::Sway,
            crate::cli::DisplayMode::WestonGpu,
            crate::cli::DisplayMode::SwayGpu,
        ] {
            let script = compositor_setup(mode, "u");
            assert!(
                script.contains("mkdir -p $XDG_RUNTIME_DIR"),
                "{mode} setup must create runtime dir"
            );
        }
    }

    #[test]
    fn all_compositor_setups_are_idempotent() {
        // All non-headless setups check for existing process before starting
        for mode in [
            crate::cli::DisplayMode::Weston,
            crate::cli::DisplayMode::Sway,
            crate::cli::DisplayMode::WestonGpu,
            crate::cli::DisplayMode::SwayGpu,
        ] {
            let script = compositor_setup(mode, "u");
            assert!(
                script.contains("pgrep"),
                "{mode} setup must check for existing process"
            );
        }
    }
}
