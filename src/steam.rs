use std::path::Path;

use crate::backend::Backend;
use crate::config::{BridgeReady, ClusterConfig, VmDef, par_each_vm};
use crate::credentials::{CredentialsMap, VmCredentials};

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

/// Shell snippet that starts Steam silently if not already running.
pub const STEAM_START_SILENT: &str = r#"
if pgrep -x steam >/dev/null; then
    echo "already-running"
else
    steam -silent -cef-disable-gpu >/dev/null 2>&1 &
    echo "started"
fi
"#;

/// Ensure weston + Steam are running on an instance, waiting for Steam's connection log.
pub async fn ensure_steam(backend: &Backend, ip: &str, vm_user: &str) -> anyhow::Result<()> {
    let log = format!("/home/{vm_user}/.local/share/Steam/logs/connection_log.txt");
    let weston = weston_setup(vm_user);
    let cmd = format!(
        "{weston}\
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
pub async fn start<S>(config: &ClusterConfig<S>, target: Option<&str>) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, false)?;
    let backend = config.backend.clone();

    println!("==> Starting Steam on {} VM(s)...", targets.len());
    let script = format!("{}{STEAM_START_SILENT}", weston_setup(&config.vm_user));

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

        let weston = weston_setup(&config.vm_user);
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
                    {weston}
                    STARTED_WESTON=1
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

    if let Some(creds) = creds {
        // Automated login via `steam -login`
        automated_login(backend, vm, creds, &config.vm_user).await?;
    } else {
        // Interactive VNC-based login (original flow)
        interactive_login(backend, vm, &config.vm_user).await?;
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

    let log = format!("/home/{vm_user}/.local/share/Steam/logs/connection_log.txt");
    let weston = weston_setup(vm_user);
    let cmd = format!(
        r#"{weston}
: > {log} 2>/dev/null
steam -login '{user}' '{pass}' -silent -cef-disable-gpu >/dev/null 2>&1 &
for i in $(seq 1 90); do
    if grep -q 'Logged On.*processing complete' {log} 2>/dev/null; then
        echo STEAM_LOGIN_OK
        exit 0
    fi
    sleep 1
done
echo STEAM_LOGIN_TIMEOUT"#,
    );

    let result = backend.run_cmd(&vm.ip, &cmd).await;
    let output = result.stdout.trim();

    if !output.contains("STEAM_LOGIN_OK") {
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
async fn interactive_login(backend: &Backend, vm: &VmDef, vm_user: &str) -> anyhow::Result<()> {
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
    loop {
        let mut input = String::new();
        stdin.read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            break;
        } else if input == "p" {
            println!("  Paste text (will be typed into focused VM window):");
            let mut text = String::new();
            stdin.read_line(&mut text)?;
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

    Ok(())
}

/// Escape single quotes for safe shell interpolation inside single-quoted strings.
fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
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
}
