use std::time::Duration;

use crate::core::config::VmDef;
use crate::core::config::{IpAddr, SceneAction, WindowTarget};

use super::{SceneBackend, scene_env_prefix, shell_quote};

pub async fn perform_action<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    vm_user: &str,
    scene_window: Option<&WindowTarget>,
    action: &SceneAction,
) -> anyhow::Result<()> {
    if let Some(keys) = action.keys.as_deref() {
        if let Some(target) = scene_window {
            focus_window(backend, vm, vm_user, target).await?;
        }
        run_remote(backend, vm, &keys_command(vm_user, keys)).await?;
        return Ok(());
    }
    if let Some(combo) = action.combo.as_deref() {
        if let Some(target) = scene_window {
            focus_window(backend, vm, vm_user, target).await?;
        }
        run_remote(backend, vm, &combo_command(vm_user, combo)?).await?;
        return Ok(());
    }
    if let Some(app_id) = action.focus.as_deref() {
        run_remote(backend, vm, &focus_app_id_command(vm_user, app_id)).await?;
        return Ok(());
    }
    if let Some(command) = action.shell_command.as_deref() {
        let cmd = format!("{}; {}", scene_env_prefix(vm_user), command);
        run_remote(backend, vm, &cmd).await?;
        return Ok(());
    }
    if let Some(ms) = action.delay_ms {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        return Ok(());
    }
    anyhow::bail!("scene action '{}' has no executable operation", action.step);
}

async fn focus_window<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    vm_user: &str,
    target: &WindowTarget,
) -> anyhow::Result<()> {
    let Some(selector) = selector_for_window(target) else {
        return Ok(());
    };
    let cmd = format!(
        "{}; swaymsg {}",
        scene_env_prefix(vm_user),
        shell_quote(&selector)
    );
    run_remote(backend, vm, &cmd).await
}

fn selector_for_window(target: &WindowTarget) -> Option<String> {
    target
        .app_id
        .as_deref()
        .map(|app_id| format!(r#"[app_id="{}"] focus"#, selector_escape(app_id)))
        .or_else(|| {
            target
                .class
                .as_deref()
                .map(|class| format!(r#"[class="{}"] focus"#, selector_escape(class)))
        })
        .or_else(|| {
            target
                .title
                .as_deref()
                .map(|title| format!(r#"[title="{}"] focus"#, selector_escape(title)))
        })
}

fn focus_app_id_command(vm_user: &str, app_id: &str) -> String {
    let selector = format!(r#"[app_id="{}"] focus"#, selector_escape(app_id));
    format!(
        "{}; swaymsg {}",
        scene_env_prefix(vm_user),
        shell_quote(&selector)
    )
}

fn keys_command(vm_user: &str, keys: &str) -> String {
    let mut cmd = scene_env_prefix(vm_user);
    cmd.push_str("; wtype");
    for key in keys.split_whitespace() {
        cmd.push_str(" -k ");
        cmd.push_str(&shell_quote(key));
    }
    cmd
}

fn combo_command(vm_user: &str, combo: &str) -> anyhow::Result<String> {
    let parts: Vec<&str> = combo
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        anyhow::bail!("combo is empty");
    }
    let mut cmd = scene_env_prefix(vm_user);
    cmd.push_str("; wtype");
    for modifier in &parts[..parts.len().saturating_sub(1)] {
        cmd.push_str(" -M ");
        cmd.push_str(&shell_quote(modifier));
    }
    let key = parts[parts.len() - 1];
    cmd.push_str(" -k ");
    cmd.push_str(&shell_quote(key));
    for modifier in parts[..parts.len().saturating_sub(1)].iter().rev() {
        cmd.push_str(" -m ");
        cmd.push_str(&shell_quote(modifier));
    }
    Ok(cmd)
}

fn selector_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

async fn run_remote<B: SceneBackend>(backend: &B, vm: &VmDef, cmd: &str) -> anyhow::Result<()> {
    let output = backend.run_cmd(&vm.ip, cmd).await;
    if output.success {
        Ok(())
    } else {
        anyhow::bail!(
            "{} command failed\nstdout:\n{}\nstderr:\n{}",
            vm.name,
            output.stdout.trim(),
            output.stderr.trim()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::backend::CmdOutput;
    use crate::core::config::VmName;
    use std::pin::Pin;
    use std::time::Duration;

    #[test]
    fn keys_command_uses_wtype_key_mode() {
        let cmd = keys_command("alice", "Return Escape");
        assert!(cmd.contains("wtype -k 'Return' -k 'Escape'"));
    }

    #[test]
    fn combo_command_presses_and_releases_modifiers() {
        let cmd = combo_command("alice", "Ctrl+Shift+P").unwrap();
        assert!(cmd.contains("wtype -M 'Ctrl' -M 'Shift' -k 'P' -m 'Shift' -m 'Ctrl'"));
    }

    #[test]
    fn selector_for_window_prefers_app_id_then_class_then_title() {
        let by_app = selector_for_window(&WindowTarget {
            app_id: Some("regicide".into()),
            class: Some("ignored".into()),
            title: Some("ignored".into()),
        })
        .unwrap();
        assert!(by_app.contains("app_id"));

        let by_class = selector_for_window(&WindowTarget {
            app_id: None,
            class: Some("steam".into()),
            title: Some("ignored".into()),
        })
        .unwrap();
        assert!(by_class.contains("class"));

        let by_title = selector_for_window(&WindowTarget {
            app_id: None,
            class: None,
            title: Some("Lobby".into()),
        })
        .unwrap();
        assert!(by_title.contains("title"));
    }

    #[test]
    fn selector_for_window_returns_none_with_no_targets() {
        let result = selector_for_window(&WindowTarget {
            app_id: None,
            class: None,
            title: None,
        });
        assert!(result.is_none());
    }

    #[test]
    fn focus_app_id_command_builds_correctly() {
        let cmd = focus_app_id_command("user", "my-app");
        assert!(cmd.contains("swaymsg"));
        assert!(cmd.contains("app_id"));
        assert!(cmd.contains("my-app"));
        assert!(cmd.contains("/tmp/runtime-user"));
    }

    #[test]
    fn combo_command_empty_errors() {
        let err = combo_command("user", "").unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn combo_command_whitespace_only_errors() {
        let err = combo_command("user", "  +  ").unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn selector_escape_handles_backslash_and_quote() {
        let escaped = selector_escape(r#"hello\"world"#);
        assert_eq!(escaped, r#"hello\\\"world"#);
    }

    // ── Mock backend tests ─────────────────────────────────────────────

    struct MockActionBackend {
        canned: std::collections::HashMap<String, CmdOutput>,
        commands: std::sync::Mutex<Vec<String>>,
    }

    impl MockActionBackend {
        fn new() -> Self {
            Self {
                canned: std::collections::HashMap::new(),
                commands: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn add(&mut self, cmd_contains: &str, output: CmdOutput) {
            self.canned.insert(cmd_contains.into(), output);
        }

        fn recorded(&self) -> Vec<String> {
            self.commands.lock().unwrap().clone()
        }
    }

    impl SceneBackend for MockActionBackend {
        fn run_cmd<'a>(
            &'a self,
            ip: &'a str,
            cmd: &'a str,
        ) -> Pin<Box<dyn Future<Output = CmdOutput> + Send + 'a>> {
            let _ = ip;
            self.commands.lock().unwrap().push(cmd.to_string());
            let owned = self
                .canned
                .iter()
                .find_map(|(prefix, output)| {
                    if cmd.contains(prefix.as_str()) {
                        Some(output.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or(CmdOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    success: false,
                });
            Box::pin(std::future::ready(owned))
        }
    }

    fn vm() -> VmDef {
        VmDef {
            name: VmName("vm-1".into()),
            ip: IpAddr("10.0.100.1".into()),
            index: 1,
        }
    }

    fn window() -> WindowTarget {
        WindowTarget {
            app_id: Some("game-app".into()),
            title: None,
            class: None,
        }
    }

    #[tokio::test]
    async fn perform_action_keys_sends_command() {
        let mut backend = MockActionBackend::new();
        backend.add("wtype", CmdOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        });
        backend.add("swaymsg", CmdOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        });
        let action = SceneAction {
            step: "press".into(),
            keys: Some("Return".into()),
            ..Default::default()
        };

        perform_action(&backend, &vm(), "user", Some(&window()), &action)
            .await
            .unwrap();
        let recorded = backend.recorded();
        assert!(recorded.iter().any(|c| c.contains("wtype")), "{recorded:?}");
    }

    #[tokio::test]
    async fn perform_action_combo_sends_command() {
        let mut backend = MockActionBackend::new();
        backend.add("wtype", CmdOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        });
        backend.add("swaymsg", CmdOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        });
        let action = SceneAction {
            step: "combo".into(),
            combo: Some("Ctrl+Shift+P".into()),
            ..Default::default()
        };

        perform_action(&backend, &vm(), "user", Some(&window()), &action)
            .await
            .unwrap();
        let recorded = backend.recorded();
        assert!(recorded.iter().any(|c| c.contains("wtype")), "{recorded:?}");
    }

    #[tokio::test]
    async fn perform_action_focus_sends_command() {
        let mut backend = MockActionBackend::new();
        backend.add("swaymsg", CmdOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        });
        let action = SceneAction {
            step: "focus".into(),
            focus: Some("my-app".into()),
            ..Default::default()
        };

        perform_action(&backend, &vm(), "user", None, &action)
            .await
            .unwrap();
        let recorded = backend.recorded();
        assert!(recorded.iter().any(|c| c.contains("swaymsg")), "{recorded:?}");
        assert!(recorded.iter().any(|c| c.contains("app_id")), "{recorded:?}");
    }

    #[tokio::test]
    async fn perform_action_shell_command_runs() {
        let mut backend = MockActionBackend::new();
        backend.add("echo", CmdOutput {
            stdout: "hello\n".into(),
            stderr: String::new(),
            success: true,
        });
        let action = SceneAction {
            step: "shell".into(),
            shell_command: Some("echo hello".into()),
            ..Default::default()
        };

        perform_action(&backend, &vm(), "user", None, &action)
            .await
            .unwrap();
        let recorded = backend.recorded();
        assert!(recorded.iter().any(|c| c.contains("echo")), "{recorded:?}");
    }

    #[tokio::test]
    async fn perform_action_delay_ms_waits() {
        let backend = MockActionBackend::new();
        let action = SceneAction {
            step: "wait".into(),
            delay_ms: Some(10),
            ..Default::default()
        };
        let start = std::time::Instant::now();

        perform_action(&backend, &vm(), "user", None, &action)
            .await
            .unwrap();

        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(10));
    }

    #[tokio::test]
    async fn perform_action_no_operation_errors() {
        let backend = MockActionBackend::new();
        let action = SceneAction {
            step: "empty".into(),
            ..Default::default()
        };

        let err = perform_action(&backend, &vm(), "user", None, &action)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no executable operation"), "{err}");
    }

    #[tokio::test]
    async fn perform_action_keys_focus_before_command() {
        let mut backend = MockActionBackend::new();
        backend.add("wtype", CmdOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        });
        backend.add("swaymsg", CmdOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        });
        let action = SceneAction {
            step: "press".into(),
            keys: Some("Escape".into()),
            ..Default::default()
        };

        perform_action(&backend, &vm(), "user", Some(&window()), &action)
            .await
            .unwrap();

        let recorded = backend.recorded();
        // First call should be swaymsg (focus), second should be wtype (keys)
        assert!(recorded[0].contains("swaymsg"), "expected focus first: {recorded:?}");
    }
}
