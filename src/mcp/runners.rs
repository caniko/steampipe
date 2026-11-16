use anyhow::Result;
use mcp_layer::{
    jobs::JobSpec,
    profiles::{Runner, RunnerRegistry},
};
use serde::Deserialize;
use toml::Table;

use super::{SteampipeCtx, default_cluster};

pub fn register(registry: &mut RunnerRegistry, ctx: &SteampipeCtx) -> Result<()> {
    for name in ["cluster-test", "cluster-deploy", "nix-run"] {
        registry.register(SteampipeRunner {
            name,
            ctx: ctx.clone(),
        })?;
    }
    Ok(())
}

struct SteampipeRunner {
    name: &'static str,
    ctx: SteampipeCtx,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClusterTest {
    #[serde(default = "default_cluster")]
    cluster: String,
    vm_count: Option<u8>,
    network: Option<String>,
    players: Option<u8>,
    max_runs: Option<u32>,
    timeout: Option<u64>,
    hard_timeout: Option<u64>,
    shutdown_timeout: Option<u64>,
    stop_on_failure: Option<bool>,
    deploy: Option<bool>,
    build: Option<bool>,
    display: Option<String>,
    capture_on_failure: Option<bool>,
    // Screenshot backend and visual validator come from a steampipe.toml
    // [profiles.<name>] entry, as with `cluster-ctl test --profile`.
    profile: Option<String>,
    chaos_profile: Option<String>,
    filter_pattern: Option<String>,
    output_file: Option<String>,
    host_args: Option<String>,
    vm_args: Option<String>,
    strace: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClusterDeploy {
    #[serde(default = "default_cluster")]
    cluster: String,
    vm_count: Option<u8>,
    build: Option<bool>,
    verify: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NixRun {
    target: String,
    args: Option<Vec<String>>,
    flake: Option<String>,
    timeout: Option<u64>,
}

fn option<T: ToString>(args: &mut Vec<String>, name: &str, value: Option<T>) {
    if let Some(value) = value {
        args.push(format!("--{name}={}", value.to_string()));
    }
}

impl Runner for SteampipeRunner {
    fn name(&self) -> &str {
        self.name
    }
    fn build(&self, params: &Table) -> Result<JobSpec> {
        let mut spec = JobSpec {
            runner: self.name.into(),
            cwd: self.ctx.project_root.clone(),
            env: vec![(super::MCP_NON_INTERACTIVE_ENV.into(), "1".into())],
            timeout_secs: 14_400,
            ..Default::default()
        };
        match self.name {
            "cluster-test" => {
                let p: ClusterTest = params.clone().try_into()?;
                // cluster-ctl defaults to 8 players; a 1v1 cluster has one VM, so without
                // an explicit count or a steampipe.toml profile it means 2, as the old tool did.
                let players = p
                    .players
                    .or_else(|| (p.profile.is_none() && p.cluster == "1v1").then_some(2));
                let a = &mut spec.argv;
                a.push(self.ctx.cluster_ctl.to_string_lossy().into_owned());
                a.push("test".into());
                option(a, "cluster", Some(&p.cluster));
                // Cancel/timeout: the job kills local descendants itself; VM games
                // live on other machines, so stop them through cluster-ctl.
                spec.cleanup = Some(vec![
                    self.ctx.cluster_ctl.to_string_lossy().into_owned(),
                    format!("--cluster={}", p.cluster),
                    "stop-game".into(),
                ]);
                option(a, "profile", p.profile);
                option(a, "chaos-profile", p.chaos_profile);
                option(a, "vm-count", p.vm_count);
                option(a, "network", p.network);
                option(a, "players", players);
                option(a, "max-runs", p.max_runs);
                option(a, "timeout", p.timeout);
                option(a, "hard-timeout", p.hard_timeout);
                option(a, "shutdown-timeout", p.shutdown_timeout);
                option(a, "display", p.display);
                option(a, "filter-pattern", p.filter_pattern);
                option(a, "output-file", p.output_file);
                option(a, "host-args", p.host_args);
                option(a, "vm-args", p.vm_args);
                for (flag, enabled) in [
                    ("no-stop-on-failure", p.stop_on_failure == Some(false)),
                    ("no-deploy", p.deploy == Some(false)),
                    ("no-build", p.build == Some(false)),
                    ("capture-on-failure", p.capture_on_failure == Some(true)),
                    ("strace", p.strace == Some(true)),
                ] {
                    if enabled {
                        a.push(format!("--{flag}"));
                    }
                }
                spec.locks.push(format!("cluster-{}", p.cluster));
            }
            "cluster-deploy" => {
                let p: ClusterDeploy = params.clone().try_into()?;
                spec.argv = vec![
                    self.ctx.cluster_ctl.to_string_lossy().into_owned(),
                    "deploy".into(),
                ];
                option(&mut spec.argv, "cluster", Some(&p.cluster));
                option(&mut spec.argv, "vm-count", p.vm_count);
                if p.build == Some(false) {
                    spec.argv.push("--no-build".into());
                }
                if p.verify == Some(true) {
                    spec.argv.push("--verify".into());
                }
                spec.locks.push(format!("cluster-{}", p.cluster));
            }
            "nix-run" => {
                let p: NixRun = params.clone().try_into()?;
                spec.argv = vec![
                    "nix".into(),
                    "run".into(),
                    format!("{}#{}", p.flake.as_deref().unwrap_or("."), p.target),
                ];
                if let Some(args) = p.args.filter(|a| !a.is_empty()) {
                    spec.argv.push("--".into());
                    spec.argv.extend(args);
                }
                spec.timeout_secs = p.timeout.unwrap_or(300);
            }
            _ => unreachable!(),
        }
        if self.name != "nix-run" {
            let path = self.ctx.project_root.join("steampipe.toml");
            if let Ok(text) = std::fs::read_to_string(path) {
                let config: Table = toml::from_str(&text)?;
                if let Some(labels) = config.get("exit_codes").and_then(|v| v.as_table()) {
                    spec.fail_markers = labels
                        .iter()
                        .filter(|(code, _)| code.as_str() != "0")
                        .filter_map(|(_, v)| v.as_str())
                        .map(regex::escape)
                        .collect();
                }
            }
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn one_v_one_defaults_to_two_players_unless_set_or_profiled() {
        let ctx = SteampipeCtx {
            project_root: ".".into(),
            cluster_ctl: "cluster-ctl".into(),
        };
        let runner = SteampipeRunner {
            name: "cluster-test",
            ctx,
        };
        let argv = |toml: &str| runner.build(&toml::from_str(toml).unwrap()).unwrap().argv;
        assert!(argv(r#"cluster = "1v1""#).contains(&"--players=2".to_string()));
        assert!(
            argv(
                r#"cluster = "1v1"
            players = 3"#
            )
            .contains(&"--players=3".to_string())
        );
        let profiled = argv(
            r#"cluster = "1v1"
            profile = "smoke""#,
        );
        assert!(!profiled.iter().any(|arg| arg.starts_with("--players")));
        let tournament = argv(r#"cluster = "tournament""#);
        assert!(!tournament.iter().any(|arg| arg.starts_with("--players")));
    }

    #[test]
    fn cluster_test_argv_is_accepted_and_unknown_keys_rejected() {
        let ctx = SteampipeCtx {
            project_root: ".".into(),
            cluster_ctl: "cluster-ctl".into(),
        };
        let runner = SteampipeRunner {
            name: "cluster-test",
            ctx,
        };
        let params: Table = toml::from_str(
            r#"
            cluster = "tournament"
            vm_count = 7
            network = "steam"
            players = 8
            max_runs = 2
            timeout = 300
            hard_timeout = 600
            shutdown_timeout = 5
            stop_on_failure = false
            deploy = false
            build = false
            display = "headless"
            capture_on_failure = true
            profile = "smoke"
            chaos_profile = "lossy"
            filter_pattern = "error"
            output_file = "out"
            host_args = "--test"
            vm_args = "--test"
            strace = true
        "#,
        )
        .unwrap();
        let spec = runner.build(&params).unwrap();
        assert!(crate::Cli::try_parse_from(&spec.argv).is_ok());
        assert!(spec.argv.contains(&"--profile=smoke".to_string()));
        let cleanup = spec
            .cleanup
            .as_ref()
            .expect("cluster-test stops VM games on cancel");
        assert!(crate::Cli::try_parse_from(cleanup).is_ok(), "{cleanup:?}");
        assert_eq!(cleanup.last().map(String::as_str), Some("stop-game"));
        let mut invalid = params.clone();
        invalid.insert(
            "screenshot_backend".into(),
            toml::Value::String("vnc".into()),
        );
        assert!(
            runner.build(&invalid).is_err(),
            "visual settings come from a profile"
        );
        let mut invalid = params;
        invalid.insert("typo".into(), toml::Value::Boolean(true));
        assert!(runner.build(&invalid).is_err());
    }
}
