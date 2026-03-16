use std::path::Path;

use crate::config::{ClusterConfig, VmDef};
use crate::ssh::SshClient;
use crate::vm;

/// Start weston + Steam on target VMs (uses already-running cluster VMs).
pub async fn start(config: &ClusterConfig, target: Option<&str>) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, false)?;
    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);

    println!("==> Starting Steam on {} VM(s)...", targets.len());
    let mut tasks = tokio::task::JoinSet::new();
    for vm in targets {
        let ssh = ssh.clone();
        let name = vm.name.clone();
        let ip = vm.ip.clone();
        tasks.spawn(async move {
            let result = ssh
                .run(
                    &ip,
                    r#"
                    export XDG_RUNTIME_DIR=/tmp/runtime-chessbender
                    mkdir -p $XDG_RUNTIME_DIR
                    if ! pgrep -x weston >/dev/null; then
                        weston --backend=headless --xwayland --no-config >/dev/null 2>&1 &
                        sleep 2
                    fi
                    export WAYLAND_DISPLAY=wayland-1
                    export DISPLAY=:0
                    if pgrep -x steam >/dev/null; then
                        echo "already-running"
                    else
                        steam -silent -cef-disable-gpu >/dev/null 2>&1 &
                        echo "started"
                    fi
                    "#,
                )
                .await;
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

/// Run the `steam-check` subcommand: boot each VM, verify Steam login + health.
pub async fn check(
    config: &ClusterConfig,
    target: Option<&str>,
    runners_dir: &Path,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, false)?;
    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);

    // Check bridge exists
    if !std::process::Command::new("ip")
        .args(["link", "show", &config.bridge])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?
        .success()
    {
        anyhow::bail!(
            "Bridge {} not found. Run 'just cluster-net-up' first.",
            config.bridge
        );
    }

    println!("==> Checking Steam setup on VMs (boot -> check -> shutdown)...");

    let mut passed = Vec::new();
    let mut failed = Vec::new();

    for vm in targets {
        println!("\n── {} ({}) ──", vm.name, vm.ip);

        // Stop any existing VM on this slot
        vm::stop_vm(config, vm);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Boot
        print!("  Boot: ");
        if let Err(e) = vm::start_vm(config, vm, runners_dir) {
            println!("FAIL ({e})");
            failed.push(vm.name.clone());
            continue;
        }
        if !ssh.wait_ready(&vm.ip, 30).await {
            println!("FAIL (SSH timeout)");
            vm::stop_vm(config, vm);
            failed.push(vm.name.clone());
            continue;
        }
        println!("OK");

        // Run checks
        let result = ssh
            .run(
                &vm.ip,
                r#"
                STEAM_DIR="$HOME/.local/share/Steam"
                ERRORS=""

                # Check Steam login
                LOGIN_FILE="$STEAM_DIR/config/loginusers.vdf"
                if [ -f "$LOGIN_FILE" ]; then
                    PERSONA=$(grep -oP '"PersonaName"\s+"\K[^"]+' "$LOGIN_FILE" 2>/dev/null | head -1)
                    if [ -z "$PERSONA" ]; then
                        ERRORS="$ERRORS no-persona"
                    fi
                else
                    PERSONA=""
                    ERRORS="$ERRORS not-logged-in"
                fi

                # Start Steam to check it doesn't crash
                export XDG_RUNTIME_DIR=/tmp/runtime-chessbender
                mkdir -p $XDG_RUNTIME_DIR
                STARTED_WESTON=0
                if ! pgrep -x weston >/dev/null; then
                    weston --backend=headless --xwayland --no-config >/dev/null 2>&1 &
                    sleep 2
                    STARTED_WESTON=1
                fi
                export WAYLAND_DISPLAY=wayland-1
                export DISPLAY=:0
                steam -silent -cef-disable-gpu >/dev/null 2>&1 &
                STEAM_PID=$!
                sleep 12
                if ! kill -0 $STEAM_PID 2>/dev/null; then
                    ERRORS="$ERRORS steam-crashed"
                fi

                # Cleanup
                pkill -x steam 2>/dev/null || true
                if [ "$STARTED_WESTON" = "1" ]; then
                    pkill -x weston 2>/dev/null || true
                fi

                if [ -z "$ERRORS" ]; then
                    echo "OK:$PERSONA"
                else
                    echo "FAIL:$PERSONA:$ERRORS"
                fi
                "#,
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

        vm::stop_vm(config, vm);
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

/// Run the `steam-login` subcommand: interactive per-VM Steam login wizard.
pub async fn login(
    config: &ClusterConfig,
    target: Option<&str>,
    continue_from: bool,
    login_runners_dir: &Path,
) -> anyhow::Result<()> {
    let targets = config.resolve_targets(target, continue_from)?;
    let ssh = SshClient::new(&config.ssh_key, &config.vm_user);

    // Check bridge
    if !std::process::Command::new("ip")
        .args(["link", "show", &config.bridge])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?
        .success()
    {
        anyhow::bail!(
            "Bridge {} not found. Run 'just cluster-net-up' first.",
            config.bridge
        );
    }

    for vm in targets {
        if let Err(e) = login_single_vm(config, vm, &ssh, login_runners_dir).await {
            eprintln!("Error with {}: {e}", vm.name);
        }
    }
    Ok(())
}

async fn login_single_vm(
    config: &ClusterConfig,
    vm: &VmDef,
    ssh: &SshClient,
    login_runners_dir: &Path,
) -> anyhow::Result<()> {
    println!();
    println!("══════════════════════════════════════════════════");
    println!("  Steam login for {} ({})", vm.name, vm.ip);
    println!("  Press Ctrl+C at any time to cancel and shut down the VM");
    println!("══════════════════════════════════════════════════");

    // Stop any existing VM on this slot
    vm::stop_vm(config, vm);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Boot the login VM (4GB RAM)
    println!("  Booting VM (4 GB RAM for Sway + Steam GUI)...");
    vm::start_vm(config, vm, login_runners_dir)?;

    // Set up Ctrl+C handler to clean up
    let vm_name = vm.name.clone();
    let config_for_cleanup = config.clone();
    let vm_for_cleanup = vm.clone();
    let cleanup = move || {
        vm::stop_vm(&config_for_cleanup, &vm_for_cleanup);
    };

    print!("  Waiting for SSH... ");
    if !ssh.wait_ready(&vm.ip, 60).await {
        println!("timeout");
        cleanup();
        anyhow::bail!("SSH timeout for {}", vm.name);
    }
    println!("ready");

    // Start Sway + wayvnc + Steam
    println!("  Starting sway + wayvnc + Steam...");
    ssh.run(
        &vm.ip,
        r#"
        export XDG_RUNTIME_DIR=/tmp/runtime-chessbender
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
        "#,
    )
    .await;

    println!();
    println!("  VNC into:  {}:5900", vm.ip);
    println!("  Complete the Steam login + Steam Guard in the VNC window.");
    println!();
    println!("  Commands:  [p] paste text into VM  [Enter] done  [Ctrl+C] cancel");
    println!();

    // Interactive loop
    let stdin = std::io::stdin();
    loop {
        let mut input = String::new();
        stdin.read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            // Enter = done
            break;
        } else if input == "p" {
            // Paste mode: read text from next line (hidden for security)
            println!("  Paste text (will be typed into focused VM window):");
            let mut text = String::new();
            stdin.read_line(&mut text)?;
            let text = text.trim_end_matches('\n');
            if !text.is_empty() {
                // Use wtype via SSH to type into the focused window
                // Pipe text via stdin to avoid shell injection
                let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
                ssh.run(
                    &vm.ip,
                    &format!(
                        r#"export XDG_RUNTIME_DIR=/tmp/runtime-chessbender; export WAYLAND_DISPLAY=wayland-1; wtype -- "{escaped}""#
                    ),
                )
                .await;
                println!("  Typed into VM.");
            }
        }
    }

    // Cleanup: kill Steam + Sway, then shut down VM
    println!("  Shutting down Steam and VM...");
    ssh.run(
        &vm.ip,
        "pkill -x steam 2>/dev/null; pkill -x wayvnc 2>/dev/null; pkill -x sway 2>/dev/null",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    vm::stop_vm(config, vm);
    println!("  {} done.", vm_name);

    Ok(())
}
