use mcp_layer::{jobs::{JobSpec, JobState, Jobs}, profiles::{ProfileOrInline, Profiles,
    Runner, RunnerRegistry}};
use serde::Deserialize;
use toml::Table;

struct EchoRunner;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoParams { msg: String, code: i32 }

impl Runner for EchoRunner {
    fn name(&self) -> &str { "echo" }
    fn build(&self, params: &Table) -> anyhow::Result<JobSpec> {
        let p: EchoParams = toml::Value::Table(params.clone()).try_into()?;
        Ok(JobSpec {
            argv: vec!["sh".into(), "-c".into(), "echo \"$msg\"; exit \"$code\"".into()],
            cwd: std::env::current_dir()?,
            env: vec![("msg".into(), p.msg), ("code".into(), p.code.to_string())],
            timeout_secs: 10,
            ..Default::default()
        })
    }
}

#[test]
fn profiles_and_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.toml");
    let second = dir.path().join("second.toml");
    std::fs::write(&first, r#"
[job_profiles.single]
runner = "echo"
params = { msg = "old", code = 0 }
[job_profiles.matrix]
cells = [
  { runner = "echo", params = { msg = "first", code = 1 } },
  { runner = "echo", params = { msg = "second", code = 0 } },
]
"#).unwrap();
    std::fs::write(&second, r#"
[job_profiles.single]
runner = "echo"
params = { msg = "new", code = 0 }
repeats = 2
"#).unwrap();
    let profiles = Profiles::load(&[dir.path().join("missing"), first, second]).unwrap();
    assert_eq!(profiles.list().len(), 2);
    let mut registry = RunnerRegistry::default();
    registry.register(EchoRunner).unwrap();
    assert!(registry.register(EchoRunner).is_err());
    let overrides: Table = toml::from_str("msg = 'override'").unwrap();
    let specs = profiles.resolve(ProfileOrInline::Name("single"), overrides, &registry).unwrap();
    assert_eq!(specs.len(), 2);
    assert!(specs[0].env.contains(&("msg".into(), "override".into())));
    let invalid: Table = toml::from_str("unknown = 1").unwrap();
    assert!(profiles.resolve(ProfileOrInline::Name("single"), invalid, &registry).is_err());
    let specs = profiles.resolve(ProfileOrInline::Name("matrix"), Table::new(), &registry).unwrap();
    let jobs = Jobs::new(dir.path().join("jobs"), env!("CARGO_BIN_EXE_job-supervisor"));
    for stop in [true, false] {
        let id = jobs.start_sequence(specs.clone(), stop).unwrap();
        let (record, log, _) = jobs.watch(&id, 10, 0).unwrap();
        assert_eq!(record.state, JobState::Failed);
        assert!(log.contains("== cell 1/2: echo =="));
        assert_eq!(log.contains("second"), !stop);
    }
}

#[test]
fn invalid_profile_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "[job_profiles.bad]\nrunner='echo'\ncells=[{runner='echo'}]")
        .unwrap();
    assert!(Profiles::load(std::slice::from_ref(&path)).is_err());
    std::fs::write(&path, "[").unwrap();
    assert!(format!("{:#}", Profiles::load(std::slice::from_ref(&path)).err().unwrap())
        .contains("bad.toml"));
}
