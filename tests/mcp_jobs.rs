use std::{os::unix::fs::PermissionsExt, sync::Arc};
use mcp_layer::{job_tools::JobTools, jobs::Jobs};
use rmcp::{ServiceExt, model::CallToolRequestParams};
use serde_json::{Value, json};
use steampipe::mcp::{SteampipeCtx, layered};

async fn call(client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &'static str, args: Value) -> Value
{
    let result = client.call_tool(CallToolRequestParams::new(name)
        .with_arguments(args.as_object().unwrap().clone())).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap()
}

#[tokio::test]
async fn fake_cluster_job_and_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake-cluster");
    std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Job profiles live in the consuming project's steampipe.toml.
    std::fs::write(dir.path().join("steampipe.toml"), r#"
[job_profiles.cluster-1v1-smoke]
runner = "cluster-test"
params = { cluster = "1v1", players = 2, max_runs = 3, timeout = 120 }

[job_profiles.cluster-tournament-8p]
runner = "cluster-test"
params = { cluster = "tournament", players = 8 }
"#).unwrap();
    let jobs = JobTools { jobs: Jobs::new(dir.path().join("jobs"), env!("CARGO_BIN_EXE_cluster-ctl")),
        registry: Arc::default(), profile_paths: vec![] };
    let ctx = SteampipeCtx { project_root: dir.path().to_owned(), cluster_ctl: script };
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        layered(ctx, jobs).serve(server_io).await.unwrap().waiting().await.unwrap();
    });
    let client = ().serve(client_io).await.unwrap();
    let names: Vec<_> = client.list_all_tools().await.unwrap().into_iter()
        .map(|t| t.name.to_string()).collect();
    for removed in ["cluster_test", "cluster_deploy", "nix_run"] {
        assert!(!names.iter().any(|n| n == removed));
    }
    let profiles = call(&client, "job_profiles", json!({})).await;
    assert!(profiles.get("cluster-1v1-smoke").is_some());
    assert!(profiles.get("cluster-tournament-8p").is_some());
    let started = call(&client, "job_start", json!({"spec": {
        "runner": "cluster-test", "params": {"cluster": "1v1", "players": 2}
    }})).await;
    let id = started["id"].as_str().unwrap();
    let watched = call(&client, "job_watch", json!({"id": id, "wait_secs": 10})).await;
    assert_eq!(watched["verdict"]["result"], "pass", "{watched}");
    let logs = call(&client, "job_logs", json!({"id": id})).await.to_string();
    assert!(logs.contains("--cluster=1v1"), "{logs}");
    assert!(logs.contains("--players=2"), "{logs}");
    client.cancel().await.unwrap();
    server.await.unwrap();
}
