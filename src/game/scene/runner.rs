use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::core::config::{Scene, VisualConfig, VmDef};

use super::SceneBackend;
use super::action::perform_action;
use super::sync::{
    describe_window_target, prepare_sync, scene_window_ready, sync_description,
    wait_for_scene_window, wait_for_sync,
};

#[derive(Debug)]
pub enum SceneError {
    UnknownScene {
        scene: String,
    },
    SceneConfig {
        scene: String,
        message: String,
    },
    ActionFailed {
        scene: String,
        index: usize,
        step: String,
        reason: String,
    },
    SyncFailed {
        scene: String,
        index: usize,
        step: String,
        predicate: String,
        reason: String,
    },
}

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownScene { scene } => write!(f, "unknown scene '{scene}'"),
            Self::SceneConfig { scene, message } => {
                write!(f, "scene '{scene}' is invalid: {message}")
            }
            Self::ActionFailed {
                scene,
                index,
                step,
                reason,
            } => write!(
                f,
                "scene '{scene}' action #{index} '{step}' failed: {reason}"
            ),
            Self::SyncFailed {
                scene,
                index,
                step,
                predicate,
                reason,
            } => write!(
                f,
                "scene '{scene}' action #{index} '{step}' sync '{predicate}' failed: {reason}"
            ),
        }
    }
}

impl std::error::Error for SceneError {}

pub async fn run_scene_named<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    vm_user: &str,
    visual: &VisualConfig,
    scene_name: &str,
) -> Result<(), SceneError> {
    let mut visiting = BTreeSet::new();
    run_scene_inner(backend, vm, vm_user, visual, scene_name, &mut visiting).await
}

fn run_scene_inner<'a, B: SceneBackend + 'a>(
    backend: &'a B,
    vm: &'a VmDef,
    vm_user: &'a str,
    visual: &'a VisualConfig,
    scene_name: &'a str,
    visiting: &'a mut BTreeSet<String>,
) -> Pin<Box<dyn Future<Output = Result<(), SceneError>> + Send + 'a>> {
    Box::pin(async move {
        let scene = visual
            .scene(scene_name)
            .ok_or_else(|| SceneError::UnknownScene {
                scene: scene_name.to_string(),
            })?;

        if !visiting.insert(scene_name.to_string()) {
            return Err(SceneError::SceneConfig {
                scene: scene_name.to_string(),
                message: "precondition cycle detected".into(),
            });
        }

        if let Some(precondition) = scene.precondition.as_deref() {
            let pre_scene = visual
                .scene(precondition)
                .ok_or_else(|| SceneError::UnknownScene {
                    scene: precondition.to_string(),
                })?;
            let satisfied = scene_window_ready(backend, vm, pre_scene)
                .await
                .map_err(|error| SceneError::SceneConfig {
                    scene: precondition.to_string(),
                    message: error.to_string(),
                })?;
            if satisfied != Some(true) {
                run_scene_inner(backend, vm, vm_user, visual, precondition, visiting).await?;
            }
        }

        if scene.action.is_empty() {
            wait_for_scene(scene, backend, vm).await?;
            visiting.remove(scene_name);
            return Ok(());
        }

        let timeout = Duration::from_secs(scene.timeout_seconds());
        for (index, action) in scene.action.iter().enumerate() {
            let sync = action
                .sync
                .as_ref()
                .ok_or_else(|| SceneError::SceneConfig {
                    scene: scene.name.clone(),
                    message: format!("action '{}' is missing sync", action.step),
                })?;
            let prepared = prepare_sync(backend, vm, sync).await.map_err(|error| {
                SceneError::ActionFailed {
                    scene: scene.name.clone(),
                    index,
                    step: action.step.clone(),
                    reason: error.to_string(),
                }
            })?;
            perform_action(backend, vm, vm_user, scene.window.as_ref(), action)
                .await
                .map_err(|error| SceneError::ActionFailed {
                    scene: scene.name.clone(),
                    index,
                    step: action.step.clone(),
                    reason: error.to_string(),
                })?;
            wait_for_sync(backend, vm, sync, &prepared, timeout)
                .await
                .map_err(|error| SceneError::SyncFailed {
                    scene: scene.name.clone(),
                    index,
                    step: action.step.clone(),
                    predicate: sync_description(sync),
                    reason: error.to_string(),
                })?;
        }

        visiting.remove(scene_name);
        Ok(())
    })
}

async fn wait_for_scene<B: SceneBackend>(
    scene: &Scene,
    backend: &B,
    vm: &VmDef,
) -> Result<(), SceneError> {
    wait_for_scene_window(
        backend,
        vm,
        scene,
        Duration::from_secs(scene.timeout_seconds()),
    )
    .await
    .map_err(|error| {
        let predicate = scene
            .window
            .as_ref()
            .map(describe_window_target)
            .unwrap_or_else(|| "scene window readiness".into());
        SceneError::SyncFailed {
            scene: scene.name.clone(),
            index: 0,
            step: "scene-ready".into(),
            predicate,
            reason: error.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use crate::core::backend::CmdOutput;
    use crate::core::config::{SceneAction, SceneSync, WindowTarget};

    use super::*;

    #[derive(Clone, Default)]
    struct MockBackend {
        expected: Arc<Mutex<VecDeque<(String, CmdOutput)>>>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl MockBackend {
        fn push(&self, needle: &str, stdout: &str, success: bool) {
            self.expected.lock().unwrap().push_back((
                needle.into(),
                CmdOutput {
                    stdout: stdout.into(),
                    stderr: String::new(),
                    success,
                },
            ));
        }

        fn commands(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl SceneBackend for MockBackend {
        fn run_cmd<'a>(
            &'a self,
            _ip: &'a str,
            cmd: &'a str,
        ) -> Pin<Box<dyn Future<Output = CmdOutput> + Send + 'a>> {
            let expected = self.expected.clone();
            let seen = self.seen.clone();
            let cmd_string = cmd.to_string();
            Box::pin(async move {
                seen.lock().unwrap().push(cmd_string.clone());
                let (needle, output) = expected
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected command");
                assert!(
                    cmd_string.contains(&needle),
                    "expected command containing {needle:?}, got {cmd_string:?}"
                );
                output
            })
        }
    }

    fn vm() -> VmDef {
        VmDef {
            name: crate::core::config::VmName("vm-1".into()),
            ip: crate::core::config::IpAddr("10.0.0.1".into()),
            index: 1,
        }
    }

    fn visual_with_scene(scene: Scene) -> VisualConfig {
        VisualConfig {
            scene: vec![scene],
            ..Default::default()
        }
    }

    fn visible_tree(app_id: &str) -> String {
        format!(
            r#"{{
                "app_id": "{app_id}",
                "visible": true,
                "name": "Game",
                "rect": {{ "width": 1280, "height": 720 }},
                "nodes": [],
                "floating_nodes": []
            }}"#
        )
    }

    #[tokio::test]
    async fn run_scene_happy_path_executes_action_and_sync() {
        let backend = MockBackend::default();
        backend.push("focus", "", true);
        backend.push("wtype -k 'Return'", "", true);
        backend.push("swaymsg -t get_tree", &visible_tree("regicide"), true);

        let scene = Scene {
            name: "main-menu".into(),
            window: Some(WindowTarget {
                app_id: Some("regicide".into()),
                title: None,
                class: None,
            }),
            action: vec![SceneAction {
                step: "press return".into(),
                keys: Some("Return".into()),
                sync: Some(SceneSync {
                    window_visible: Some("regicide".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        run_scene_named(
            &backend,
            &vm(),
            "alice",
            &visual_with_scene(scene),
            "main-menu",
        )
        .await
        .unwrap();

        let commands = backend.commands();
        assert!(commands.iter().any(|cmd| cmd.contains("wtype -k 'Return'")));
    }

    #[tokio::test]
    async fn run_scene_reports_sync_failure_with_step_and_predicate() {
        let backend = MockBackend::default();
        backend.push("test -e", "", false);

        let scene = Scene {
            name: "turn-1".into(),
            timeout_seconds: Some(0),
            action: vec![SceneAction {
                step: "wait for marker".into(),
                delay_ms: Some(0),
                sync: Some(SceneSync {
                    file_exists: Some("/tmp/missing".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let error = run_scene_named(
            &backend,
            &vm(),
            "alice",
            &visual_with_scene(scene),
            "turn-1",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("wait for marker"));
        assert!(error.contains("file_exists(/tmp/missing)"));
    }

    #[tokio::test]
    async fn precondition_scene_runs_when_not_already_ready() {
        let backend = MockBackend::default();
        backend.push(
            "swaymsg -t get_tree",
            r#"{"nodes":[],"floating_nodes":[]}"#,
            true,
        );
        backend.push("focus", "", true);
        backend.push("wtype -k 'Return'", "", true);
        backend.push("swaymsg -t get_tree", &visible_tree("regicide"), true);
        backend.push("focus", "", true);
        backend.push("wtype -k 'Escape'", "", true);
        backend.push("swaymsg -t get_tree", &visible_tree("regicide"), true);

        let pre = Scene {
            name: "main-menu".into(),
            window: Some(WindowTarget {
                app_id: Some("regicide".into()),
                title: None,
                class: None,
            }),
            action: vec![SceneAction {
                step: "open".into(),
                keys: Some("Return".into()),
                sync: Some(SceneSync {
                    window_visible: Some("regicide".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let next = Scene {
            name: "submenu".into(),
            precondition: Some("main-menu".into()),
            window: Some(WindowTarget {
                app_id: Some("regicide".into()),
                title: None,
                class: None,
            }),
            action: vec![SceneAction {
                step: "back".into(),
                keys: Some("Escape".into()),
                sync: Some(SceneSync {
                    window_visible: Some("regicide".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let visual = VisualConfig {
            scene: vec![pre, next],
            ..Default::default()
        };

        run_scene_named(&backend, &vm(), "alice", &visual, "submenu")
            .await
            .unwrap();

        let commands = backend.commands().join("\n");
        assert!(commands.contains("wtype -k 'Return'"));
        assert!(commands.contains("wtype -k 'Escape'"));
    }
}
