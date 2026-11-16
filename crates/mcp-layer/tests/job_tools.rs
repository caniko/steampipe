use std::sync::Arc;

use mcp_layer::{
    Layered,
    job_tools::JobTools,
    jobs::{JobSpec, Jobs},
    profiles::{Runner, RunnerRegistry},
};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ServerCapabilities, ServerConfig},
};
use serde::Deserialize;
use serde_json::{Value, json};

struct EchoRunner;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    command: String,
    artifact: Option<String>,
    cleanup: Option<Vec<String>>,
}

impl Runner for EchoRunner {
    fn name(&self) -> &str {
        "echo"
    }
    fn build(&self, params: &toml::Table) -> anyhow::Result<JobSpec> {
        let params: Params = toml::Value::Table(params.clone()).try_into()?;
        Ok(JobSpec {
            argv: vec!["sh".into(), "-c".into(), params.command],
            cwd: std::env::current_dir()?,
            timeout_secs: 60,
            grace_secs: 1,
            artifacts: params.artifact.into_iter().collect(),
            cleanup: params.cleanup,
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn artifact_logs_and_cancel_wait() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("artifact.log");
    std::fs::write(&artifact, "artifact-line\n").unwrap();
    let mut registry = RunnerRegistry::default();
    registry.register(EchoRunner).unwrap();
    let tools = JobTools {
        jobs: Jobs::new(
            dir.path().join("jobs"),
            env!("CARGO_BIN_EXE_job-supervisor"),
        ),
        registry: Arc::new(registry),
        profile_paths: vec![],
    };
    let info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        Layered::new(info)
            .with(tools.tool_set())
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let client = ().serve(client_io).await.unwrap();
    let started = call(
        &client,
        "job_start",
        json!({"spec":{"runner":"echo",
        "params":{"command":"sleep 30", "artifact":artifact,
            "cleanup":["sh","-c","sleep 0.5"]}}}),
    )
    .await
    .unwrap();
    let id = &started["id"];
    for selector in [json!(0), json!(artifact)] {
        let logs = call(&client, "job_logs", json!({"id":id,"artifact":selector}))
            .await
            .unwrap();
        assert_eq!(logs["lines"], json!(["artifact-line"]));
    }
    assert!(
        call(
            &client,
            "job_logs",
            json!({"id":id,"artifact":"/etc/passwd"})
        )
        .await
        .is_err()
    );
    let started = std::time::Instant::now();
    let record = call(&client, "job_cancel", json!({"id":id})).await.unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(500));
    assert_eq!(record["state"], "Cancelled");
    assert!(record["end"].is_number());
    client.cancel().await.unwrap();
    server.await.unwrap();
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &'static str,
    args: Value,
) -> Result<Value, String> {
    let request =
        CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone());
    let response = client.call_tool(request).await.map_err(|e| e.to_string())?;
    let text = &response.content[0].as_text().unwrap().text;
    if response.is_error == Some(true) {
        return Err(text.clone());
    }
    Ok(serde_json::from_str(text).unwrap())
}

#[tokio::test]
async fn tools_over_duplex() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.toml");
    std::fs::write(
        &path,
        concat!(
            "[job_profiles.hello]\nrunner='echo'\n",
            "params={command='echo first; echo match; echo last'}\n"
        ),
    )
    .unwrap();
    let mut registry = RunnerRegistry::default();
    registry.register(EchoRunner).unwrap();
    let tools = JobTools {
        jobs: Jobs::new(
            dir.path().join("jobs"),
            env!("CARGO_BIN_EXE_job-supervisor"),
        ),
        registry: Arc::new(registry),
        profile_paths: vec![path.clone()],
    };
    let info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        Layered::new(info)
            .with(tools.tool_set())
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let client = ().serve(client_io).await.unwrap();
    let names: Vec<_> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(
        names,
        [
            "job_cancel",
            "job_list",
            "job_logs",
            "job_profiles",
            "job_start",
            "job_watch"
        ]
    );
    let profiles = call(&client, "job_profiles", json!({})).await.unwrap();
    assert_eq!(profiles["hello"]["runner"], "echo");
    let started = call(&client, "job_start", json!({"profile":"hello"}))
        .await
        .unwrap();
    let id = &started["id"];
    let watch = call(&client, "job_watch", json!({"id":id,"wait_secs":9999}))
        .await
        .unwrap();
    assert_eq!(watch["wait_secs"], 300);
    assert_eq!(watch["verdict"]["result"], "pass");
    let next = call(
        &client,
        "job_watch",
        json!({"id":id,"cursor":watch["next_cursor"]}),
    )
    .await
    .unwrap();
    assert_eq!(next["new_lines"], "");
    let logs = call(
        &client,
        "job_logs",
        json!({"id":id,"tail":2,"pattern":"match"}),
    )
    .await
    .unwrap();
    assert_eq!(logs["lines"], json!(["match"]));
    assert!(
        call(
            &client,
            "job_start",
            json!({"spec":{"runner":"echo",
        "params":{"command":"true", "unknown":1}}})
        )
        .await
        .is_err()
    );
    assert!(
        call(&client, "job_start", json!({"profile":"missing"}))
            .await
            .is_err()
    );
    assert!(
        call(&client, "job_start", json!({"spec":{"runner":"missing"}}))
            .await
            .unwrap_err()
            .contains("unknown runner")
    );
    assert!(call(&client, "job_start", json!({})).await.is_err());
    let sleep = call(
        &client,
        "job_start",
        json!({"spec":{"runner":"echo",
        "params":{"command":"sleep 30"}}}),
    )
    .await
    .unwrap();
    let stopped = call(&client, "job_cancel", json!({"id":sleep["id"]}))
        .await
        .unwrap();
    assert_eq!(stopped["state"], "Cancelled");
    let cancelled = call(
        &client,
        "job_watch",
        json!({"id":sleep["id"],"wait_secs":10}),
    )
    .await
    .unwrap();
    assert_eq!(cancelled["verdict"]["result"], "cancelled");
    assert_eq!(
        call(&client, "job_list", json!({"limit":1}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    std::fs::write(
        &path,
        "[job_profiles.changed]\nrunner='echo'\nparams={command='true'}\n",
    )
    .unwrap();
    assert!(
        call(&client, "job_profiles", json!({}))
            .await
            .unwrap()
            .get("changed")
            .is_some()
    );
    client.cancel().await.unwrap();
    server.await.unwrap();
}
