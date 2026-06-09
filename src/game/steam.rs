use std::env;
use std::ffi::OsStr;
use std::io::{self, BufRead, ErrorKind, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime};

use crate::core::backend::{Backend, microvm_log_path};
use crate::core::config::{BridgeReady, ClusterConfig, Unchecked, VmDef, VmName, par_each_vm};
use crate::core::credentials::{CredentialsMap, VmCredentials};
use crate::core::nixos_module::{Discovery, NixosModuleConfig, RowStatus};
use crate::game::accounts::{self, SessionStatus};
use crate::vm::lease;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use zeroize::Zeroize;

const LOGIN_SSH_TIMEOUT_SECS: u32 = 180;
const GUARD_PROMPT_BIN_ENV: &str = "STEAMPIPE_GUARD_PROMPT_BIN";
const GUARD_PROMPT_BIN_NAME: &str = "cluster-guard-prompt";
const GUARD_PROMPT_TIMEOUT_SECS: u64 = 120;
pub const GUARD_CODE_NEEDED_EXIT_CODE: i32 = 3;

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
export DISPLAY=:0
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
export DISPLAY=:0
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
            "  To fix 'not-logged-in': run `cluster-ctl steam login <vm>` on an operator host with a display (raises a Steam Guard dialog), or pass `--code` / set STEAMPIPE_GUARD_CODE on a headless host."
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

pub(crate) fn steam_identity_probe_script(vm_user: &str) -> String {
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

pub(crate) fn parse_steam_identity(output: &str) -> (Option<String>, bool) {
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

enum LoginLogMode {
    Stdout,
    Buffered,
    Silent,
}

struct LoginLog {
    mode: LoginLogMode,
    lines: Vec<String>,
}

impl LoginLog {
    fn stdout() -> Self {
        Self {
            mode: LoginLogMode::Stdout,
            lines: Vec::new(),
        }
    }

    fn buffered() -> Self {
        Self {
            mode: LoginLogMode::Buffered,
            lines: Vec::new(),
        }
    }

    fn silent() -> Self {
        Self {
            mode: LoginLogMode::Silent,
            lines: Vec::new(),
        }
    }

    fn blank(&mut self) {
        self.line("");
    }

    fn line(&mut self, line: impl Into<String>) {
        let line = line.into();
        match self.mode {
            LoginLogMode::Stdout => println!("{line}"),
            LoginLogMode::Buffered => self.lines.push(line),
            LoginLogMode::Silent => {}
        }
    }

    fn into_output(self) -> String {
        self.lines.join("\n")
    }
}

enum LoginAttempt {
    Skipped(String),
    LoggedIn,
    GuardCodeNeeded(GuardCodeNeeded),
    Failed(anyhow::Error),
}

struct LoginReport {
    vm_index: u8,
    vm_name: String,
    output: String,
    attempt: LoginAttempt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginBootAction {
    ReuseRunning,
    Restart,
}

fn login_boot_action(automated: bool, reachable: bool) -> LoginBootAction {
    if automated && reachable {
        LoginBootAction::ReuseRunning
    } else {
        LoginBootAction::Restart
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardCodeNeeded {
    pub vm_name: String,
    pub steam_user: String,
    pub reason: String,
}

impl GuardCodeNeeded {
    fn new(vm: &VmDef, steam_user: &str, reason: &str) -> Self {
        Self {
            vm_name: vm.name.to_string(),
            steam_user: steam_user.to_string(),
            reason: reason.to_string(),
        }
    }

    pub(crate) fn for_vm_name(vm_name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            vm_name: vm_name.into(),
            steam_user: "unknown".to_string(),
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for GuardCodeNeeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "STEAM_GUARD_CODE_NEEDED: {} needs a Steam Guard code for {} ({}). Run `cluster-ctl steam login {}` on an operator host with a display, or run `cluster-ctl steam login {} --code <CODE>` / set STEAMPIPE_GUARD_CODE on a headless host.",
            self.vm_name, self.steam_user, self.reason, self.vm_name, self.vm_name
        )
    }
}

impl std::error::Error for GuardCodeNeeded {}

pub(crate) struct GuardPromptContext<'a> {
    pub vm_name: &'a str,
    pub steam_user: &'a str,
    pub reason: &'a str,
    pub invalid_retry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GuardCodeOutcome {
    Code(String),
    #[allow(dead_code)]
    Cancelled,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoginContext {
    pub display: bool,
    pub stdin_tty: bool,
    pub non_interactive: bool,
}

impl LoginContext {
    pub(crate) fn detect(force_non_interactive: bool) -> Self {
        Self {
            display: host_display_available(),
            stdin_tty: io::stdin().is_terminal(),
            // Phase 06 will also set this when cluster-ctl is serving MCP.
            non_interactive: force_non_interactive,
        }
    }

    pub(crate) fn force_non_interactive() -> Self {
        Self {
            display: host_display_available(),
            stdin_tty: io::stdin().is_terminal(),
            non_interactive: true,
        }
    }

    #[cfg(test)]
    fn synthetic(display: bool, stdin_tty: bool, non_interactive: bool) -> Self {
        Self {
            display,
            stdin_tty,
            non_interactive,
        }
    }
}

fn host_display_available() -> bool {
    display_env_present(
        env::var_os("WAYLAND_DISPLAY").as_deref(),
        env::var_os("DISPLAY").as_deref(),
    )
}

fn display_env_present(wayland_display: Option<&OsStr>, display: Option<&OsStr>) -> bool {
    wayland_display.is_some_and(|value| !value.is_empty())
        || display.is_some_and(|value| !value.is_empty())
}

/// Locate an already-built helper binary: an explicit override, the
/// `$STEAMPIPE_GUARD_PROMPT_BIN` env, a sibling of the current exe, or `$PATH`.
/// Used for the installed/production layout and tests; the development path
/// prefers `cargo run` (see [`guard_prompt_command`]).
fn resolve_guard_prompt_binary(override_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = override_path {
        return Some(path.to_path_buf());
    }

    if let Some(path) = env::var_os(GUARD_PROMPT_BIN_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }

    if let Ok(current_exe) = env::current_exe()
        && let Some(exe_dir) = current_exe.parent()
    {
        let candidate = exe_dir.join(GUARD_PROMPT_BIN_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|dir| dir.join(GUARD_PROMPT_BIN_NAME))
            .find(|candidate| candidate.is_file())
    })
}

/// Build the base command that launches the Steam Guard prompt helper, ready for
/// the prompt arguments to be appended.
///
/// Resolution order:
/// 1. An explicit override or `$STEAMPIPE_GUARD_PROMPT_BIN` (tests + the installed
///    Nix wrapper that bakes the GUI library path).
/// 2. **Development:** when `cluster-ctl` itself is running from a cargo
///    `target/<profile>/` tree, run the helper via `cargo run -p
///    cluster-guard-prompt`. This is preferred over a sibling binary on purpose —
///    `cargo run -- steam login …` rebuilds only `cluster-ctl`, so a sibling
///    `cluster-guard-prompt` would be **stale**; going through cargo rebuilds it
///    (instant when already current) so helper edits always take effect.
/// 3. **Installed:** a sibling of the current exe or a `$PATH` binary.
///
/// Returns `None` only when no binary and no cargo workspace can be located.
fn guard_prompt_command(override_path: Option<&Path>) -> Option<Command> {
    if let Some(path) = override_path {
        return Some(Command::new(path));
    }
    if let Some(path) = env::var_os(GUARD_PROMPT_BIN_ENV).filter(|value| !value.is_empty()) {
        return Some(Command::new(path));
    }

    if let Some(workspace_root) = cargo_workspace_root_from_current_exe() {
        let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .current_dir(workspace_root)
            .arg("run")
            .arg("--quiet")
            .arg("--package")
            .arg(GUARD_PROMPT_BIN_NAME)
            .arg("--");
        return Some(command);
    }

    resolve_guard_prompt_binary(None).map(Command::new)
}

/// Locate the cargo workspace root when running from a `target/<profile>/` tree,
/// so the development fallback can `cargo run -p cluster-guard-prompt` against it.
fn cargo_workspace_root_from_current_exe() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    loop {
        if dir.file_name() == Some(OsStr::new("target")) {
            let root = dir.parent()?;
            return root
                .join("Cargo.toml")
                .is_file()
                .then(|| root.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

async fn request_guard_code_from_helper(
    cx: &GuardPromptContext<'_>,
    override_path: Option<&Path>,
) -> anyhow::Result<GuardCodeOutcome> {
    let Some(mut command) = guard_prompt_command(override_path) else {
        return Ok(GuardCodeOutcome::Unavailable);
    };

    command
        .arg("--vm")
        .arg(cx.vm_name)
        .arg("--user")
        .arg(cx.steam_user)
        .arg("--reason")
        .arg(cx.reason)
        .arg("--timeout-secs")
        .arg(GUARD_PROMPT_TIMEOUT_SECS.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if cx.invalid_retry {
        command.arg("--invalid-retry");
    }

    let output = match command.output().await {
        Ok(output) => output,
        Err(error) => {
            eprintln!(
                "  Could not launch the Steam Guard dialog ({GUARD_PROMPT_BIN_NAME}): {error}"
            );
            return Ok(GuardCodeOutcome::Unavailable);
        }
    };

    match output.status.code() {
        Some(0) => guard_code_from_helper_stdout(output.stdout),
        Some(10 | 11) => Ok(GuardCodeOutcome::Cancelled),
        Some(12) => Ok(GuardCodeOutcome::Unavailable),
        Some(other) => {
            eprintln!(
                "  Steam Guard dialog exited unexpectedly (code {other}); falling back to terminal input if available."
            );
            Ok(GuardCodeOutcome::Unavailable)
        }
        None => Ok(GuardCodeOutcome::Unavailable),
    }
}

fn guard_code_from_helper_stdout(mut stdout: Vec<u8>) -> anyhow::Result<GuardCodeOutcome> {
    if stdout.ends_with(b"\n") {
        stdout.pop();
    }
    if stdout.is_empty() {
        return Ok(GuardCodeOutcome::Cancelled);
    }
    if stdout.iter().any(|byte| byte.is_ascii_whitespace()) {
        return Ok(GuardCodeOutcome::Unavailable);
    }

    Ok(String::from_utf8(stdout)
        .map(GuardCodeOutcome::Code)
        .unwrap_or(GuardCodeOutcome::Unavailable))
}

#[derive(Debug, Clone)]
pub(crate) enum GuardProvider {
    PreSupplied(Arc<StdMutex<Option<String>>>),
    Stdin {
        prompt_lock: Arc<AsyncMutex<()>>,
    },
    HelperGui {
        prompt_lock: Arc<AsyncMutex<()>>,
        helper_bin: Option<PathBuf>,
        /// Fall back to a terminal prompt when the GUI dialog can't be shown
        /// (no usable display / helper missing). Captured from the TTY check at
        /// selection time so a failed popup degrades to stdin instead of a
        /// dead-end fail-fast.
        stdin_fallback: bool,
    },
    FailFast,
}

impl GuardProvider {
    pub(crate) fn pre_supplied(code: String) -> Self {
        Self::PreSupplied(Arc::new(StdMutex::new(Some(code))))
    }

    pub(crate) fn fail_fast() -> Self {
        Self::FailFast
    }

    pub(crate) fn stdin() -> Self {
        Self::Stdin {
            prompt_lock: Arc::new(AsyncMutex::new(())),
        }
    }

    pub(crate) fn helper_gui(stdin_fallback: bool) -> Self {
        Self::HelperGui {
            prompt_lock: Arc::new(AsyncMutex::new(())),
            helper_bin: None,
            stdin_fallback,
        }
    }

    /// Build a GUI provider pinned to a specific helper binary. Test-only;
    /// `pub(crate)` so sibling modules' tests (e.g. `steam_auth`) can drive the
    /// retry session against a fake helper script.
    #[cfg(test)]
    pub(crate) fn helper_gui_with_binary(helper_bin: PathBuf) -> Self {
        Self::HelperGui {
            prompt_lock: Arc::new(AsyncMutex::new(())),
            helper_bin: Some(helper_bin),
            stdin_fallback: false,
        }
    }

    pub(crate) fn select(code: Option<String>, context: LoginContext, no_gui: bool) -> Self {
        if let Some(code) = code.filter(|code| !code.trim().is_empty()) {
            return Self::pre_supplied(code.trim().to_owned());
        }
        if context.non_interactive {
            return Self::fail_fast();
        }
        if context.display && !no_gui {
            return Self::helper_gui(context.stdin_tty);
        }
        if context.stdin_tty {
            return Self::stdin();
        }
        Self::fail_fast()
    }

    #[cfg(test)]
    fn kind(&self) -> GuardProviderKind {
        match self {
            Self::PreSupplied(_) => GuardProviderKind::PreSupplied,
            Self::Stdin { .. } => GuardProviderKind::Stdin,
            Self::HelperGui { .. } => GuardProviderKind::HelperGui,
            Self::FailFast => GuardProviderKind::FailFast,
        }
    }

    pub(crate) async fn request(
        &self,
        cx: &GuardPromptContext<'_>,
    ) -> anyhow::Result<GuardCodeOutcome> {
        match self {
            Self::PreSupplied(code) => {
                let mut code = code
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Steam Guard provider lock poisoned"))?;
                Ok(code
                    .take()
                    .map(GuardCodeOutcome::Code)
                    .unwrap_or(GuardCodeOutcome::Unavailable))
            }
            Self::Stdin { prompt_lock } => {
                let _guard = prompt_lock.lock().await;
                read_guard_code_from_stdin(cx)
            }
            Self::HelperGui {
                prompt_lock,
                helper_bin,
                stdin_fallback,
            } => {
                let _guard = prompt_lock.lock().await;
                let outcome = request_guard_code_from_helper(cx, helper_bin.as_deref()).await?;
                // The GUI dialog couldn't be shown (no usable display / helper
                // missing). Degrade to a terminal prompt rather than dead-ending,
                // so the operator can still complete the login. A user-initiated
                // cancel (Cancelled) is respected and does NOT fall through.
                if matches!(outcome, GuardCodeOutcome::Unavailable)
                    && *stdin_fallback
                    && io::stdin().is_terminal()
                {
                    eprintln!("  Steam Guard dialog unavailable; falling back to terminal input.");
                    return read_guard_code_from_stdin(cx);
                }
                Ok(outcome)
            }
            Self::FailFast => Ok(GuardCodeOutcome::Unavailable),
        }
    }

    /// Open a retry-capable Guard code source for the headless token mint.
    ///
    /// For [`GuardProvider::HelperGui`] this spawns ONE persistent dialog
    /// (`--interactive`) that stays open across rejected codes — showing each
    /// rejection inline and closing only on success / cancel / timeout — so a
    /// corrected code is resubmitted on the *same* `steam-vent` auth session
    /// (never mailing a fresh email code). If the dialog can't be shown it
    /// degrades exactly like [`request`](Self::request): a terminal prompt when
    /// `stdin_fallback` and a TTY, otherwise fail-fast. The other providers
    /// (`--code`, stdin, fail-fast) are adapted via one-shot `request()` per
    /// attempt, with `invalid_retry` reflecting the prior rejection.
    pub(crate) async fn open_retry_session(
        &self,
        vm_name: &str,
        steam_user: &str,
        reason: &str,
    ) -> GuardRetrySession {
        if let Self::HelperGui {
            prompt_lock,
            helper_bin,
            stdin_fallback,
        } = self
        {
            // Serialize on the shared prompt lock for the WHOLE interactive
            // session, not per-prompt: a persistent dialog (or its terminal
            // fallback) owns the operator's attention for its entire lifetime,
            // so concurrent automated logins (`steam login` over many VMs) must
            // not pop several dialogs — or race the terminal — at once. Held
            // until the session drops. (`--code`/fail-fast take the branch below
            // and stay fully parallel; direct stdin serializes per-prompt inside
            // `request()`.)
            let guard = Arc::clone(prompt_lock).lock_owned().await;
            let cx = GuardPromptContext {
                vm_name,
                steam_user,
                reason,
                invalid_retry: false,
            };
            if let Some(dialog) = spawn_guard_dialog(&cx, helper_bin.as_deref()) {
                return GuardRetrySession {
                    inner: RetryInner::Dialog(Box::new(dialog)),
                    _prompt_guard: Some(guard),
                };
            }
            // The dialog couldn't be launched (no helper command / spawn failed).
            // Mirror request()'s degradation rather than dead-ending. The terminal
            // reader uses a fresh, uncontended lock (a different `Arc`) so it does
            // NOT re-lock the session guard we already hold — that would deadlock.
            let provider = if *stdin_fallback && io::stdin().is_terminal() {
                eprintln!("  Steam Guard dialog unavailable; falling back to terminal input.");
                GuardProvider::stdin()
            } else {
                GuardProvider::fail_fast()
            };
            return GuardRetrySession::from_provider(
                provider,
                vm_name,
                steam_user,
                reason,
                Some(guard),
            );
        }

        GuardRetrySession::from_provider(self.clone(), vm_name, steam_user, reason, None)
    }
}

/// The result of asking a [`GuardRetrySession`] for the next code.
pub(crate) enum GuardRetryNext {
    /// A code to submit to Steam.
    Code(String),
    /// The operator cancelled or the prompt timed out.
    Cancelled,
    /// No code channel was available (fail-fast / dialog could not be shown).
    Unavailable,
}

/// A retry-capable Guard code source for the headless token mint: it yields one
/// code per attempt and, for the GUI provider, keeps a single dialog window open
/// across rejected codes (see [`GuardProvider::open_retry_session`]).
pub(crate) struct GuardRetrySession {
    inner: RetryInner,
    /// Held for interactive sessions (GUI dialog / terminal fallback) so
    /// concurrent automated logins serialize on the operator — one prompt at a
    /// time. `None` for non-interactive sources (`--code`, fail-fast), preserving
    /// their parallelism. Released when the session is dropped.
    _prompt_guard: Option<OwnedMutexGuard<()>>,
}

enum RetryInner {
    /// A live, persistent GUI dialog process (boxed — it is far larger than the
    /// `Provider` variant: it owns the child handle, a pipe writer, and a
    /// buffered line reader).
    Dialog(Box<GuardDialog>),
    /// A one-shot provider (`--code`, stdin, fail-fast) adapted to the retry
    /// interface by re-invoking `request()` per attempt.
    Provider {
        provider: GuardProvider,
        vm_name: String,
        steam_user: String,
        reason: String,
    },
}

impl GuardRetrySession {
    fn from_provider(
        provider: GuardProvider,
        vm_name: &str,
        steam_user: &str,
        reason: &str,
        prompt_guard: Option<OwnedMutexGuard<()>>,
    ) -> Self {
        Self {
            inner: RetryInner::Provider {
                provider,
                vm_name: vm_name.to_owned(),
                steam_user: steam_user.to_owned(),
                reason: reason.to_owned(),
            },
            _prompt_guard: prompt_guard,
        }
    }

    /// Request the next code. `previous_error` is `Some` when a prior code on
    /// this session was rejected, which surfaces inline in the dialog (or as the
    /// `invalid_retry` banner on the terminal/one-shot path).
    pub(crate) async fn next_code(&mut self, previous_error: Option<&str>) -> GuardRetryNext {
        match &mut self.inner {
            RetryInner::Dialog(dialog) => {
                if let Some(msg) = previous_error {
                    dialog.show_error(msg).await;
                }
                dialog.read_code().await
            }
            RetryInner::Provider {
                provider,
                vm_name,
                steam_user,
                reason,
            } => {
                let cx = GuardPromptContext {
                    vm_name,
                    steam_user,
                    reason,
                    invalid_retry: previous_error.is_some(),
                };
                match provider.request(&cx).await {
                    Ok(GuardCodeOutcome::Code(code)) => GuardRetryNext::Code(code),
                    Ok(GuardCodeOutcome::Cancelled) => GuardRetryNext::Cancelled,
                    Ok(GuardCodeOutcome::Unavailable) | Err(_) => GuardRetryNext::Unavailable,
                }
            }
        }
    }

    /// Signal a successful login so a persistent dialog closes itself (exit 0).
    /// A no-op for the one-shot provider path.
    pub(crate) async fn confirm_success(&mut self) {
        if let RetryInner::Dialog(dialog) = &mut self.inner {
            dialog.confirm_success().await;
        }
    }
}

/// A live, persistent Steam Guard dialog: the same window stays open across
/// rejected codes. cluster-ctl reads one code line per Submit from the child's
/// stdout and writes control lines (`OK` / `ERR <msg>`) to its stdin. Dropping
/// it reaps the child (`kill_on_drop`), so a fatal non-Guard error closes the
/// window without a bespoke control message.
struct GuardDialog {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl GuardDialog {
    /// Read the next code the operator submits, or classify why the dialog
    /// closed: Cancel (exit 10) / Timeout (exit 11) map to [`GuardRetryNext::Cancelled`],
    /// any other exit (display-unavailable / unexpected) to [`GuardRetryNext::Unavailable`].
    async fn read_code(&mut self) -> GuardRetryNext {
        match self.stdout.next_line().await {
            Ok(Some(line)) => GuardRetryNext::Code(line.trim().to_owned()),
            Ok(None) | Err(_) => match self.child.wait().await.ok().and_then(|s| s.code()) {
                Some(10 | 11) => GuardRetryNext::Cancelled,
                _ => GuardRetryNext::Unavailable,
            },
        }
    }

    /// Show a rejection inline and re-enable input; the window stays open.
    async fn show_error(&mut self, msg: &str) {
        let line = format!("ERR {}\n", sanitize_control_message(msg));
        let _ = self.stdin.write_all(line.as_bytes()).await;
        let _ = self.stdin.flush().await;
    }

    /// Tell the dialog the login succeeded so it closes gracefully (exit 0).
    /// Bounded so a wedged dialog never hangs the mint — `kill_on_drop` reaps it.
    async fn confirm_success(&mut self) {
        let _ = self.stdin.write_all(b"OK\n").await;
        let _ = self.stdin.flush().await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

/// Spawn the Guard prompt helper in persistent interactive mode. Returns `None`
/// when no helper command is available or the spawn / pipe setup fails (the
/// caller then degrades to a terminal prompt or fail-fast).
fn spawn_guard_dialog(
    cx: &GuardPromptContext<'_>,
    override_path: Option<&Path>,
) -> Option<GuardDialog> {
    let mut command = guard_prompt_command(override_path)?;
    command
        .arg("--vm")
        .arg(cx.vm_name)
        .arg("--user")
        .arg(cx.steam_user)
        .arg("--reason")
        .arg(cx.reason)
        .arg("--timeout-secs")
        .arg(GUARD_PROMPT_TIMEOUT_SECS.to_string())
        .arg("--interactive")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    let mut text_busy_retries = 0;
    let mut child = 'spawn: loop {
        match command.spawn() {
            Ok(child) => break 'spawn child,
            Err(error) if error.raw_os_error() == Some(26) => {
                text_busy_retries += 1;
                if text_busy_retries > 20 {
                    eprintln!(
                        "  Could not launch the Steam Guard dialog ({GUARD_PROMPT_BIN_NAME}): {error}"
                    );
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => {
                eprintln!(
                    "  Could not launch the Steam Guard dialog ({GUARD_PROMPT_BIN_NAME}): {error}"
                );
                return None;
            }
        }
    };
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    Some(GuardDialog {
        child,
        stdin,
        stdout: BufReader::new(stdout).lines(),
    })
}

/// Strip the control-line framing characters and bound the length so an error
/// message can neither inject extra `OK`/`ERR` lines nor overflow the dialog.
fn sanitize_control_message(msg: &str) -> String {
    msg.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .take(200)
        .collect()
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardProviderKind {
    PreSupplied,
    Stdin,
    HelperGui,
    FailFast,
}

/// Run the `steam login` subcommand: interactive per-instance Steam login wizard.
/// Requires `BridgeReady` — boots instances to perform login.
pub(crate) async fn login(
    config: &ClusterConfig<BridgeReady>,
    target: Option<&str>,
    continue_from: bool,
    login_runners_dir: &Path,
    creds: Option<&CredentialsMap>,
    force: bool,
    login_state_dir: Option<&Path>,
    guard_data_path: Option<&Path>,
    guard_provider: &GuardProvider,
) -> anyhow::Result<LoginOutcome> {
    let targets = config.resolve_targets(target, continue_from)?;
    let login_state_dir = match login_state_dir {
        Some(path) if std::fs::read_dir(path).is_ok() => Some(path),
        Some(path) => {
            println!(
                "==> Cannot read loginStateDir {}; cannot fill-the-gaps. Logging in every selected VM.",
                path.display()
            );
            None
        }
        None => {
            println!(
                "==> No loginStateDir configured; cannot fill-the-gaps. Logging in every selected VM."
            );
            None
        }
    };

    let _ = guard_data_path;

    let mut failed = 0usize;
    let mut logged_in = 0usize;
    let mut skipped = 0usize;
    for vm in &targets {
        if !force && let Some(login_state_dir) = login_state_dir {
            let vm_name = vm.name.to_string();
            let status = accounts::classify_session_status(
                login_state_dir,
                &vm_name,
                accounts::DEFAULT_WARN_WITHIN_DAYS,
            );
            if status == SessionStatus::Ok {
                let info = accounts::read_account_info(
                    login_state_dir,
                    vm_name,
                    None,
                    accounts::DEFAULT_WARN_WITHIN_DAYS,
                );
                let persona = info.persona.as_deref().unwrap_or("unknown");
                println!(
                    "  {}: already logged in ({persona}), skipping (use --force to re-login)",
                    vm.name
                );
                skipped += 1;
                continue;
            }
            println!("  {}: {}, logging in", vm.name, status.login_reason());
        }

        let vm_creds = creds.and_then(|c| c.get::<str>(&vm.name));
        let mut log = LoginLog::stdout();
        let login_reason = login_reason_for_vm(login_state_dir, vm, force);
        match login_single_vm(
            config,
            vm,
            login_runners_dir,
            vm_creds,
            &login_reason,
            guard_provider,
            &mut log,
        )
        .await
        {
            Ok(LoginOutcome::Completed | LoginOutcome::ManualCompletionNeeded) => logged_in += 1,
            Ok(LoginOutcome::GuardCodeNeeded(needed)) => {
                eprintln!("{needed}");
                return Ok(LoginOutcome::GuardCodeNeeded(needed));
            }
            Err(e) => {
                eprintln!("Error with {}: {e}", vm.name);
                failed += 1;
            }
        }
    }
    println!("==> Logged in: {logged_in}, Skipped: {skipped}, Failed: {failed}");
    if failed > 0 {
        anyhow::bail!("{failed}/{} Steam login target(s) failed", targets.len());
    }
    Ok(LoginOutcome::Completed)
}

async fn login_vm_attempt(
    config: &ClusterConfig<BridgeReady>,
    vm: &VmDef,
    login_runners_dir: &Path,
    creds: Option<&CredentialsMap>,
    login_state_dir: Option<&Path>,
    guard_data_path: Option<&Path>,
    force: bool,
    guard_provider: &GuardProvider,
    log: &mut LoginLog,
) -> LoginAttempt {
    let mut login_reason = login_reason_for_vm(login_state_dir, vm, force);
    if !force && let Some(login_state_dir) = login_state_dir {
        let vm_name = vm.name.to_string();
        let status = accounts::classify_session_status(
            login_state_dir,
            &vm_name,
            accounts::DEFAULT_WARN_WITHIN_DAYS,
        );
        if status == SessionStatus::Ok {
            let info = accounts::read_account_info(
                login_state_dir,
                vm_name,
                None,
                accounts::DEFAULT_WARN_WITHIN_DAYS,
            );
            let persona = info.persona.as_deref().unwrap_or("unknown");
            return LoginAttempt::Skipped(format!(
                "  {}: already logged in ({persona}), skipping (use --force to re-login)",
                vm.name
            ));
        }
        login_reason = status.login_reason().to_string();
        log.line(format!(
            "  {}: {}, logging in",
            vm.name,
            status.login_reason()
        ));
    }

    let Some(vm_creds) = creds.and_then(|c| c.get::<str>(&vm.name)) else {
        return LoginAttempt::Failed(anyhow::anyhow!(
            "credentials for {} are required for parallel automated Steam login",
            vm.name
        ));
    };
    match login_single_vm_automated(
        config,
        vm,
        login_runners_dir,
        vm_creds,
        &login_reason,
        guard_provider,
        login_state_dir,
        guard_data_path,
        log,
    )
    .await
    {
        Ok(LoginOutcome::Completed | LoginOutcome::ManualCompletionNeeded) => {
            LoginAttempt::LoggedIn
        }
        Ok(LoginOutcome::GuardCodeNeeded(needed)) => LoginAttempt::GuardCodeNeeded(needed),
        Err(e) => LoginAttempt::Failed(e),
    }
}

fn login_reason_for_vm(login_state_dir: Option<&Path>, vm: &VmDef, force: bool) -> String {
    if force {
        return "forced re-login".to_string();
    }
    login_state_dir
        .map(|login_state_dir| {
            accounts::classify_session_status(
                login_state_dir,
                vm.name.as_ref(),
                accounts::DEFAULT_WARN_WITHIN_DAYS,
            )
            .login_reason()
            .to_string()
        })
        .unwrap_or_else(|| "first login".to_string())
}

fn finish_login_reports(
    mut results: Vec<LoginReport>,
    target_count: usize,
) -> anyhow::Result<LoginOutcome> {
    results.sort_by_key(|result| result.vm_index);

    let mut failed = 0usize;
    let mut logged_in = 0usize;
    let mut skipped = 0usize;
    let mut guard_needed = None;
    for result in results {
        if !result.output.is_empty() {
            println!("{}", result.output);
        }
        match result.attempt {
            LoginAttempt::Skipped(reason) => {
                println!("{reason}");
                skipped += 1;
            }
            LoginAttempt::LoggedIn => logged_in += 1,
            LoginAttempt::GuardCodeNeeded(needed) => {
                eprintln!("{needed}");
                guard_needed.get_or_insert(needed);
                failed += 1;
            }
            LoginAttempt::Failed(e) => {
                eprintln!("Error with {}: {e}", result.vm_name);
                failed += 1;
            }
        }
    }

    println!("==> Logged in: {logged_in}, Skipped: {skipped}, Failed: {failed}");
    if failed > 0 {
        if let Some(needed) = guard_needed {
            return Ok(LoginOutcome::GuardCodeNeeded(needed));
        }
        anyhow::bail!("{failed}/{target_count} Steam login target(s) failed");
    }
    Ok(LoginOutcome::Completed)
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
            let mut log = LoginLog::stdout();
            finish_steam_login(
                backend,
                vm,
                vm_creds,
                &config.vm_user,
                &config.state_dir,
                &password_secrets,
                &mut log,
            )
            .await?;
            shutdown_steam_login_vm(backend, config, vm).await;
            return Ok(());
        }
        SteamCmdWait::GuardRequired => {}
    }

    let code = match code {
        Some(code) => code.trim().to_owned(),
        None => read_guard_code_line_from_stdin()?.unwrap_or_default(),
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
    let mut log = LoginLog::stdout();
    finish_steam_login(
        backend,
        vm,
        vm_creds,
        &config.vm_user,
        &config.state_dir,
        &secrets,
        &mut log,
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

fn read_guard_code_from_stdin(cx: &GuardPromptContext<'_>) -> anyhow::Result<GuardCodeOutcome> {
    if cx.invalid_retry {
        println!(
            "Steam Guard code for {} on {} was invalid. Enter a new code:",
            cx.steam_user, cx.vm_name
        );
    } else {
        println!(
            "Steam Guard code for {} on {} ({}):",
            cx.steam_user, cx.vm_name, cx.reason
        );
    }
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    read_guard_code_from_reader(&mut stdin)
}

fn read_guard_code_from_reader<R: BufRead>(reader: &mut R) -> anyhow::Result<GuardCodeOutcome> {
    Ok(read_stdin_line(reader)?
        .map(|code| code.trim().to_owned())
        .map(GuardCodeOutcome::Code)
        .unwrap_or(GuardCodeOutcome::Unavailable))
}

fn read_guard_code_line_from_stdin() -> anyhow::Result<Option<String>> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    Ok(read_stdin_line(&mut stdin)?.map(|code| code.trim().to_owned()))
}

async fn login_single_vm_automated(
    _config: &ClusterConfig<BridgeReady>,
    vm: &VmDef,
    _login_runners_dir: &Path,
    creds: &VmCredentials,
    login_reason: &str,
    guard_provider: &GuardProvider,
    login_state_dir: Option<&Path>,
    guard_data_path: Option<&Path>,
    log: &mut LoginLog,
) -> anyhow::Result<LoginOutcome> {
    log.blank();
    log.line("══════════════════════════════════════════════════");
    log.line(format!("  Steam login for {} ({})", vm.name, vm.ip));
    log.line("  Mode: headless token mint (no in-VM Steam GUI)");
    log.line("══════════════════════════════════════════════════");

    let vm_name = vm.name.to_string();
    mint_and_write_session(
        creds,
        &vm_name,
        login_reason,
        guard_provider,
        login_state_dir,
        guard_data_path,
        log,
    )
    .await
}

/// Refresh SteamClient sessions host-side with configured credentials.
///
/// This is deliberately independent of runners, SSH, bridge setup, and VM
/// startup. Existing valid login-state artifacts are left untouched unless
/// `force` is set.
pub(crate) async fn refresh<S>(
    _config: &ClusterConfig<S>,
    _target: Option<&str>,
    _continue_from: bool,
    _creds: Option<&CredentialsMap>,
    _force: bool,
    _login_state_dir: Option<&Path>,
    _guard_data_path: Option<&Path>,
    _guard_provider: &GuardProvider,
) -> anyhow::Result<LoginOutcome> {
    anyhow::bail!(
        "steam refresh cannot satisfy the Steam GUI client. Run `cluster-ctl steam login <vm> --force` to create a GUI-valid Steam session."
    )
}

/// Mint a SteamClient session headlessly and persist it to the VM's login-state
/// share.
///
/// No VM boot, SSH, steamcmd, or in-VM Steam GUI is involved: the
/// SteamClient-audience refresh token is minted host-side via
/// [`crate::game::steam_auth::mint_session`] (with the Guard code obtained
/// through `guard_provider`) and written via
/// [`crate::game::steam_session::write_session`] so the host detector reports
/// `Ok` and the VM's Steam client can auto-login from it.
async fn mint_and_write_session(
    creds: &VmCredentials,
    vm_name: &str,
    login_reason: &str,
    guard_provider: &GuardProvider,
    login_state_dir: Option<&Path>,
    guard_data_path: Option<&Path>,
    log: &mut LoginLog,
) -> anyhow::Result<LoginOutcome> {
    let Some(login_state_dir) = login_state_dir else {
        anyhow::bail!(
            "cannot persist Steam session for {vm_name}: no loginStateDir configured \
             (set services.steampipe-cluster's loginStateDir, or pass --login-state-dir)"
        );
    };

    log.line(format!(
        "  Minting Steam session for {} (headless, no in-VM GUI)...",
        creds.steam_user
    ));
    match crate::game::steam_auth::mint_session(
        creds,
        vm_name,
        login_reason,
        guard_provider,
        guard_data_path,
    )
    .await?
    {
        crate::game::steam_auth::MintOutcome::Minted(session) => {
            crate::game::steam_session::write_session(login_state_dir, vm_name, &session)
                .map_err(|e| anyhow::anyhow!("writing Steam session for {vm_name}: {e}"))?;
            let expiry = if session.expires_at > 0 {
                let days = (session.expires_at - chrono::Utc::now().timestamp()) / 86_400;
                format!(", token expires in ~{days} days")
            } else {
                String::new()
            };
            log.line(format!(
                "  Login successful (SteamID {}{expiry}); session written to login state.",
                session.steam_id64
            ));
            Ok(LoginOutcome::Completed)
        }
        crate::game::steam_auth::MintOutcome::GuardCodeNeeded(needed) => {
            Ok(LoginOutcome::GuardCodeNeeded(needed))
        }
    }
}

async fn login_single_vm(
    config: &ClusterConfig<BridgeReady>,
    vm: &VmDef,
    login_runners_dir: &Path,
    creds: Option<&VmCredentials>,
    login_reason: &str,
    guard_provider: &GuardProvider,
    log: &mut LoginLog,
) -> anyhow::Result<LoginOutcome> {
    let automated = creds.is_some();
    let backend = &config.backend;

    log.blank();
    log.line("══════════════════════════════════════════════════");
    log.line(format!("  Steam login for {} ({})", vm.name, vm.ip));
    if automated {
        log.line("  Mode: automated (credentials from TOML)");
    } else {
        log.line("  Mode: interactive (VNC)");
        log.line("  Press Ctrl+C at any time to cancel and shut down the VM");
    }
    log.line("══════════════════════════════════════════════════");

    let reachable = automated && backend.is_reachable(&vm.ip).await;
    let started_vm = match login_boot_action(automated, reachable) {
        LoginBootAction::ReuseRunning => {
            log.line(format!("  {}: already up, skipping reboot", vm.name));
            false
        }
        LoginBootAction::Restart => {
            backend.stop_instance(config, vm);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            log.line("  Booting VM...");
            crate::core::admission::admit().await;
            let pid = backend.start_instance(config, vm, login_runners_dir)?;
            lease::write_claim(vm.index, &config.lock_dir, &config.cluster_name, pid)?;
            true
        }
    };

    let config_for_cleanup = config.clone();
    let vm_for_cleanup = vm.clone();
    let cleanup = move || {
        config_for_cleanup
            .backend
            .stop_instance(&config_for_cleanup, &vm_for_cleanup);
        lease::remove_claim(vm_for_cleanup.index, &config_for_cleanup.lock_dir);
    };

    if started_vm {
        if !backend.wait_ready(&vm.ip, LOGIN_SSH_TIMEOUT_SECS).await {
            log.line(format!(
                "  Waiting for SSH... timeout after {LOGIN_SSH_TIMEOUT_SECS}s"
            ));
            print_vm_log_tail(&config.state_dir, vm, log);
            cleanup();
            anyhow::bail!("SSH timeout for {}", vm.name);
        }
        log.line("  Waiting for SSH... ready");
    }

    let outcome = if let Some(creds) = creds {
        // Automated login via SteamCMD state bootstrap + GUI validation.
        automated_login(
            backend,
            vm,
            creds,
            &config.vm_user,
            &config.state_dir,
            login_reason,
            guard_provider,
            log,
        )
        .await?
    } else {
        // Interactive VNC-based login (original flow)
        interactive_login(backend, vm, &config.vm_user).await?
    };

    if matches!(outcome, LoginOutcome::ManualCompletionNeeded) {
        log.line(format!(
            "  {} is awaiting manual Steam login completion.",
            vm.name
        ));
        log.line("  After completing Steam login, validate with `cluster-ctl steam accounts`.");
        log.line("  Stop the login VM with `cluster-ctl down` when you are done.");
        return Ok(outcome);
    }

    if matches!(outcome, LoginOutcome::GuardCodeNeeded(_)) {
        return Ok(outcome);
    }

    if !started_vm {
        log.line(format!("  {} done.", vm.name));
        return Ok(outcome);
    }

    log.line("  Shutting down Steam and VM...");
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
    log.line(format!("  {} done.", vm.name));

    Ok(outcome)
}

pub fn vm_log_tail(state_dir: &Path, vm: &VmDef) -> Option<String> {
    let path = microvm_log_path(state_dir, vm);
    let log = read_vm_log_lossy(&path).ok()?;
    // vm.log is host-side hypervisor output; remote command stdout/stderr is
    // still redacted separately before being included in user-facing errors.
    Some(vm_log_tail_lines(&log).join("\n"))
}

fn print_vm_log_tail(state_dir: &Path, vm: &VmDef, log: &mut LoginLog) {
    let path = microvm_log_path(state_dir, vm);
    let tail_log = match read_vm_log_lossy(&path) {
        Ok(tail_log) => tail_log,
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                log.line(format!(
                    "  VM log unavailable: {} (log file does not exist; backend may not have started the VM)",
                    path.display()
                ));
            } else {
                log.line(format!("  VM log unavailable: {} ({err})", path.display()));
            }
            return;
        }
    };
    let tail = vm_log_tail_lines(&tail_log).join("\n");
    if tail.is_empty() {
        log.line(format!(
            "  VM log unavailable: {} (log file is empty)",
            path.display()
        ));
    } else {
        log.line(format!("  --- {} tail ---", path.display()));
        for line in tail.lines() {
            log.line(format!("  {line}"));
        }
        log.line("  --- end VM log tail ---");
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
    _config: &ClusterConfig<S>,
    vms: &[VmDef],
    creds: &CredentialsMap,
    login_state_dir: Option<&Path>,
    guard_data_path: Option<&Path>,
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
    let login_state_dir = login_state_dir.map(Path::to_path_buf);
    let _ = guard_data_path;

    let results = par_each_vm(vms, |vm| {
        let creds = creds.clone();
        let login_state_dir = login_state_dir.clone();
        async move {
            let Some(vm_creds) = creds.get::<str>(&vm.name) else {
                return AutoLoginReport::no_credentials(vm.name);
            };
            let _ = vm_creds;
            let vm_name = vm.name.to_string();
            match auto_login_action(login_state_dir.as_deref(), &vm_name) {
                AutoLoginAction::UseExisting => {
                    return AutoLoginReport::ok(
                        vm.name,
                        "existing Steam session OK".to_string(),
                    );
                }
                AutoLoginAction::LoginRequired { status } => {
                    eprintln!(
                        "  {}: {}. Run `cluster-ctl steam login {} --force` to create a GUI-valid Steam session.",
                        vm.name,
                        status.login_reason(),
                        vm.name
                    );
                    AutoLoginReport::failed(vm.name)
                }
                AutoLoginAction::MissingLoginStateDir => {
                    eprintln!(
                        "  {}: login failed: no loginStateDir configured; cannot persist a Steam session",
                        vm.name
                    );
                    AutoLoginReport::failed(vm.name)
                }
            }
        }
    })
    .await?;

    let mut ok = 0;
    let mut fail = 0;
    for report in &results {
        match &report.result {
            Some(Ok(message)) => {
                println!("  {}: {message}", report.name);
                ok += 1;
            }
            Some(Err(())) => fail += 1,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AutoLoginReport {
    name: VmName,
    result: Option<Result<String, ()>>,
}

impl AutoLoginReport {
    fn no_credentials(name: VmName) -> Self {
        Self { name, result: None }
    }

    fn ok(name: VmName, message: String) -> Self {
        Self {
            name,
            result: Some(Ok(message)),
        }
    }

    fn failed(name: VmName) -> Self {
        Self {
            name,
            result: Some(Err(())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoLoginAction {
    UseExisting,
    LoginRequired { status: SessionStatus },
    MissingLoginStateDir,
}

fn auto_login_action(login_state_dir: Option<&Path>, vm_name: &str) -> AutoLoginAction {
    let Some(login_state_dir) = login_state_dir else {
        return AutoLoginAction::MissingLoginStateDir;
    };
    let status = accounts::classify_session_status(
        login_state_dir,
        vm_name,
        accounts::DEFAULT_WARN_WITHIN_DAYS,
    );
    if status == SessionStatus::Ok {
        AutoLoginAction::UseExisting
    } else {
        AutoLoginAction::LoginRequired { status }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    Completed,
    GuardCodeNeeded(GuardCodeNeeded),
    ManualCompletionNeeded,
}

/// Automated login: starts Steam GUI, types credentials, drives Steam Guard via
/// the host prompt helper, and waits for real GUI-side session evidence.
async fn automated_login(
    backend: &Backend,
    vm: &VmDef,
    creds: &VmCredentials,
    vm_user: &str,
    state_dir: &Path,
    login_reason: &str,
    guard_provider: &GuardProvider,
    log: &mut LoginLog,
) -> anyhow::Result<LoginOutcome> {
    log.line(format!("  Logging in as {}...", creds.steam_user));
    log.line("  Starting Steam GUI and submitting credentials...");

    let password_secrets = [&creds.steam_pass as &str];
    let started = backend
        .run_cmd(
            &vm.ip,
            &steam_gui_submit_credentials_script(vm_user, &creds.steam_user, &creds.steam_pass),
        )
        .await;
    let stdout = redact_sensitive(&started.stdout, &password_secrets);
    let stderr = redact_sensitive(&started.stderr, &password_secrets);
    if !started.success || !steam_gui_credentials_submitted(&stdout) {
        if stdout.trim().is_empty() && stderr.trim().is_empty() {
            let tail = vm_log_tail(state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
            anyhow::bail!(
                "{}: Steam GUI credential entry produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
                vm.name
            );
        }
        anyhow::bail!(
            "{}: Steam GUI credential entry failed. stdout: {} stderr: {}",
            vm.name,
            stdout,
            stderr
        );
    }

    let mut guard_session = guard_provider
        .open_retry_session(&vm.name, &creds.steam_user, login_reason)
        .await;
    let mut previous_error: Option<String> = None;
    for attempt in 0..8 {
        match wait_for_steam_gui_login(backend, vm, vm_user, state_dir, &password_secrets).await? {
            SteamGuiWait::LoggedIn => {
                guard_session.confirm_success().await;
                finish_steam_gui_login(backend, vm, creds, log).await?;
                return Ok(LoginOutcome::Completed);
            }
            SteamGuiWait::GuardRequired if attempt < 7 => {
                log.line(format!("  Steam Guard required for {}.", creds.steam_user));
                let mut code = match guard_session.next_code(previous_error.as_deref()).await {
                    GuardRetryNext::Code(code) => code,
                    GuardRetryNext::Cancelled | GuardRetryNext::Unavailable => {
                        return Ok(LoginOutcome::GuardCodeNeeded(GuardCodeNeeded::new(
                            vm,
                            &creds.steam_user,
                            login_reason,
                        )));
                    }
                };
                if code.trim().is_empty() {
                    code.zeroize();
                    return Ok(LoginOutcome::GuardCodeNeeded(GuardCodeNeeded::new(
                        vm,
                        &creds.steam_user,
                        login_reason,
                    )));
                }
                log.line("  Typing Steam Guard code into Steam GUI...");
                let typed = backend
                    .run_cmd(&vm.ip, &steam_gui_submit_guard_code_script(vm_user, &code))
                    .await;
                let secrets = [&creds.steam_pass as &str, code.as_str()];
                let stdout = redact_sensitive(&typed.stdout, &secrets);
                let stderr = redact_sensitive(&typed.stderr, &secrets);
                code.zeroize();
                if !typed.success || !steam_gui_guard_submitted(&stdout) {
                    anyhow::bail!(
                        "{}: Steam Guard GUI entry failed. stdout: {} stderr: {}",
                        vm.name,
                        stdout,
                        stderr
                    );
                }
                match wait_for_steam_gui_login_with_timeout(
                    backend,
                    vm,
                    vm_user,
                    state_dir,
                    &password_secrets,
                    45,
                    false,
                )
                .await?
                {
                    SteamGuiWait::LoggedIn => {
                        guard_session.confirm_success().await;
                        finish_steam_gui_login(backend, vm, creds, log).await?;
                        return Ok(LoginOutcome::Completed);
                    }
                    SteamGuiWait::BadCredentials => {
                        anyhow::bail!(
                            "{}: Steam rejected the configured credentials for {}",
                            vm.name,
                            creds.steam_user
                        );
                    }
                    SteamGuiWait::GuardRequired | SteamGuiWait::Timeout => {
                        previous_error = Some(
                            "Steam Guard code was rejected or did not complete login; enter the latest code."
                                .to_string(),
                        );
                    }
                }
            }
            SteamGuiWait::GuardRequired => {
                return Ok(LoginOutcome::GuardCodeNeeded(GuardCodeNeeded::new(
                    vm,
                    &creds.steam_user,
                    login_reason,
                )));
            }
            SteamGuiWait::BadCredentials => {
                anyhow::bail!(
                    "{}: Steam rejected the configured credentials for {}",
                    vm.name,
                    creds.steam_user
                );
            }
            SteamGuiWait::Timeout => {
                anyhow::bail!(
                    "{}: timed out waiting for Steam GUI login success, rejected credentials, or Steam Guard UI evidence",
                    vm.name
                );
            }
        }
    }

    anyhow::bail!(
        "{}: Steam Guard was not accepted after repeated GUI attempts",
        vm.name
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SteamGuiWait {
    LoggedIn,
    GuardRequired,
    BadCredentials,
    Timeout,
}

async fn wait_for_steam_gui_login(
    backend: &Backend,
    vm: &VmDef,
    vm_user: &str,
    state_dir: &Path,
    secrets: &[&str],
) -> anyhow::Result<SteamGuiWait> {
    wait_for_steam_gui_login_with_timeout(backend, vm, vm_user, state_dir, secrets, 90, true).await
}

async fn wait_for_steam_gui_login_with_timeout(
    backend: &Backend,
    vm: &VmDef,
    vm_user: &str,
    state_dir: &Path,
    secrets: &[&str],
    timeout_secs: u64,
    guard_candidate_fallback: bool,
) -> anyhow::Result<SteamGuiWait> {
    let wait = backend
        .run_cmd(
            &vm.ip,
            &steam_gui_wait_script(vm_user, timeout_secs, guard_candidate_fallback),
        )
        .await;
    let stdout = redact_sensitive(&wait.stdout, secrets);
    let stderr = redact_sensitive(&wait.stderr, secrets);
    if stdout
        .lines()
        .any(|line| line.trim() == "STEAM_GUI_LOGIN_OK")
    {
        return Ok(SteamGuiWait::LoggedIn);
    }
    if stdout
        .lines()
        .any(|line| line.trim() == "STEAM_GUI_GUARD_REQUIRED")
    {
        return Ok(SteamGuiWait::GuardRequired);
    }
    if stdout
        .lines()
        .any(|line| line.trim() == "STEAM_GUI_BAD_CREDENTIALS")
    {
        return Ok(SteamGuiWait::BadCredentials);
    }
    if stdout
        .lines()
        .any(|line| line.trim() == "STEAM_GUI_WAIT_TIMEOUT")
    {
        return Ok(SteamGuiWait::Timeout);
    }
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        let tail = vm_log_tail(state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
        anyhow::bail!(
            "{}: Steam GUI wait produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
            vm.name
        );
    }
    anyhow::bail!(
        "{}: Steam GUI wait failed. stdout: {} stderr: {}",
        vm.name,
        stdout,
        stderr
    )
}

async fn complete_guard_login_with_provider(
    backend: &Backend,
    vm: &VmDef,
    creds: &VmCredentials,
    vm_user: &str,
    state_dir: &Path,
    login_reason: &str,
    guard_provider: &GuardProvider,
    log: &mut LoginLog,
) -> anyhow::Result<SteamCmdWait> {
    let mut invalid_retry = false;
    loop {
        let cx = GuardPromptContext {
            vm_name: &vm.name,
            steam_user: &creds.steam_user,
            reason: login_reason,
            invalid_retry,
        };
        log.line(format!("  Steam Guard required for {}.", creds.steam_user));
        let mut code = match guard_provider.request(&cx).await? {
            GuardCodeOutcome::Code(code) => code,
            GuardCodeOutcome::Cancelled | GuardCodeOutcome::Unavailable => {
                let _ = kill_steamcmd_login_session(backend, vm, vm_user).await;
                return Ok(SteamCmdWait::GuardRequired);
            }
        };
        if code.trim().is_empty() {
            code.zeroize();
            let _ = kill_steamcmd_login_session(backend, vm, vm_user).await;
            return Ok(SteamCmdWait::GuardRequired);
        }

        // Feed the code into the LIVE SteamCMD session that is already parked at
        // the `Steam Guard code:` prompt. The initial no-code bootstrap is what
        // triggered Steam to *issue* the code (email Steam Guard sends the code in
        // response to the login attempt; it cannot be known beforehand), so we must
        // submit into that same attempt rather than start a fresh positional login.
        // A pre-supplied `--code`/env value (only meaningful for the on-demand
        // mobile authenticator) flows through this same path.
        log.line("  Submitting Steam Guard code to the live SteamCMD session...");
        let mut submit_script = steamcmd_guard_submit_script(vm_user, &code);
        let secrets = [&creds.steam_pass as &str, code.as_str()];
        let submit = backend.run_cmd(&vm.ip, &submit_script).await;
        submit_script.zeroize();
        if !submit.success || !steamcmd_guard_submitted(&submit.stdout) {
            let stdout = redact_sensitive(&submit.stdout, &secrets);
            let stderr = redact_sensitive(&submit.stderr, &secrets);
            if stdout.trim().is_empty() && stderr.trim().is_empty() {
                let tail =
                    vm_log_tail(state_dir, vm).unwrap_or_else(|| "(vm.log unavailable)".into());
                code.zeroize();
                anyhow::bail!(
                    "{}: Steam Guard submit produced no output; the VM likely crashed during the call. vm.log tail:\n{tail}",
                    vm.name
                );
            }
            code.zeroize();
            anyhow::bail!(
                "{}: Steam Guard submit failed. stdout: {} stderr: {}",
                vm.name,
                stdout,
                stderr
            );
        }

        let wait =
            wait_for_steamcmd_guard_completion(backend, vm, vm_user, state_dir, &secrets).await?;
        code.zeroize();
        match wait {
            SteamCmdWait::Complete => return Ok(SteamCmdWait::Complete),
            SteamCmdWait::GuardRequired => invalid_retry = true,
        }
    }
}

async fn kill_steamcmd_login_session(backend: &Backend, vm: &VmDef, vm_user: &str) -> bool {
    backend
        .run_cmd(&vm.ip, &steamcmd_kill_session_script(vm_user))
        .await
        .success
}

/// Build the no-code SteamCMD bootstrap. Intentionally has **no** positional
/// Guard-code form: `steamcmd +login user pass` is what triggers Steam to issue
/// the code (email Steam Guard mails it in response to this attempt), and the
/// code is then submitted into this same live session by
/// [`steamcmd_guard_submit_script`]. A positional `+login user pass code` would
/// only ever satisfy the on-demand mobile authenticator and cannot work for a
/// first-login email challenge, so it is deliberately not offered.
fn steamcmd_bootstrap_command(vm_user: &str, steam_user: &str, steam_pass: &str) -> String {
    let home = format!("/home/{vm_user}");
    let user = shell_escape(steam_user);
    let pass = shell_escape(steam_pass);
    let login_args = format!("'{user}' '{pass}'");
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
steamcmd +login {login_args} +quit
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

fn steamcmd_kill_session_script(vm_user: &str) -> String {
    format!(
        r#"HOME=/home/{vm_user}
SESSION="steampipe-steamcmd-login"
tmux kill-session -t "$SESSION" 2>/dev/null || true
echo STEAMCMD_SESSION_KILLED"#
    )
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

async fn finish_steam_gui_login(
    backend: &Backend,
    vm: &VmDef,
    creds: &VmCredentials,
    log: &mut LoginLog,
) -> anyhow::Result<()> {
    log.line("  Login successful");

    if let Some(key) = &creds.game_key {
        log.line("  Activating game key...");
        let escaped_key = shell_escape(key);
        let activate_cmd = format!(
            r#"steam steam://registerkey/{escaped_key} &
sleep 10
echo KEY_SUBMITTED"#,
        );
        let key_result = backend.run_cmd(&vm.ip, &activate_cmd).await;
        log.line(format!(
            "  Game key activation submitted ({})",
            key_result.stdout.trim()
        ));
    }

    Ok(())
}

fn steam_gui_submit_credentials_script(
    vm_user: &str,
    steam_user: &str,
    steam_pass: &str,
) -> String {
    let log_dir = format!("/home/{vm_user}/.local/share/Steam/logs");
    let stdout_log = format!("{log_dir}/steampipe_gui_login_stdout.log");
    let user = shell_escape(steam_user);
    let pass = shell_escape(steam_pass);
    let compositor = sway_setup(vm_user);
    format!(
        r#"{compositor}
mkdir -p {log_dir}
: > {stdout_log} 2>/dev/null
pkill -TERM -x steam 2>/dev/null || true
pkill -TERM -x steamwebhelper 2>/dev/null || true
sleep 2
pkill -KILL -x steam 2>/dev/null || true
pkill -KILL -x steamwebhelper 2>/dev/null || true
rm -f /home/{vm_user}/.local/share/Steam/config/loginusers.vdf
if [ -f /home/{vm_user}/.steam/registry.vdf ]; then
    sed -i '/"AutoLoginUser"/d' /home/{vm_user}/.steam/registry.vdf 2>/dev/null || true
fi
launch_steam() {{
    steam -cef-disable-gpu >>{stdout_log} 2>&1 &
    echo "STEAM_GUI_START_PID=$!"
}}
steam_alive() {{
    pgrep -u "$(id -u)" -x steam >/dev/null 2>&1 || \
    pgrep -u "$(id -u)" -x steamwebhelper >/dev/null 2>&1 || \
    pgrep -u "$(id -u)" -f '[/]steamwebhelper|[/]ubuntu12_32[/]steam' >/dev/null 2>&1
}}
launch_steam
find_swaysock() {{
    for socket in "$XDG_RUNTIME_DIR"/sway-ipc.*.sock; do
        if [ -S "$socket" ]; then
            export SWAYSOCK="$socket"
            return 0
        fi
    done
    return 1
}}
focus_login_window() {{
    find_swaysock || return 1
    swaymsg '[title="Sign in to Steam"] focus' >/dev/null 2>&1 && return 0
    return 1
}}
find_login_xwindow() {{
    command -v xdotool >/dev/null 2>&1 || return 1
    DISPLAY=:0 xdotool getwindowfocus 2>/dev/null
}}
type_credentials() {{
    window="$1"
    [ -n "$window" ] || return 1
    DISPLAY=:0 xdotool windowactivate --sync "$window" windowfocus "$window" >/dev/null 2>&1 || return 1
    DISPLAY=:0 xdotool key --clearmodifiers ctrl+a BackSpace
    DISPLAY=:0 xdotool type --clearmodifiers --delay 20 -- '{user}'
    DISPLAY=:0 xdotool key --clearmodifiers Tab
    sleep 0.2
    DISPLAY=:0 xdotool key --clearmodifiers ctrl+a BackSpace
    DISPLAY=:0 xdotool type --clearmodifiers --delay 20 -- '{pass}'
    DISPLAY=:0 xdotool key --clearmodifiers Return
}}
for i in $(seq 1 240); do
    if focus_login_window; then
        sleep 3
        window="$(find_login_xwindow || true)"
        if [ -n "$window" ] && type_credentials "$window"; then
            echo STEAM_GUI_CREDENTIALS_SUBMITTED
            exit 0
        fi
        if ! command -v xdotool >/dev/null 2>&1; then
            echo STEAM_GUI_XDOTOOL_MISSING
            exit 1
        fi
    fi
    if [ "$i" -gt 20 ] && ! steam_alive; then
        echo STEAM_GUI_RELAUNCH_AFTER_EARLY_EXIT
        launch_steam
    fi
    sleep 1
done
echo STEAM_GUI_SIGNIN_TIMEOUT
echo "--- sway tree ---"
find_swaysock && swaymsg -t get_tree 2>/dev/null || true
echo "--- steam stdout/stderr tail ---"
tail -n 80 {stdout_log} 2>/dev/null || true
exit 1"#,
    )
}

fn steam_gui_submit_guard_code_script(vm_user: &str, code: &str) -> String {
    let code = shell_escape(code);
    format!(
        r#"export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
export WAYLAND_DISPLAY=wayland-1
export DISPLAY=:0
for socket in "$XDG_RUNTIME_DIR"/sway-ipc.*.sock; do
    if [ -S "$socket" ]; then
        export SWAYSOCK="$socket"
        break
    fi
done
if [ -n "${{SWAYSOCK:-}}" ]; then
    swaymsg '[title="Sign in to Steam"] focus' >/dev/null 2>&1 || true
    swaymsg '[class="steam"] focus' >/dev/null 2>&1 || true
fi
if ! command -v xdotool >/dev/null 2>&1; then
    echo STEAM_GUI_XDOTOOL_MISSING
    exit 1
fi
window="$(DISPLAY=:0 xdotool getwindowfocus 2>/dev/null)"
if [ -z "$window" ]; then
    window="$(DISPLAY=:0 xdotool search --class steam 2>/dev/null | tail -n1)"
fi
[ -n "$window" ] || exit 1
DISPLAY=:0 xdotool windowactivate --sync "$window" windowfocus "$window" >/dev/null 2>&1
DISPLAY=:0 xdotool key --clearmodifiers ctrl+a BackSpace
DISPLAY=:0 xdotool type --clearmodifiers --delay 20 -- '{code}'
DISPLAY=:0 xdotool key --clearmodifiers Return
echo STEAM_GUI_GUARD_SUBMITTED"#,
    )
}

fn steam_gui_wait_script(
    vm_user: &str,
    timeout_secs: u64,
    guard_candidate_fallback: bool,
) -> String {
    let steam_dir = format!("/home/{vm_user}/.local/share/Steam");
    let log_dir = format!("{steam_dir}/logs");
    let connection_log = format!("{log_dir}/connection_log.txt");
    let webhelper_log = format!("{log_dir}/webhelper_js.txt");
    let steamui_login_log = format!("{log_dir}/steamui_login.txt");
    let guard_candidate_check = if guard_candidate_fallback {
        r#"    if [ "$i" -gt 8 ] && guard_ui_candidate; then
        echo STEAM_GUI_GUARD_REQUIRED
        exit 0
    fi
"#
    } else {
        ""
    };
    format!(
        r#"export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
export WAYLAND_DISPLAY=wayland-1
export DISPLAY=:0
LOGIN_FILE="{steam_dir}/config/loginusers.vdf"
CONNECTION_LOG="{connection_log}"
WEBHELPER_LOG="{webhelper_log}"
STEAMUI_LOGIN_LOG="{steamui_login_log}"
find_swaysock() {{
    for socket in "$XDG_RUNTIME_DIR"/sway-ipc.*.sock; do
        if [ -S "$socket" ]; then
            export SWAYSOCK="$socket"
            return 0
        fi
    done
    return 1
}}
valid_login_file() {{
    [ -s "$LOGIN_FILE" ] || return 1
    grep -Eq '"[0-9]{{17}}"' "$LOGIN_FILE" || return 1
    grep -Eq '"(PersonaName|AccountName)"' "$LOGIN_FILE" || return 1
}}
gui_cache_present() {{
    grep -Eq '"ConnectCache"' "{steam_dir}/local.vdf" 2>/dev/null || \
    grep -Eq '"ConnectCache"' "{steam_dir}/config/config.vdf" 2>/dev/null
}}
logged_on() {{
    grep -Eq '{STEAM_READY_LOG_PATTERN}' "$CONNECTION_LOG" 2>/dev/null || \
    grep -Eq "RecvMsgClientLogOnResponse\(\) : \[U:1:[0-9]+\] 'OK'" "$CONNECTION_LOG" 2>/dev/null
}}
guard_required() {{
    if find_swaysock; then
        swaymsg -t get_tree 2>/dev/null | grep -Eiq 'Steam Guard|verification code|Enter code|Enter the code|authenticator|email code' && return 0
    fi
    grep -Eiq 'Steam Guard|verification code|Enter code|Enter the code|authenticator|email code' "$WEBHELPER_LOG" "$STEAMUI_LOGIN_LOG" 2>/dev/null
}}
guard_ui_candidate() {{
    command -v xdotool >/dev/null 2>&1 || return 1
    DISPLAY=:0 xdotool getwindowfocus getwindowname 2>/dev/null | grep -Eq '^Sign in to Steam$'
}}
bad_credentials() {{
    grep -Eiq 'incorrect password|invalid password|InvalidPassword|account name or password' "$WEBHELPER_LOG" "$STEAMUI_LOGIN_LOG" "$CONNECTION_LOG" 2>/dev/null
}}
for i in $(seq 1 {timeout_secs}); do
    if valid_login_file && gui_cache_present && logged_on; then
        echo STEAM_GUI_LOGIN_OK
        exit 0
    fi
    if bad_credentials; then
        echo STEAM_GUI_BAD_CREDENTIALS
        exit 0
    fi
    if guard_required; then
        echo STEAM_GUI_GUARD_REQUIRED
        exit 0
    fi
{guard_candidate_check}\
    sleep 1
done
echo STEAM_GUI_WAIT_TIMEOUT
echo "--- connection_log.txt tail ---"
tail -n 80 "$CONNECTION_LOG" 2>/dev/null || true
echo "--- webhelper_js.txt tail ---"
tail -n 80 "$WEBHELPER_LOG" 2>/dev/null || true
echo "--- steamui_login.txt tail ---"
tail -n 80 "$STEAMUI_LOGIN_LOG" 2>/dev/null || true
echo "--- sway tree ---"
find_swaysock && swaymsg -t get_tree 2>/dev/null || true"#,
    )
}

async fn finish_steam_login(
    backend: &Backend,
    vm: &VmDef,
    creds: &VmCredentials,
    vm_user: &str,
    state_dir: &Path,
    secrets: &[&str],
    log: &mut LoginLog,
) -> anyhow::Result<()> {
    log.line("  Syncing SteamCMD login state into Steam GUI profile...");
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

    log.line("  Starting Steam GUI for session validation...");
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
    log.line("  Login successful");

    if let Some(key) = &creds.game_key {
        log.line("  Activating game key...");
        let escaped_key = shell_escape(key);
        let activate_cmd = format!(
            r#"steam steam://registerkey/{escaped_key} &
sleep 10
echo KEY_SUBMITTED"#,
        );
        let key_result = backend.run_cmd(&vm.ip, &activate_cmd).await;
        log.line(format!(
            "  Game key activation submitted ({})",
            key_result.stdout.trim()
        ));
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
    for id_dir in "$base"/config/[0-9]*/; do
        [ -d "$id_dir" ] || continue
        id="$(basename "$id_dir")"
        copy_file "$id_dir/local.vdf" "$GUI_CONFIG/$id"
    done
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
            return Ok(LoginOutcome::ManualCompletionNeeded);
        };
        let input = input.trim();

        if input.is_empty() {
            break;
        } else if input == "p" {
            println!("  Paste text (will be typed into focused VM window):");
            let Some(text) = read_stdin_line(&mut stdin)? else {
                println!("  stdin is nonblocking; paste was not sent.");
                println!("  Continue in VNC, then run `cluster-ctl steam accounts` to validate.");
                return Ok(LoginOutcome::ManualCompletionNeeded);
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

fn steam_gui_credentials_submitted(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim() == "STEAM_GUI_CREDENTIALS_SUBMITTED")
}

fn steam_gui_guard_submitted(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim() == "STEAM_GUI_GUARD_SUBMITTED")
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
    use crate::core::config::IpAddr;
    use base64::Engine as _;
    use std::os::unix::fs::PermissionsExt;

    fn test_jwt(exp: i64) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        format!("{header}.{payload}.")
    }

    fn write_login_state(root: &Path, vm_name: &str, steam_id: &str, exp: i64) {
        let config_dir = root.join(vm_name).join("config");
        let token_dir = config_dir.join(steam_id);
        std::fs::create_dir_all(&token_dir).expect("create token dir");
        std::fs::write(
            config_dir.join("loginusers.vdf"),
            format!(
                r#""users"
{{
    "{steam_id}"
    {{
        "PersonaName"    "{vm_name}"
    }}
}}
"#
            ),
        )
        .expect("write loginusers.vdf");
        std::fs::write(
            token_dir.join("local.vdf"),
            format!(r#""RefreshToken_{steam_id}"   "{}""#, test_jwt(exp)),
        )
        .expect("write local.vdf");
    }

    fn write_gui_login_state(root: &Path, vm_name: &str, steam_id: &str) {
        let login_dir = root.join(vm_name);
        let config_dir = login_dir.join("config");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::write(
            config_dir.join("loginusers.vdf"),
            format!(
                r#""users"
{{
    "{steam_id}"
    {{
        "PersonaName"    "{vm_name}"
    }}
}}
"#
            ),
        )
        .expect("write loginusers.vdf");
        std::fs::write(
            login_dir.join("local.vdf"),
            r#""MachineUserConfigStore"
{
    "Software"
    {
        "Valve"
        {
            "Steam"
            {
                "ConnectCache"
                {
                    "ea8626751" "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                }
            }
        }
    }
}
"#,
        )
        .expect("write local.vdf");
    }

    #[test]
    fn auto_login_action_skips_healthy_persisted_session() {
        let temp = tempfile::tempdir().expect("temp login state dir");
        write_gui_login_state(temp.path(), "vm-1", "76561198000000001");

        assert_eq!(
            auto_login_action(Some(temp.path()), "vm-1"),
            AutoLoginAction::UseExisting
        );
    }

    #[test]
    fn auto_login_action_requires_gui_login_for_unhealthy_persisted_sessions() {
        let temp = tempfile::tempdir().expect("temp login state dir");
        let now = chrono::Utc::now().timestamp();
        write_login_state(
            temp.path(),
            "vm-stale",
            "76561198000000002",
            now + 7 * 86_400,
        );
        write_login_state(temp.path(), "vm-expired", "76561198000000003", now - 86_400);
        std::fs::create_dir_all(temp.path().join("vm-no-token").join("config"))
            .expect("create no-token state");

        assert_eq!(
            auto_login_action(Some(temp.path()), "vm-stale"),
            AutoLoginAction::LoginRequired {
                status: SessionStatus::NoToken
            }
        );
        assert_eq!(
            auto_login_action(Some(temp.path()), "vm-expired"),
            AutoLoginAction::LoginRequired {
                status: SessionStatus::NoToken
            }
        );
        assert_eq!(
            auto_login_action(Some(temp.path()), "vm-no-token"),
            AutoLoginAction::LoginRequired {
                status: SessionStatus::NoToken
            }
        );
        assert_eq!(
            auto_login_action(Some(temp.path()), "vm-missing"),
            AutoLoginAction::LoginRequired {
                status: SessionStatus::Unknown
            }
        );
    }

    #[test]
    fn auto_login_action_requires_login_state_dir() {
        assert_eq!(
            auto_login_action(None, "vm-1"),
            AutoLoginAction::MissingLoginStateDir
        );
    }

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
    fn steam_login_automated_reachable_vm_skips_bounce() {
        assert_eq!(login_boot_action(true, true), LoginBootAction::ReuseRunning);
    }

    #[test]
    fn steam_login_automated_unreachable_vm_bounces() {
        assert_eq!(login_boot_action(true, false), LoginBootAction::Restart);
    }

    #[test]
    fn steam_login_interactive_reachable_vm_still_bounces() {
        assert_eq!(login_boot_action(false, true), LoginBootAction::Restart);
    }

    #[test]
    fn guard_provider_selects_code_stdin_or_failfast() {
        assert_eq!(
            GuardProvider::select(
                Some(" 12345 ".to_string()),
                LoginContext::synthetic(false, false, false),
                true,
            )
            .kind(),
            GuardProviderKind::PreSupplied
        );
        assert_eq!(
            GuardProvider::select(None, LoginContext::synthetic(false, true, false), true).kind(),
            GuardProviderKind::Stdin
        );
        assert_eq!(
            GuardProvider::select(None, LoginContext::synthetic(true, true, true), false).kind(),
            GuardProviderKind::FailFast
        );
    }

    #[test]
    fn guard_code_needed_message_is_machine_distinguishable_and_actionable() {
        let message =
            GuardCodeNeeded::for_vm_name("vm-2", "cached Steam session is missing").to_string();
        assert!(message.contains("STEAM_GUARD_CODE_NEEDED"));
        assert!(message.contains("cluster-ctl steam login vm-2"));
        assert!(message.contains("--code <CODE>"));
        assert!(message.contains("STEAMPIPE_GUARD_CODE"));
    }

    #[test]
    #[ignore = "documents real-backend login ordering; run against atlas with cluster-ctl steam login all"]
    fn steam_login_parallel_output_is_ordered_by_vm_index_smoke() {}

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
    fn steamcmd_bootstrap_command_has_no_positional_guard_code() {
        // The bootstrap must remain a no-code login attempt: that attempt is what
        // makes Steam issue the Guard code. The code is submitted separately into
        // the live session via steamcmd_guard_submit_script (tmux send-keys).
        let script = steamcmd_bootstrap_command("chessbender", "account", "password");
        assert!(script.contains("steamcmd +login 'account' 'password' +quit"));
        assert!(!script.contains("steamcmd +login 'account' 'password' '"));
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
        assert!(script.contains("config/[0-9]*/"));
        assert!(script.contains("copy_file \"$id_dir/local.vdf\" \"$GUI_CONFIG/$id\""));
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
    fn steam_gui_submit_credentials_drives_sway_and_xdotool() {
        let script = steam_gui_submit_credentials_script("chessbender", "account", "pa'ss");
        assert!(script.contains("steam -cef-disable-gpu"));
        assert!(
            script.contains("rm -f /home/chessbender/.local/share/Steam/config/loginusers.vdf")
        );
        assert!(script.contains("AutoLoginUser"));
        assert!(script.contains("swaymsg '[title=\"Sign in to Steam\"] focus'"));
        assert!(script.contains("sleep 3"));
        assert!(script.contains("command -v xdotool"));
        assert!(script.contains("DISPLAY=:0 xdotool getwindowfocus"));
        assert!(script.contains(
            "DISPLAY=:0 xdotool windowactivate --sync \"$window\" windowfocus \"$window\""
        ));
        assert!(script.contains("DISPLAY=:0 xdotool key --clearmodifiers ctrl+a BackSpace"));
        assert!(
            script.contains("DISPLAY=:0 xdotool type --clearmodifiers --delay 20 -- 'account'")
        );
        assert!(script.contains("DISPLAY=:0 xdotool key --clearmodifiers Tab"));
        assert!(
            script.contains("DISPLAY=:0 xdotool type --clearmodifiers --delay 20 -- 'pa'\\''ss'")
        );
        assert!(script.contains("DISPLAY=:0 xdotool key --clearmodifiers Return"));
        assert!(script.contains("STEAM_GUI_XDOTOOL_MISSING"));
        assert!(script.contains("STEAM_GUI_CREDENTIALS_SUBMITTED"));
        assert!(script.contains("STEAM_GUI_RELAUNCH_AFTER_EARLY_EXIT"));
        assert!(script.contains("steam_alive"));
        assert!(script.contains("swaymsg -t get_tree"));
        assert!(!script.contains("wtype -- 'account'"));
        assert!(!script.contains("steam -silent -login"));
    }

    #[test]
    fn steam_gui_wait_requires_login_file_gui_cache_and_logged_on_evidence() {
        let script = steam_gui_wait_script("chessbender", 7, true);
        let post_guard_script = steam_gui_wait_script("chessbender", 7, false);
        assert!(
            script.contains(
                "LOGIN_FILE=\"/home/chessbender/.local/share/Steam/config/loginusers.vdf\""
            )
        );
        assert!(script.contains("valid_login_file"));
        assert!(script.contains("gui_cache_present"));
        assert!(script.contains("logged_on"));
        assert!(script.contains("guard_required"));
        assert!(script.contains("guard_ui_candidate"));
        assert!(script.contains("DISPLAY=:0 xdotool getwindowfocus getwindowname"));
        assert!(script.contains("[ \"$i\" -gt 8 ] && guard_ui_candidate"));
        assert!(!post_guard_script.contains("[ \"$i\" -gt 8 ] && guard_ui_candidate"));
        assert!(script.contains("Enter the code"));
        assert!(script.contains("bad_credentials"));
        assert!(script.contains("STEAM_GUI_LOGIN_OK"));
        assert!(script.contains("STEAM_GUI_GUARD_REQUIRED"));
        assert!(script.contains("STEAM_GUI_BAD_CREDENTIALS"));
        assert!(script.contains("STEAM_GUI_WAIT_TIMEOUT"));
        assert!(script.contains("swaymsg -t get_tree"));
        assert!(script.contains("webhelper_js.txt"));
        assert!(script.contains("steamui_login.txt"));
        assert!(script.contains("RecvMsgClientLogOnResponse\\(\\) : \\[U:1:[0-9]+\\] 'OK'"));
    }

    #[test]
    fn steam_gui_marker_parsers_require_exact_marker_lines() {
        assert!(steam_gui_credentials_submitted(
            "noise\nSTEAM_GUI_CREDENTIALS_SUBMITTED\n"
        ));
        assert!(!steam_gui_credentials_submitted(
            "STEAM_GUI_CREDENTIALS_SUBMITTED_EXTRA\n"
        ));
        assert!(steam_gui_guard_submitted(
            "noise\nSTEAM_GUI_GUARD_SUBMITTED\n"
        ));
        assert!(!steam_gui_guard_submitted(
            "prefix STEAM_GUI_GUARD_SUBMITTED\n"
        ));
    }

    #[test]
    fn steam_gui_guard_submission_uses_focused_gui_window() {
        let script = steam_gui_submit_guard_code_script("chessbender", "6KPPX");
        assert!(script.contains("swaymsg '[title=\"Sign in to Steam\"] focus'"));
        assert!(script.contains("swaymsg '[class=\"steam\"] focus'"));
        assert!(script.contains("command -v xdotool"));
        assert!(script.contains("DISPLAY=:0 xdotool getwindowfocus"));
        assert!(script.contains("DISPLAY=:0 xdotool type --clearmodifiers --delay 20 -- '6KPPX'"));
        assert!(script.contains("DISPLAY=:0 xdotool key --clearmodifiers Return"));
        assert!(script.contains("STEAM_GUI_GUARD_SUBMITTED"));
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

    #[test]
    fn display_probe_accepts_wayland_or_x11_only_when_non_empty() {
        assert!(!display_env_present(None, None));
        assert!(!display_env_present(
            Some(OsStr::new("")),
            Some(OsStr::new(""))
        ));
        assert!(display_env_present(Some(OsStr::new("wayland-1")), None));
        assert!(display_env_present(None, Some(OsStr::new(":0"))));
    }

    fn write_helper_script(path: &Path, body: &str) {
        let staging = path.with_extension(format!("tmp-{}", std::process::id()));
        {
            let mut file = std::fs::File::create(&staging).unwrap();
            std::io::Write::write_all(&mut file, format!("#!/bin/sh\n{body}\n").as_bytes())
                .unwrap();
            file.sync_all().unwrap();
        }
        std::fs::rename(&staging, path).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    fn guard_prompt_context<'a>() -> GuardPromptContext<'a> {
        GuardPromptContext {
            vm_name: "vm-1",
            steam_user: "account",
            reason: "test login",
            invalid_retry: false,
        }
    }

    #[test]
    fn helper_gui_provider_maps_exit_codes_from_env_override() {
        let temp = tempfile::tempdir().unwrap();
        let helper = temp.path().join("helper");

        write_helper_script(&helper, "echo ABCDE");
        assert_helper_env_child("code", &helper);

        write_helper_script(&helper, "exit 10");
        assert_helper_env_child("cancelled", &helper);

        write_helper_script(&helper, "exit 12");
        assert_helper_env_child("unavailable", &helper);

        assert_helper_env_child("unavailable", &temp.path().join("missing-helper"));
    }

    fn assert_helper_env_child(expected: &str, helper: &Path) {
        let output = std::process::Command::new(env::current_exe().unwrap())
            .arg("helper_gui_env_override_child")
            .arg("--ignored")
            .env(GUARD_PROMPT_BIN_ENV, helper)
            .env("STEAMPIPE_EXPECTED_GUARD_OUTCOME", expected)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "spawned by helper_gui_provider_maps_exit_codes_from_env_override with controlled env"]
    async fn helper_gui_env_override_child() {
        let expected = env::var("STEAMPIPE_EXPECTED_GUARD_OUTCOME").unwrap();
        let outcome = GuardProvider::helper_gui(false)
            .request(&guard_prompt_context())
            .await
            .unwrap();
        match expected.as_str() {
            "code" => assert_eq!(outcome, GuardCodeOutcome::Code("ABCDE".to_owned())),
            "cancelled" => assert_eq!(outcome, GuardCodeOutcome::Cancelled),
            "unavailable" => assert_eq!(outcome, GuardCodeOutcome::Unavailable),
            other => panic!("unknown expected outcome: {other}"),
        }
    }

    #[tokio::test]
    async fn helper_gui_provider_serializes_concurrent_requests() {
        let temp = tempfile::tempdir().unwrap();
        let helper = temp.path().join("helper");
        let active = temp.path().join("active");
        let overlap = temp.path().join("overlap");
        write_helper_script(
            &helper,
            &format!(
                r#"if [ -e '{active}' ]; then
    echo overlap > '{overlap}'
    exit 12
fi
touch '{active}'
sleep 0.2
rm -f '{active}'
echo ABCDE"#,
                active = active.display(),
                overlap = overlap.display()
            ),
        );

        let provider = GuardProvider::helper_gui_with_binary(helper);
        let cx1 = GuardPromptContext {
            vm_name: "vm-1",
            steam_user: "account",
            reason: "first",
            invalid_retry: false,
        };
        let cx2 = GuardPromptContext {
            vm_name: "vm-2",
            steam_user: "account",
            reason: "second",
            invalid_retry: false,
        };

        let (first, second) = tokio::join!(provider.request(&cx1), provider.request(&cx2));
        assert_eq!(first.unwrap(), GuardCodeOutcome::Code("ABCDE".to_owned()));
        assert_eq!(second.unwrap(), GuardCodeOutcome::Code("ABCDE".to_owned()));
        assert!(!overlap.exists(), "helper processes overlapped");
    }

    #[tokio::test]
    async fn retry_session_dialog_yields_code_then_confirms_success() {
        let temp = tempfile::tempdir().unwrap();
        let helper = temp.path().join("helper");
        // Persistent dialog: print a code, then wait for the `OK` control line.
        write_helper_script(&helper, "echo CODE1\nIFS= read -r _line\nexit 0");

        let mut session = GuardProvider::helper_gui_with_binary(helper)
            .open_retry_session("vm-1", "account", "test login")
            .await;

        match session.next_code(None).await {
            GuardRetryNext::Code(code) => assert_eq!(code, "CODE1"),
            other => panic!("expected a code, got {:?}", DebugNext(&other)),
        }
        // Closing on success must not hang (bounded wait + kill_on_drop).
        session.confirm_success().await;
    }

    #[tokio::test]
    async fn retry_session_dialog_reprompts_after_rejection() {
        let temp = tempfile::tempdir().unwrap();
        let helper = temp.path().join("helper");
        // First code, then on the `ERR` control line emit a corrected code, then
        // wait for `OK`. This proves the SAME process serves both attempts.
        write_helper_script(
            &helper,
            "echo CODE1\nIFS= read -r first\necho CODE2\nIFS= read -r _second\nexit 0",
        );

        let mut session = GuardProvider::helper_gui_with_binary(helper)
            .open_retry_session("vm-1", "account", "test login")
            .await;

        match session.next_code(None).await {
            GuardRetryNext::Code(code) => assert_eq!(code, "CODE1"),
            other => panic!("expected first code, got {:?}", DebugNext(&other)),
        }
        match session.next_code(Some("that code was rejected")).await {
            GuardRetryNext::Code(code) => assert_eq!(code, "CODE2"),
            other => panic!("expected corrected code, got {:?}", DebugNext(&other)),
        }
        session.confirm_success().await;
    }

    #[tokio::test]
    async fn retry_session_dialog_cancel_maps_to_cancelled() {
        let temp = tempfile::tempdir().unwrap();
        let helper = temp.path().join("helper");
        // Exit 10 == the operator clicked Cancel.
        write_helper_script(&helper, "exit 10");

        let mut session = GuardProvider::helper_gui_with_binary(helper)
            .open_retry_session("vm-1", "account", "test login")
            .await;

        assert!(
            matches!(session.next_code(None).await, GuardRetryNext::Cancelled),
            "cancel exit code must map to Cancelled"
        );
    }

    #[tokio::test]
    async fn retry_session_dialog_display_failure_maps_to_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let helper = temp.path().join("helper");
        // Exit 12 == no usable display; must be Unavailable (→ GuardCodeNeeded),
        // not Cancelled.
        write_helper_script(&helper, "exit 12");

        let mut session = GuardProvider::helper_gui_with_binary(helper)
            .open_retry_session("vm-1", "account", "test login")
            .await;

        assert!(
            matches!(session.next_code(None).await, GuardRetryNext::Unavailable),
            "display-unavailable exit code must map to Unavailable"
        );
    }

    /// `GuardRetryNext` carries a `String`, so give panics a readable form
    /// without deriving `Debug` on the public type.
    struct DebugNext<'a>(&'a GuardRetryNext);
    impl std::fmt::Debug for DebugNext<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self.0 {
                GuardRetryNext::Code(_) => f.write_str("Code(..)"),
                GuardRetryNext::Cancelled => f.write_str("Cancelled"),
                GuardRetryNext::Unavailable => f.write_str("Unavailable"),
            }
        }
    }

    #[test]
    fn sanitize_control_message_strips_newlines_and_caps_length() {
        assert_eq!(
            sanitize_control_message("line one\nline two\r\nthree"),
            "line one line two  three"
        );
        assert_eq!(sanitize_control_message(&"x".repeat(500)).len(), 200);
        // Without injected newlines the message can't break the `ERR <msg>`
        // single-line control protocol.
        assert!(!sanitize_control_message("a\nOK").contains('\n'));
    }

    #[test]
    fn guard_provider_selection_policy_is_deterministic() {
        let cases = [
            (
                "code wins over non-interactive",
                Some(" ABCDE ".to_owned()),
                LoginContext::synthetic(false, false, true),
                false,
                GuardProviderKind::PreSupplied,
            ),
            (
                "non-interactive fails fast",
                None,
                LoginContext::synthetic(true, true, true),
                false,
                GuardProviderKind::FailFast,
            ),
            (
                "mcp-forced non-interactive fails fast even with display and tty",
                None,
                LoginContext::force_non_interactive(),
                false,
                GuardProviderKind::FailFast,
            ),
            (
                "display gui branch selects helper",
                None,
                LoginContext::synthetic(true, true, false),
                false,
                GuardProviderKind::HelperGui,
            ),
            (
                "display gui branch selects helper without tty",
                None,
                LoginContext::synthetic(true, false, false),
                false,
                GuardProviderKind::HelperGui,
            ),
            (
                "no display tty selects stdin",
                None,
                LoginContext::synthetic(false, true, false),
                false,
                GuardProviderKind::Stdin,
            ),
            (
                "no display no tty fails fast",
                None,
                LoginContext::synthetic(false, false, false),
                false,
                GuardProviderKind::FailFast,
            ),
            (
                "no-gui display tty selects stdin",
                None,
                LoginContext::synthetic(true, true, false),
                true,
                GuardProviderKind::Stdin,
            ),
        ];

        for (name, code, context, no_gui, expected) in cases {
            assert_eq!(
                GuardProvider::select(code, context, no_gui).kind(),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn stdin_guard_provider_reports_unavailable_on_eof() {
        let mut input = io::Cursor::new("");
        assert_eq!(
            read_guard_code_from_reader(&mut input).unwrap(),
            GuardCodeOutcome::Unavailable
        );
    }

    #[test]
    fn stdin_guard_provider_trims_codes() {
        let mut input = io::Cursor::new(" ABCDE \n");
        assert_eq!(
            read_guard_code_from_reader(&mut input).unwrap(),
            GuardCodeOutcome::Code("ABCDE".to_owned())
        );
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
        assert!(script.contains("DISPLAY=:0"));
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
        assert!(script.contains("DISPLAY=:0"));
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

    // ── GuardProvider ──────────────────────────────────────────────────

    #[test]
    fn guard_provider_pre_supplied_returns_code_then_unavailable() {
        let provider = GuardProvider::pre_supplied("ABC123".into());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cx = GuardPromptContext {
            vm_name: "vm-1",
            steam_user: "player1",
            reason: "test",
            invalid_retry: false,
        };
        let outcome1 = rt.block_on(provider.request(&cx)).unwrap();
        assert_eq!(outcome1, GuardCodeOutcome::Code("ABC123".into()));

        let outcome2 = rt.block_on(provider.request(&cx)).unwrap();
        assert_eq!(outcome2, GuardCodeOutcome::Unavailable);
    }

    #[test]
    fn guard_provider_fail_fast_returns_unavailable() {
        let provider = GuardProvider::fail_fast();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cx = GuardPromptContext {
            vm_name: "vm-1",
            steam_user: "player1",
            reason: "test",
            invalid_retry: false,
        };
        let outcome = rt.block_on(provider.request(&cx)).unwrap();
        assert_eq!(outcome, GuardCodeOutcome::Unavailable);
    }

    #[test]
    fn guard_provider_select_supplied_code_uses_pre_supplied() {
        let context = LoginContext::synthetic(true, true, false);
        let provider = GuardProvider::select(Some("CODE".into()), context, false);
        assert_eq!(provider.kind(), GuardProviderKind::PreSupplied);
    }

    #[test]
    fn guard_provider_select_non_interactive_uses_fail_fast() {
        let context = LoginContext::synthetic(true, true, true);
        let provider = GuardProvider::select(None, context, false);
        assert_eq!(provider.kind(), GuardProviderKind::FailFast);
    }

    #[test]
    fn guard_provider_select_display_no_gui_uses_stdin() {
        let context = LoginContext::synthetic(true, true, false);
        let provider = GuardProvider::select(None, context, true);
        assert_eq!(provider.kind(), GuardProviderKind::Stdin);
    }

    #[test]
    fn guard_provider_select_no_display_no_tty_uses_fail_fast() {
        let context = LoginContext::synthetic(false, false, false);
        let provider = GuardProvider::select(None, context, false);
        assert_eq!(provider.kind(), GuardProviderKind::FailFast);
    }

    #[test]
    fn guard_provider_select_no_display_with_tty_uses_stdin() {
        let context = LoginContext::synthetic(false, true, false);
        let provider = GuardProvider::select(None, context, false);
        assert_eq!(provider.kind(), GuardProviderKind::Stdin);
    }

    #[test]
    fn guard_provider_select_display_without_no_gui_uses_helper_gui() {
        let context = LoginContext::synthetic(true, false, false);
        let provider = GuardProvider::select(None, context, false);
        assert_eq!(provider.kind(), GuardProviderKind::HelperGui);
    }

    // ── display_env_present ────────────────────────────────────────────

    #[test]
    fn display_env_present_detects_wayland() {
        assert!(display_env_present(Some(std::ffi::OsStr::new("wayland-1")), None));
    }

    #[test]
    fn display_env_present_detects_x11() {
        assert!(display_env_present(None, Some(std::ffi::OsStr::new(":0"))));
    }

    #[test]
    fn display_env_present_prefers_wayland_over_x11() {
        assert!(display_env_present(
            Some(std::ffi::OsStr::new("wayland-1")),
            Some(std::ffi::OsStr::new(":0")),
        ));
    }

    #[test]
    fn display_env_present_rejects_empty_wayland() {
        assert!(!display_env_present(
            Some(std::ffi::OsStr::new("")),
            None,
        ));
    }

    #[test]
    fn display_env_present_returns_false_when_both_missing() {
        assert!(!display_env_present(None, None));
    }

    // ── parse_steam_identity ────────────────────────────────────────────

    #[test]
    fn parse_steam_identity_login_with_persona() {
        let (persona, logged_in) = parse_steam_identity("LOGIN:player1:76561197960287930\n");
        assert_eq!(persona.as_deref(), Some("player1"));
        assert!(logged_in);
    }

    #[test]
    fn parse_steam_identity_login_unknown_persona() {
        let (persona, logged_in) = parse_steam_identity("LOGIN:unknown:76561197960287930\n");
        assert_eq!(persona, None);
        assert!(logged_in);
    }

    #[test]
    fn parse_steam_identity_no_login() {
        let (persona, logged_in) = parse_steam_identity("NO_LOGIN\n");
        assert_eq!(persona, None);
        assert!(!logged_in);
    }

    #[test]
    fn parse_steam_identity_empty_string() {
        let (persona, logged_in) = parse_steam_identity("");
        assert_eq!(persona, None);
        assert!(!logged_in);
    }

    #[test]
    fn parse_steam_identity_malformed() {
        let (persona, logged_in) = parse_steam_identity("LOGIN:\n");
        // "LOGIN:" prefix strips, then split on ':' gives ["\n"]
        // "\n" is not empty and not "unknown", so it becomes the persona
        assert_eq!(persona.as_deref(), Some("\n"));
        assert!(logged_in);
    }

    // ── LoginContext ───────────────────────────────────────────────────

    #[test]
    fn login_context_force_non_interactive() {
        let ctx = LoginContext::force_non_interactive();
        assert!(ctx.non_interactive);
    }

    #[test]
    fn login_context_synthetic_round_trip() {
        let ctx = LoginContext::synthetic(true, false, true);
        assert!(ctx.display);
        assert!(!ctx.stdin_tty);
        assert!(ctx.non_interactive);
    }
}
