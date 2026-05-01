use std::io::{self, BufRead, ErrorKind};
use std::path::Path;

use crate::core::backend::Backend;
use crate::core::config::{BridgeReady, ClusterConfig, VmDef, par_each_vm};
use crate::core::credentials::{CredentialsMap, VmCredentials};

/// Shell snippet that ensures weston (headless) is running and exports display vars.
/// Append your own commands after this to run under the compositor.
pub fn weston_setup(vm_user: &str) -> String {
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p $XDG_RUNTIME_DIR
if ! pgrep -x weston >/dev/null; then
    weston --backend=headless --xwayland --no-config >/dev/null 2>&1 &
    sleep 2
fi
export WAYLAND_DISPLAY=wayland-1
export DISPLAY=:0
"#
    )
}

/// Shell snippet that ensures sway is running and exports display vars.
pub fn sway_setup(vm_user: &str) -> String {
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p $XDG_RUNTIME_DIR
if ! pgrep -x sway >/dev/null; then
    sway >/dev/null 2>&1 &
    sleep 2
fi
export WAYLAND_DISPLAY=wayland-1
export DISPLAY=:0
"#
    )
}

/// Shell snippet that ensures weston is running on the virtio-gpu DRM device.
pub fn weston_gpu_setup(vm_user: &str) -> String {
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p $XDG_RUNTIME_DIR
if ! pgrep -x weston >/dev/null; then
    weston --xwayland --no-config >/dev/null 2>&1 &
    sleep 2
fi
export WAYLAND_DISPLAY=wayland-1
export DISPLAY=:0
"#
    )
}

/// Shell snippet that ensures sway is running on the virtio-gpu DRM device.
pub fn sway_gpu_setup(vm_user: &str) -> String {
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p $XDG_RUNTIME_DIR
if ! pgrep -x sway >/dev/null; then
    WLR_BACKENDS=drm sway >/dev/null 2>&1 &
    sleep 2
fi
export WAYLAND_DISPLAY=wayland-1
export DISPLAY=:0
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

/// Shell snippet that starts Steam silently if not already running.
pub const STEAM_START_SILENT: &str = r#"
if pgrep -x steam >/dev/null; then
    echo "already-running"
else
    steam -silent -cef-disable-gpu >/dev/null 2>&1 &
    echo "started"
fi
"#;

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
    let log = format!("/home/{vm_user}/.local/share/Steam/logs/connection_log.txt");
    let cmd = format!(
        "{compositor}\
         if pgrep -x steam >/dev/null; then \
         echo STEAM_READY; exit 0; \
         fi; \
         : > {log} 2>/dev/null; \
         nohup steam -silent >/dev/null 2>&1 & \
         for i in $(seq 1 60); do \
         if grep -q 'Logged On.*processing complete' {log} 2>/dev/null; then \
         echo STEAM_READY; exit 0; \
         fi; sleep 1; done; \
         echo STEAM_TIMEOUT"
    );
    let result = backend.run_cmd(ip, &cmd).await;
    if !result.success || !result.stdout.contains("STEAM_READY") {
        anyhow::bail!("Steam failed on {ip}: {}", result.stderr);
    }
    Ok(())
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
        compositor_setup(display, &config.vm_user),
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

/// Run the `steam-check` subcommand: boot each instance, verify Steam login + health.
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

        let compositor = compositor_setup(display, &config.vm_user);
        let result = backend
            .run_cmd(
                &vm.ip,
                &format!(
                    r#"
                    STEAM_DIR="$HOME/.local/share/Steam"
                    ERRORS=""
                    LOGIN_FILE="$STEAM_DIR/config/loginusers.vdf"
                    if [ -f "$LOGIN_FILE" ]; then
                        PERSONA=$(grep -oP '"PersonaName"\s+"\K[^"]+' "$LOGIN_FILE" 2>/dev/null | head -1)
                        if [ -z "$PERSONA" ]; then ERRORS="$ERRORS no-persona"; fi
                    else
                        PERSONA=""
                        ERRORS="$ERRORS not-logged-in"
                    fi
                    {compositor}
                    STARTED_COMPOSITOR=1
                    steam -silent -cef-disable-gpu >/dev/null 2>&1 &
                    STEAM_PID=$!
                    sleep 12
                    if ! kill -0 $STEAM_PID 2>/dev/null; then ERRORS="$ERRORS steam-crashed"; fi
                    pkill -x steam 2>/dev/null || true
                    pkill -x weston 2>/dev/null || true
                    if [ -z "$ERRORS" ]; then echo "OK:$PERSONA"; else echo "FAIL:$PERSONA:$ERRORS"; fi
                    "#
                ),
            )
            .await;

        let output = result.stdout.trim();
        if let Some(rest) = output.strip_prefix("OK:") {
            println!("  Login: OK ({rest})");
            println!("  Steam: OK");
            passed.push(vm.name.clone());
        } else if let Some(rest) = output.strip_prefix("FAIL:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            let persona = parts.first().unwrap_or(&"");
            let errors = parts.get(1).unwrap_or(&"");
            if persona.is_empty() || errors.contains("not-logged-in") {
                println!("  Login: FAIL");
            } else {
                println!("  Login: OK ({persona})");
            }
            if errors.contains("steam-crashed") {
                println!("  Steam: FAIL (process crashed)");
            } else {
                println!("  Steam: OK");
            }
            failed.push(vm.name.clone());
        } else {
            println!("  Check: FAIL (unexpected output: {output})");
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
        println!("  To fix 'not-logged-in': run 'nix run .#cluster-steam-login'");
        anyhow::bail!("Some VMs failed checks");
    } else {
        println!("  All VMs ready for testing");
    }
    Ok(())
}

/// Run the `steam-login` subcommand: interactive per-instance Steam login wizard.
/// Requires `BridgeReady` — boots instances to perform login.
pub async fn login(
    config: &ClusterConfig<BridgeReady>,
    target: Option<&str>,
    continue_from: bool,
    login_runners_dir: &Path,
    creds: Option<&CredentialsMap>,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, continue_from)?;

    for vm in &targets {
        let vm_creds = creds.and_then(|c| c.get::<str>(&vm.name));
        if let Err(e) = login_single_vm(config, vm, login_runners_dir, vm_creds).await {
            eprintln!("Error with {}: {e}", vm.name);
        }
    }
    Ok(())
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
    backend.start_instance(config, vm, login_runners_dir)?;

    let config_for_cleanup = config.clone();
    let vm_for_cleanup = vm.clone();
    let cleanup = move || {
        config_for_cleanup
            .backend
            .stop_instance(&config_for_cleanup, &vm_for_cleanup);
    };

    print!("  Waiting for SSH... ");
    if !backend.wait_ready(&vm.ip, 60).await {
        println!("timeout");
        cleanup();
        anyhow::bail!("SSH timeout for {}", vm.name);
    }
    println!("ready");

    let outcome = if let Some(creds) = creds {
        // Automated login via `steam -login`
        automated_login(backend, vm, creds, &config.vm_user).await?;
        LoginOutcome::Completed
    } else {
        // Interactive VNC-based login (original flow)
        interactive_login(backend, vm, &config.vm_user).await?
    };

    if outcome == LoginOutcome::LeftRunning {
        println!("  {} left running for manual VNC login.", vm.name);
        println!("  After completing Steam login, validate with `cluster-ctl accounts`.");
        println!("  Stop the login VM with `cluster-ctl down` when you are done.");
        return Ok(());
    }

    println!("  Shutting down Steam and VM...");
    backend
        .run_cmd(
            &vm.ip,
            "pkill -x steam 2>/dev/null; pkill -x wayvnc 2>/dev/null; pkill -x sway 2>/dev/null",
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    backend.stop_instance(config, vm);
    println!("  {} done.", vm.name);

    Ok(())
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

    let results = par_each_vm(vms, |vm| {
        let backend = backend.clone();
        let vm_user = vm_user.clone();
        let creds = creds.clone();
        async move {
            let Some(vm_creds) = creds.get::<str>(&vm.name) else {
                return (vm.name, None);
            };
            match automated_login(&backend, &vm, vm_creds, &vm_user).await {
                Ok(()) => (vm.name, Some(true)),
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

/// Automated login: starts weston, runs `steam -login`, waits for login confirmation,
/// and optionally activates a game key.
async fn automated_login(
    backend: &Backend,
    vm: &VmDef,
    creds: &VmCredentials,
    vm_user: &str,
) -> anyhow::Result<()> {
    let user = shell_escape(&creds.steam_user);
    let pass = shell_escape(&creds.steam_pass);

    println!("  Logging in as {}...", creds.steam_user);

    let log_dir = format!("/home/{vm_user}/.local/share/Steam/logs");
    let log = format!("{log_dir}/connection_log.txt");
    let stdout_log = format!("{log_dir}/steampipe_login_stdout.log");
    let weston = weston_setup(vm_user);
    let cmd = format!(
        r#"{weston}
mkdir -p {log_dir}
: > {log} 2>/dev/null
: > {stdout_log} 2>/dev/null
steam -login '{user}' '{pass}' -silent -cef-disable-gpu >{stdout_log} 2>&1 &
for i in $(seq 1 180); do
    if grep -q 'Logged On.*processing complete' {log} 2>/dev/null; then
        echo STEAM_LOGIN_OK
        exit 0
    fi
    sleep 1
done
if grep -q 'Update complete, launching' {log_dir}/bootstrap_log.txt 2>/dev/null; then
    echo STEAM_LOGIN_RETRY_AFTER_UPDATE
    pkill -x steam 2>/dev/null || true
    sleep 5
    : > {log} 2>/dev/null
    : > {stdout_log} 2>/dev/null
    steam -login '{user}' '{pass}' -silent -cef-disable-gpu >{stdout_log} 2>&1 &
    for i in $(seq 1 180); do
        if grep -q 'Logged On.*processing complete' {log} 2>/dev/null; then
            echo STEAM_LOGIN_OK
            exit 0
        fi
        sleep 1
    done
fi
echo STEAM_LOGIN_TIMEOUT
echo "--- connection_log.txt tail ---"
tail -n 80 {log} 2>/dev/null || true
echo "--- bootstrap_log.txt tail ---"
tail -n 80 {log_dir}/bootstrap_log.txt 2>/dev/null || true
echo "--- cef_log.txt tail ---"
tail -n 80 {log_dir}/cef_log.txt 2>/dev/null || true
echo "--- steam stdout/stderr tail ---"
tail -n 80 {stdout_log} 2>/dev/null || true"#,
    );

    let result = backend.run_cmd(&vm.ip, &cmd).await;
    let output = result.stdout.trim();

    if !steam_login_succeeded(output) {
        anyhow::bail!(
            "{}: Steam login failed ({}). stderr: {}",
            vm.name,
            output,
            result.stderr.trim()
        );
    }
    println!("  Login successful");

    // Activate game key if provided
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
            sway >/dev/null 2>&1 &
            sleep 2
        fi
        export WAYLAND_DISPLAY=wayland-1
        if ! pgrep -x wayvnc >/dev/null; then
            wayvnc 0.0.0.0 5900 >/dev/null 2>&1 &
            sleep 1
        fi
        export DISPLAY=:0
        if ! pgrep -x steam >/dev/null; then
            steam -cef-disable-gpu >/dev/null 2>&1 &
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
                "  VNC into {}:5900, complete Steam login, then run `cluster-ctl accounts`.",
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
                println!("  Continue in VNC, then run `cluster-ctl accounts` to validate.");
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

/// Escape single quotes for safe shell interpolation inside single-quoted strings.
fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Default directory where the NixOS module persists Steam login state.
const DEFAULT_LOGIN_STATE_DIR: &str = "/var/lib/steampipe/logins";

/// Remove persisted Steam login state from the host login state directory.
///
/// `target` selects which VMs to clean: `None` or `Some("all")` cleans every
/// VM subdirectory; a VM name like `"vm-3"` or bare number `"3"` cleans only
/// that one.
pub fn clean_logins(
    vms: &[VmDef],
    login_state_dir: Option<&Path>,
    target: Option<&str>,
) -> anyhow::Result<()> {
    let dir = login_state_dir.unwrap_or_else(|| Path::new(DEFAULT_LOGIN_STATE_DIR));

    if !dir.exists() {
        println!("Login state directory does not exist: {}", dir.display());
        return Ok(());
    }

    let targets: Vec<&VmDef> = match target {
        None | Some("all") => vms.iter().collect(),
        Some(t) => {
            let name = if t.starts_with("vm-") {
                t.to_string()
            } else {
                format!("vm-{t}")
            };
            match vms.iter().find(|v| *v.name == *name) {
                Some(vm) => vec![vm],
                None => anyhow::bail!("unknown VM: {name}"),
            }
        }
    };

    let mut cleaned = 0u32;
    for vm in &targets {
        let vm_dir = dir.join(&vm.name);
        if !vm_dir.exists() {
            println!("  {}: no login state, skipping", vm.name);
            continue;
        }
        std::fs::remove_dir_all(&vm_dir)?;
        std::fs::create_dir_all(&vm_dir)?;
        println!("  {}: cleaned", vm.name);
        cleaned += 1;
    }

    if cleaned == 0 {
        println!("Nothing to clean.");
    } else {
        println!("==> Cleaned login state for {cleaned} VM(s)");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn steam_login_succeeded_requires_exact_marker_line() {
        assert!(steam_login_succeeded("noise\nSTEAM_LOGIN_OK\nmore noise"));
        assert!(!steam_login_succeeded("STEAM_LOGIN_TIMEOUT"));
        assert!(!steam_login_succeeded("NOT_STEAM_LOGIN_OK"));
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

    // ── Compositor setup scripts ────────────────────────────────────────

    #[test]
    fn weston_setup_uses_headless_backend() {
        let script = weston_setup("testuser");
        assert!(
            script.contains("--backend=headless"),
            "weston_setup must use headless backend"
        );
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("DISPLAY=:0"));
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-testuser"));
    }

    #[test]
    fn weston_gpu_setup_no_headless_backend() {
        let script = weston_gpu_setup("testuser");
        assert!(
            !script.contains("--backend=headless"),
            "weston_gpu_setup must NOT use headless backend"
        );
        assert!(script.contains("weston"), "must still launch weston");
        assert!(script.contains("--xwayland"));
        assert!(script.contains("--no-config"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("DISPLAY=:0"));
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-testuser"));
    }

    #[test]
    fn sway_setup_script() {
        let script = sway_setup("testuser");
        assert!(script.contains("sway"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("DISPLAY=:0"));
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-testuser"));
    }

    #[test]
    fn sway_gpu_setup_sets_drm_backend() {
        let script = sway_gpu_setup("testuser");
        assert!(
            script.contains("WLR_BACKENDS=drm"),
            "sway_gpu_setup must set WLR_BACKENDS=drm"
        );
        assert!(script.contains("sway"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("DISPLAY=:0"));
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-testuser"));
    }

    #[test]
    fn compositor_setup_headless_returns_empty() {
        let script = compositor_setup(crate::cli::DisplayMode::Headless, "u");
        assert!(script.is_empty());
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
        assert!(!script.contains("WLR_BACKENDS"));
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
