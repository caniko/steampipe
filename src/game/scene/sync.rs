use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::core::config::{LogLineMatches, Scene, SceneSync, VmDef, VmName, IpAddr, WindowTarget};

use super::{SceneBackend, shell_quote};

const SCENE_SYNC_INITIAL_DELAY: Duration = Duration::from_millis(250);
const SCENE_SYNC_MAX_DELAY: Duration = Duration::from_secs(4);

#[derive(Debug, Clone)]
pub struct PreparedSync {
    started_at: Instant,
    log_offset: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PollOutcome {
    pub matched: bool,
    pub detail: Option<String>,
}

pub async fn prepare_sync<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    sync: &SceneSync,
) -> anyhow::Result<PreparedSync> {
    let log_offset = match sync.log_line_matches.as_ref() {
        Some(log) => Some(remote_file_size(backend, vm, &log.path).await?),
        None => None,
    };
    Ok(PreparedSync {
        started_at: Instant::now(),
        log_offset,
    })
}

pub async fn wait_for_sync<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    sync: &SceneSync,
    prepared: &PreparedSync,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut delay = SCENE_SYNC_INITIAL_DELAY;
    let mut first_match_at = None;
    let min_age = Duration::from_millis(sync.min_age_ms.unwrap_or(0));

    loop {
        let outcome = poll_sync(backend, vm, sync, prepared).await?;
        if outcome.matched {
            let matched_at = *first_match_at.get_or_insert_with(Instant::now);
            if matched_at.elapsed() >= min_age {
                return Ok(());
            }
        } else {
            first_match_at = None;
        }
        let last_detail = outcome.detail;
        let now = Instant::now();
        if now >= deadline {
            let suffix = last_detail
                .as_deref()
                .map(|detail| format!("; last observation: {detail}"))
                .unwrap_or_default();
            anyhow::bail!("timed out after {}s{}", timeout.as_secs(), suffix);
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(delay.min(remaining)).await;
        delay = (delay * 2).min(SCENE_SYNC_MAX_DELAY);
    }
}

pub async fn wait_for_scene_window<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    scene: &Scene,
    timeout: Duration,
) -> anyhow::Result<()> {
    let Some(target) = scene_window_target(scene)? else {
        anyhow::bail!(
            "scene '{}' has no scene-level readiness target (window/wait_for)",
            scene.name
        );
    };
    let deadline = Instant::now() + timeout;
    let min_age = Duration::from_millis(
        scene
            .wait_for
            .as_ref()
            .and_then(|wait_for| wait_for.min_age_ms)
            .unwrap_or(0),
    );
    let mut first_match_at = None;
    let mut delay = SCENE_SYNC_INITIAL_DELAY;
    loop {
        let matched = poll_window_target(backend, vm, &target).await?;
        if matched {
            let matched_at = *first_match_at.get_or_insert_with(Instant::now);
            if matched_at.elapsed() >= min_age {
                return Ok(());
            }
        } else {
            first_match_at = None;
        }
        let now = Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "timed out after {}s waiting for {}",
                timeout.as_secs(),
                describe_window_target(&target)
            );
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(delay.min(remaining)).await;
        delay = (delay * 2).min(SCENE_SYNC_MAX_DELAY);
    }
}

pub async fn scene_window_ready<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    scene: &Scene,
) -> anyhow::Result<Option<bool>> {
    let Some(target) = scene_window_target(scene)? else {
        return Ok(None);
    };
    Ok(Some(poll_window_target(backend, vm, &target).await?))
}

pub fn sync_description(sync: &SceneSync) -> String {
    if let Some(app_id) = sync.window_visible.as_deref() {
        let age = sync
            .min_age_ms
            .map(|ms| format!(", min_age_ms={ms}"))
            .unwrap_or_default();
        return format!("window_visible(app_id={app_id}{age})");
    }
    if let Some(name) = sync.sway_node_named.as_deref() {
        return format!("sway_node_named({name})");
    }
    if let Some(path) = sync.file_exists.as_deref() {
        return format!("file_exists({path})");
    }
    if let Some(log) = sync.log_line_matches.as_ref() {
        return format!("log_line_matches(path={}, regex={})", log.path, log.regex);
    }
    if let Some(ms) = sync.fixed_ms {
        return format!("fixed_ms({ms})");
    }
    "unknown".into()
}

async fn poll_sync<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    sync: &SceneSync,
    prepared: &PreparedSync,
) -> anyhow::Result<PollOutcome> {
    if let Some(app_id) = sync.window_visible.as_deref() {
        let tree = fetch_tree(backend, vm).await?;
        let matched = any_matching_node(
            &tree,
            &WindowTarget {
                app_id: Some(app_id.to_string()),
                title: None,
                class: None,
            },
        );
        return Ok(PollOutcome {
            matched,
            detail: Some(format!("window app_id={app_id} visible={matched}")),
        });
    }
    if let Some(name) = sync.sway_node_named.as_deref() {
        let tree = fetch_tree(backend, vm).await?;
        let matched = any_node_named(&tree, name);
        return Ok(PollOutcome {
            matched,
            detail: Some(format!("node name={name} visible={matched}")),
        });
    }
    if let Some(path) = sync.file_exists.as_deref() {
        let cmd = format!("test -e {}", shell_quote(path));
        let output = backend.run_cmd(&vm.ip, &cmd).await;
        return Ok(PollOutcome {
            matched: output.success,
            detail: Some(format!("file {path} exists={}", output.success)),
        });
    }
    if let Some(log) = sync.log_line_matches.as_ref() {
        let offset = prepared.log_offset.unwrap_or(0).saturating_add(1);
        let matched = poll_log_line(backend, vm, log, offset).await?;
        return Ok(PollOutcome {
            matched,
            detail: Some(format!("log {} matched={matched}", log.path)),
        });
    }
    if let Some(ms) = sync.fixed_ms {
        let matched = prepared.started_at.elapsed() >= Duration::from_millis(ms);
        return Ok(PollOutcome {
            matched,
            detail: Some(format!(
                "elapsed={}ms",
                prepared.started_at.elapsed().as_millis()
            )),
        });
    }
    anyhow::bail!("scene sync predicate is empty")
}

fn scene_window_target(scene: &Scene) -> anyhow::Result<Option<WindowTarget>> {
    let wait_for = scene.wait_for.as_ref();
    if wait_for.and_then(|wait| wait.window_visible) == Some(true) || scene.window.is_some() {
        let target = scene.window.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "scene '{}' requests window readiness but does not define a window target",
                scene.name
            )
        })?;
        return Ok(Some(target));
    }
    Ok(None)
}

pub fn describe_window_target(target: &WindowTarget) -> String {
    target
        .app_id
        .as_deref()
        .map(|app_id| format!("window(app_id={app_id})"))
        .or_else(|| {
            target
                .class
                .as_deref()
                .map(|class| format!("window(class={class})"))
        })
        .or_else(|| {
            target
                .title
                .as_deref()
                .map(|title| format!("window(title={title})"))
        })
        .unwrap_or_else(|| "window(<unspecified>)".into())
}

async fn poll_window_target<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    target: &WindowTarget,
) -> anyhow::Result<bool> {
    let tree = fetch_tree(backend, vm).await?;
    Ok(any_matching_node(&tree, target))
}

async fn poll_log_line<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    log: &LogLineMatches,
    offset: u64,
) -> anyhow::Result<bool> {
    let cmd = format!(
        "if [ -f {path} ]; then tail -c +{offset} {path} | grep -E -q -- {regex}; else exit 1; fi",
        path = shell_quote(&log.path),
        regex = shell_quote(&log.regex),
    );
    Ok(backend.run_cmd(&vm.ip, &cmd).await.success)
}

async fn remote_file_size<B: SceneBackend>(
    backend: &B,
    vm: &VmDef,
    path: &str,
) -> anyhow::Result<u64> {
    let cmd = format!("wc -c < {} 2>/dev/null || echo 0", shell_quote(path));
    let output = backend.run_cmd(&vm.ip, &cmd).await;
    if !output.success && output.stdout.trim().is_empty() {
        anyhow::bail!("failed to read file size for {}", path);
    }
    Ok(output.stdout.trim().parse::<u64>().unwrap_or(0))
}

async fn fetch_tree<B: SceneBackend>(backend: &B, vm: &VmDef) -> anyhow::Result<SwayNode> {
    let output = backend.run_cmd(&vm.ip, "swaymsg -t get_tree").await;
    if !output.success {
        anyhow::bail!("swaymsg get_tree failed: {}", output.stderr.trim());
    }
    Ok(serde_json::from_str::<SwayNode>(&output.stdout)?)
}

fn any_matching_node(node: &SwayNode, target: &WindowTarget) -> bool {
    node_matches(node, target)
        || node
            .nodes
            .iter()
            .chain(node.floating_nodes.iter())
            .any(|child| any_matching_node(child, target))
}

fn any_node_named(node: &SwayNode, name: &str) -> bool {
    let matched = node.visible.unwrap_or(false)
        && node.rect.width > 0
        && node.rect.height > 0
        && node.name.as_deref() == Some(name);
    matched
        || node
            .nodes
            .iter()
            .chain(node.floating_nodes.iter())
            .any(|child| any_node_named(child, name))
}

fn node_matches(node: &SwayNode, target: &WindowTarget) -> bool {
    if !node.visible.unwrap_or(false) || node.rect.width <= 0 || node.rect.height <= 0 {
        return false;
    }
    let app_id_match = target
        .app_id
        .as_deref()
        .is_some_and(|app_id| node.app_id.as_deref() == Some(app_id));
    let class_match = target.class.as_deref().is_some_and(|class| {
        node.window_properties
            .as_ref()
            .and_then(|props| props.class.as_deref())
            == Some(class)
    });
    let title_match = target
        .title
        .as_deref()
        .is_some_and(|title| node.name.as_deref() == Some(title));
    app_id_match || class_match || title_match
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SwayNode {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    visible: Option<bool>,
    #[serde(default)]
    rect: SwayRect,
    #[serde(default)]
    window_properties: Option<SwayWindowProperties>,
    #[serde(default)]
    nodes: Vec<SwayNode>,
    #[serde(default)]
    floating_nodes: Vec<SwayNode>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SwayRect {
    #[serde(default)]
    width: i32,
    #[serde(default)]
    height: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SwayWindowProperties {
    #[serde(default)]
    class: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::backend::CmdOutput;
    use crate::core::config::WaitFor;
    use std::pin::Pin;

    fn tree(json: &str) -> SwayNode {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn node_match_finds_visible_app_id_recursively() {
        let tree = tree(
            r#"{
                "nodes": [
                    {
                        "visible": false,
                        "rect": { "width": 0, "height": 0 },
                        "nodes": [],
                        "floating_nodes": []
                    },
                    {
                        "nodes": [
                            {
                                "app_id": "regicide",
                                "visible": true,
                                "name": "Game",
                                "rect": { "width": 1280, "height": 720 },
                                "nodes": [],
                                "floating_nodes": []
                            }
                        ],
                        "floating_nodes": []
                    }
                ],
                "floating_nodes": []
            }"#,
        );
        assert!(any_matching_node(
            &tree,
            &WindowTarget {
                app_id: Some("regicide".into()),
                title: None,
                class: None
            }
        ));
    }

    #[test]
    fn node_match_rejects_hidden_zero_rect_window() {
        let tree = tree(
            r#"{
                "app_id": "regicide",
                "visible": false,
                "rect": { "width": 0, "height": 0 },
                "nodes": [],
                "floating_nodes": []
            }"#,
        );
        assert!(!any_matching_node(
            &tree,
            &WindowTarget {
                app_id: Some("regicide".into()),
                title: None,
                class: None
            }
        ));
    }

    #[test]
    fn node_match_finds_xwayland_class() {
        let tree = tree(
            r#"{
                "visible": true,
                "name": "Steam",
                "rect": { "width": 900, "height": 700 },
                "window_properties": { "class": "steam" },
                "nodes": [],
                "floating_nodes": []
            }"#,
        );
        assert!(any_matching_node(
            &tree,
            &WindowTarget {
                app_id: None,
                title: None,
                class: Some("steam".into())
            }
        ));
    }

    #[test]
    fn any_node_named_requires_visibility() {
        let tree = tree(
            r#"{
                "name": "Lobby",
                "visible": true,
                "rect": { "width": 200, "height": 100 },
                "nodes": [],
                "floating_nodes": []
            }"#,
        );
        assert!(any_node_named(&tree, "Lobby"));
        assert!(!any_node_named(&tree, "Missing"));
    }

    #[test]
    fn sync_description_includes_min_age() {
        let description = sync_description(&SceneSync {
            window_visible: Some("regicide".into()),
            min_age_ms: Some(500),
            ..Default::default()
        });
        assert!(description.contains("min_age_ms=500"));
    }

    #[test]
    fn scene_window_target_uses_wait_for_and_window() {
        let scene = Scene {
            name: "main-menu".into(),
            window: Some(WindowTarget {
                app_id: Some("regicide".into()),
                title: None,
                class: None,
            }),
            wait_for: Some(WaitFor {
                window_visible: Some(true),
                min_age_ms: Some(250),
            }),
            ..Default::default()
        };
        let target = scene_window_target(&scene).unwrap().unwrap();
        assert_eq!(target.app_id.as_deref(), Some("regicide"));
    }

    // ── Mock backend tests ─────────────────────────────────────────────

    /// A SceneBackend that returns canned responses keyed by command prefix.
    /// Records invocations for inspection.
    struct MockBackend {
        canned: std::collections::HashMap<String, CmdOutput>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl MockBackend {
        fn new(canned: std::collections::HashMap<String, CmdOutput>) -> Self {
            Self {
                canned,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn record(&self, ip: &str, cmd: &str) {
            self.calls.lock().unwrap().push(format!("{ip}:{cmd}"));
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn last_call(&self) -> String {
            self.calls.lock().unwrap().last().cloned().unwrap_or_default()
        }
    }

    impl SceneBackend for MockBackend {
        fn run_cmd<'a>(
            &'a self,
            ip: &'a str,
            cmd: &'a str,
        ) -> Pin<Box<dyn Future<Output = CmdOutput> + Send + 'a>> {
            self.record(ip, cmd);
            let owned = self
                .canned
                .iter()
                .find_map(|(prefix, output)| {
                    if cmd.starts_with(prefix.as_str()) {
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

    fn sway_tree_json(app_id: &str, visible: bool) -> String {
        format!(
            r#"{{
                "app_id": "{app_id}",
                "visible": {visible},
                "name": "Window",
                "rect": {{"width": 1280, "height": 720}},
                "nodes": [],
                "floating_nodes": []
            }}"#
        )
    }

    #[tokio::test]
    async fn wait_for_sync_file_exists_immediate() {
        let mut canned = std::collections::HashMap::new();
        canned.insert(
            "test -e".into(),
            CmdOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            },
        );
        let backend = MockBackend::new(canned);
        let sync = SceneSync {
            file_exists: Some("/tmp/marker".into()),
            ..Default::default()
        };
        let prepared = PreparedSync {
            started_at: std::time::Instant::now(),
            log_offset: None,
        };

        let result = wait_for_sync(&backend, &vm(), &sync, &prepared, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "{:?}", result);
        assert!(backend.call_count() >= 1);
    }

    #[tokio::test]
    async fn wait_for_sync_file_exists_timeout() {
        let backend = MockBackend::new(std::collections::HashMap::new());
        let sync = SceneSync {
            file_exists: Some("/tmp/never-appears".into()),
            ..Default::default()
        };
        let prepared = PreparedSync {
            started_at: std::time::Instant::now(),
            log_offset: None,
        };

        let result = wait_for_sync(&backend, &vm(), &sync, &prepared, Duration::from_millis(100)).await;
        assert!(result.is_err(), "expected timeout error");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("timed out"), "{err}");
    }

    #[tokio::test]
    async fn wait_for_sync_fixed_ms_immediate() {
        let backend = MockBackend::new(std::collections::HashMap::new());
        let sync = SceneSync {
            fixed_ms: Some(1),
            ..Default::default()
        };
        let prepared = PreparedSync {
            started_at: std::time::Instant::now(),
            log_offset: None,
        };

        let result = wait_for_sync(&backend, &vm(), &sync, &prepared, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "{:?}", result);
    }

    #[tokio::test]
    async fn wait_for_sync_fixed_ms_timeout() {
        let backend = MockBackend::new(std::collections::HashMap::new());
        let sync = SceneSync {
            fixed_ms: Some(10_000), // longer than timeout
            ..Default::default()
        };
        let prepared = PreparedSync {
            started_at: std::time::Instant::now(),
            log_offset: None,
        };

        let result = wait_for_sync(&backend, &vm(), &sync, &prepared, Duration::from_millis(50)).await;
        assert!(result.is_err(), "expected timeout error");
    }

    #[tokio::test]
    async fn wait_for_sync_window_visible_immediate() {
        let mut canned = std::collections::HashMap::new();
        canned.insert(
            "swaymsg".into(),
            CmdOutput {
                stdout: sway_tree_json("my-app", true),
                stderr: String::new(),
                success: true,
            },
        );
        let backend = MockBackend::new(canned);
        let sync = SceneSync {
            window_visible: Some("my-app".into()),
            min_age_ms: Some(0),
            ..Default::default()
        };
        let prepared = PreparedSync {
            started_at: std::time::Instant::now(),
            log_offset: None,
        };

        let result = wait_for_sync(&backend, &vm(), &sync, &prepared, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "{:?}", result);
        assert!(backend.call_count() >= 1);
    }

    #[tokio::test]
    async fn wait_for_sync_window_visible_not_found_timeout() {
        let mut canned = std::collections::HashMap::new();
        canned.insert(
            "swaymsg".into(),
            CmdOutput {
                stdout: sway_tree_json("other-app", true),
                stderr: String::new(),
                success: true,
            },
        );
        let backend = MockBackend::new(canned);
        let sync = SceneSync {
            window_visible: Some("missing-app".into()),
            ..Default::default()
        };
        let prepared = PreparedSync {
            started_at: std::time::Instant::now(),
            log_offset: None,
        };

        let result = wait_for_sync(&backend, &vm(), &sync, &prepared, Duration::from_millis(100)).await;
        assert!(result.is_err(), "expected timeout error");
    }

    #[tokio::test]
    async fn wait_for_sync_sway_node_named_found() {
        let mut canned = std::collections::HashMap::new();
        canned.insert(
            "swaymsg".into(),
            CmdOutput {
                stdout: sway_tree_json("my-app", true),
                stderr: String::new(),
                success: true,
            },
        );
        let backend = MockBackend::new(canned);
        let sync = SceneSync {
            sway_node_named: Some("Window".into()),
            ..Default::default()
        };
        let prepared = PreparedSync {
            started_at: std::time::Instant::now(),
            log_offset: None,
        };

        let result = wait_for_sync(&backend, &vm(), &sync, &prepared, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "{:?}", result);
    }

    #[tokio::test]
    async fn wait_for_sync_empty_predicate_errors() {
        let backend = MockBackend::new(std::collections::HashMap::new());
        let sync = SceneSync::default();
        let prepared = PreparedSync {
            started_at: std::time::Instant::now(),
            log_offset: None,
        };

        let result = wait_for_sync(&backend, &vm(), &sync, &prepared, Duration::from_secs(5)).await;
        assert!(result.is_err(), "expected error for empty predicate");
    }

    #[tokio::test]
    async fn prepare_sync_with_log_line_matches_records_offset() {
        let mut canned = std::collections::HashMap::new();
        canned.insert(
            "wc -c".into(),
            CmdOutput {
                stdout: "12345\n".into(),
                stderr: String::new(),
                success: true,
            },
        );
        let backend = MockBackend::new(canned);
        let sync = SceneSync {
            log_line_matches: Some(LogLineMatches {
                path: "/var/log/game.log".into(),
                regex: "ERROR".into(),
            }),
            ..Default::default()
        };

        let prepared = prepare_sync(&backend, &vm(), &sync).await.unwrap();
        assert_eq!(prepared.log_offset, Some(12345));
    }

    #[tokio::test]
    async fn wait_for_window_target_immediate_match() {
        let mut canned = std::collections::HashMap::new();
        canned.insert(
            "swaymsg".into(),
            CmdOutput {
                stdout: sway_tree_json("game-app", true),
                stderr: String::new(),
                success: true,
            },
        );
        let backend = MockBackend::new(canned);
        let target = WindowTarget {
            app_id: Some("game-app".into()),
            title: None,
            class: None,
        };
        let matched = poll_window_target(&backend, &vm(), &target).await.unwrap();
        assert!(matched);
    }

    #[tokio::test]
    async fn wait_for_window_target_no_match() {
        let mut canned = std::collections::HashMap::new();
        canned.insert(
            "swaymsg".into(),
            CmdOutput {
                stdout: sway_tree_json("game-app", true),
                stderr: String::new(),
                success: true,
            },
        );
        let backend = MockBackend::new(canned);
        let target = WindowTarget {
            app_id: Some("other-app".into()),
            title: None,
            class: None,
        };
        let matched = poll_window_target(&backend, &vm(), &target).await.unwrap();
        assert!(!matched);
    }
}
