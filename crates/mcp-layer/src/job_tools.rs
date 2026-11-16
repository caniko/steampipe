//! Composable MCP tools for starting and monitoring durable jobs.

use std::{collections::VecDeque, io::{BufRead, BufReader}, path::PathBuf, sync::Arc};

use regex::Regex;
use rmcp::{ErrorData, handler::server::wrapper::Parameters, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Routed, jobs::{JobState, Jobs}, profiles::{Profile, ProfileOrInline, Profiles,
    RunnerRegistry}};

pub struct JobTools {
    pub jobs: Jobs,
    pub registry: Arc<RunnerRegistry>,
    pub profile_paths: Vec<PathBuf>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartInput {
    profile: Option<String>,
    spec: Option<Value>,
    overrides: Option<serde_json::Map<String, Value>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchInput { id: String, wait_secs: Option<u64>, cursor: Option<u64> }

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogsInput { id: String, tail: Option<usize>, pattern: Option<String>,
    artifact: Option<Value> }

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdInput { id: String }

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInput { limit: Option<usize> }

fn error(err: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_params(err.to_string(), None)
}

#[tool_router]
impl JobTools {
    pub fn tool_set(self) -> Routed<Self> {
        Routed { state: self, router: Self::tool_router() }
    }

    #[tool(description = "Start a job from a named profile or inline spec")]
    async fn job_start(&self, Parameters(input): Parameters<StartInput>)
        -> Result<String, ErrorData>
    {
        let profiles = Profiles::load(&self.profile_paths).map_err(|e| error(format!("{e:#}")))?;
        let inline: Option<Profile> = input.spec.map(serde_json::from_value).transpose()
            .map_err(error)?;
        if input.profile.is_some() == inline.is_some() {
            return Err(error("exactly one of profile or spec is required"));
        }
        let overrides = serde_json::from_value::<toml::Table>(
            Value::Object(input.overrides.unwrap_or_default())).map_err(error)?;
        let (profile, stop) = if let Some(name) = &input.profile {
            let stop = profiles.list().get(name).ok_or_else(|| error(format!(
                "unknown profile: {name}")))?.stop_on_failure;
            (ProfileOrInline::Name(name), stop)
        } else {
            let profile = inline.as_ref().expect("checked above");
            (ProfileOrInline::Inline(profile), profile.stop_on_failure)
        };
        let specs = profiles.resolve(profile, overrides, &self.registry)
            .map_err(|e| error(format!("{e:#}")))?;
        let jobs = self.jobs.clone();
        let id = tokio::task::spawn_blocking(move || jobs.start_sequence(specs, stop)).await
            .map_err(error)?.map_err(|e| error(format!("{e:#}")))?;
        Ok(json!({"id": id}).to_string())
    }

    #[tool(description = "List available profiles, reloading configuration files")]
    async fn job_profiles(&self) -> Result<String, ErrorData> {
        let profiles = Profiles::load(&self.profile_paths).map_err(|e| error(format!("{e:#}")))?;
        serde_json::to_string(profiles.list()).map_err(error)
    }

    #[tool(description = "Wait for a job and read output from a byte cursor")]
    async fn job_watch(&self, Parameters(input): Parameters<WatchInput>)
        -> Result<String, ErrorData>
    {
        let wait = input.wait_secs.unwrap_or(30).clamp(1, 300);
        let jobs = self.jobs.clone();
        let (record, lines, cursor) = tokio::task::spawn_blocking(move || {
            jobs.watch(&input.id, wait, input.cursor.unwrap_or(0))
        }).await.map_err(error)?.map_err(|e| error(format!("{e:#}")))?;
        let result = match record.state {
            JobState::Pending | JobState::Running => None,
            JobState::Passed => Some("pass"), JobState::Failed => Some("fail"),
            JobState::TimedOut => Some("timeout"), JobState::Cancelled => Some("cancelled"),
            JobState::Lost => Some("lost"),
        };
        let mut response = json!({"state": record.state, "new_lines": lines,
            "next_cursor": cursor, "wait_secs": wait});
        if let Some(result) = result {
            response["verdict"] = json!({"result": result, "exit_code": record.exit_code,
                "marker_hits": record.marker_hits});
        }
        Ok(response.to_string())
    }

    #[tool(description = "Read recent job log lines, optionally filtered by regex")]
    async fn job_logs(&self, Parameters(input): Parameters<LogsInput>)
        -> Result<String, ErrorData>
    {
        let pattern = input.pattern.as_deref().map(Regex::new).transpose().map_err(error)?;
        let record = self.jobs.get(&input.id).map_err(|e| error(format!("{e:#}")))?;
        let path = match input.artifact {
            None => record.log_path,
            Some(value) => {
                let artifact = if let Some(index) = value.as_u64() {
                    usize::try_from(index).ok().and_then(|i| record.spec.artifacts.get(i))
                } else if let Some(path) = value.as_str() {
                    record.spec.artifacts.iter().find(|p| *p == path)
                } else { None }.ok_or_else(|| error("artifact is not declared"))?;
                record.spec.cwd.join(artifact)
            }
        };
        let file = std::fs::File::open(path).map_err(error)?;
        let limit = input.tail.unwrap_or(500).min(500);
        let mut lines = VecDeque::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(error)?;
            if limit > 0 {
                if lines.len() == limit { lines.pop_front(); }
                lines.push_back(line);
            }
        }
        let lines: Vec<_> = lines.into_iter().filter(|line|
            pattern.as_ref().is_none_or(|re| re.is_match(line))).collect();
        Ok(json!({"lines": lines}).to_string())
    }

    #[tool(description = "Cancel a running job")]
    async fn job_cancel(&self, Parameters(input): Parameters<IdInput>)
        -> Result<String, ErrorData>
    {
        let jobs = self.jobs.clone();
        let record = tokio::task::spawn_blocking(move || {
            let grace = jobs.get(&input.id)?.spec.grace_secs;
            jobs.cancel(&input.id)?;
            let (record, _, _) = jobs.watch(&input.id, grace.saturating_add(65), 0)?;
            anyhow::ensure!(!matches!(record.state, JobState::Pending | JobState::Running),
                "cancellation timed out");
            Ok::<_, anyhow::Error>(record)
        }).await.map_err(error)?.map_err(|e| error(format!("{e:#}")))?;
        serde_json::to_string(&record).map_err(error)
    }

    #[tool(description = "List recent jobs")]
    async fn job_list(&self, Parameters(input): Parameters<ListInput>)
        -> Result<String, ErrorData>
    {
        let mut records = self.jobs.list().map_err(|e| error(format!("{e:#}")))?;
        records.truncate(input.limit.unwrap_or(records.len()));
        serde_json::to_string(&records).map_err(error)
    }
}
