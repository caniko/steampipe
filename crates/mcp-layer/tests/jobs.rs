use mcp_layer::jobs::{JobSpec, JobState, Jobs};
use std::{path::PathBuf, thread, time::{Duration, Instant}};

fn setup() -> (tempfile::TempDir, Jobs) {
    let dir = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(dir.path(), PathBuf::from(env!("CARGO_BIN_EXE_job-supervisor")));
    (dir, jobs)
}

fn spec(command: &str) -> JobSpec {
    JobSpec {
        argv: vec!["sh".into(), "-c".into(), command.into()],
        cwd: std::env::current_dir().unwrap(),
        timeout_secs: 10,
        ..Default::default()
    }
}

fn finished(jobs: &Jobs, id: &str) -> mcp_layer::jobs::JobRecord {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = jobs.get(id).unwrap();
        if !matches!(record.state, JobState::Pending | JobState::Running) {
            return record;
        }
        assert!(Instant::now() < deadline, "job did not finish");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn exit_and_restart() {
    let (dir, jobs) = setup();
    let id = jobs.start(spec("exit 3")).unwrap();
    assert_eq!(finished(&jobs, &id).state, JobState::Failed);
    let fresh = Jobs::new(dir.path(), PathBuf::from(env!("CARGO_BIN_EXE_job-supervisor")));
    assert_eq!(fresh.get(&id).unwrap().exit_code, Some(3));
}

#[test]
fn fail_marker() {
    let (_dir, jobs) = setup();
    let mut s = spec("echo fatal-42");
    s.fail_markers.push("fatal-[0-9]+".into());
    let id = jobs.start(s).unwrap();
    assert_eq!(finished(&jobs, &id).state, JobState::Failed);
}

#[test]
fn timeout_kills_group_and_grandchild() {
    let (dir, jobs) = setup();
    let mut s = spec("sleep 60 & echo $! > grandchild; wait");
    s.cwd = dir.path().into();
    s.timeout_secs = 1;
    let id = jobs.start(s).unwrap();
    let record = finished(&jobs, &id);
    assert_eq!(record.state, JobState::TimedOut);
    let pid: i32 = std::fs::read_to_string(dir.path().join("grandchild"))
        .unwrap().trim().parse().unwrap();
    // A zombie is already dead; its reaping belongs to the host init process.
    for process in [pid, record.pgid.unwrap() as i32] {
        let stat = std::fs::read_to_string(format!("/proc/{process}/stat")).unwrap_or_default();
        assert!(stat.is_empty() || stat.split(") ").nth(1).unwrap().starts_with('Z'));
    }
}

#[test]
fn cancellation_and_locks() {
    let (_dir, jobs) = setup();
    let mut s = spec("sleep 60");
    s.locks.push("network".into());
    let id = jobs.start(s.clone()).unwrap();
    assert!(jobs.start(s).is_err());
    jobs.cancel(&id).unwrap();
    assert_eq!(finished(&jobs, &id).state, JobState::Cancelled);
}

#[test]
fn cancellation_allows_term_handler_to_exit() {
    let (dir, jobs) = setup();
    let mut s = spec("trap 'echo stopped; exit 0' TERM; echo ready; while :; do sleep 1; done");
    s.cwd = dir.path().into();
    let id = jobs.start(s).unwrap();
    let log = dir.path().join(&id).join("output.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(&log).unwrap_or_default().contains("ready") {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    jobs.cancel(&id).unwrap();
    let record = finished(&jobs, &id);
    assert_eq!(record.state, JobState::Cancelled);
    assert_eq!(record.exit_code, Some(0));
    assert!(std::fs::read_to_string(log).unwrap().contains("stopped"));
}

#[test]
fn cli_watch_returns_failure() {
    let (dir, jobs) = setup();
    let id = jobs.start(spec("echo failed; exit 3")).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_job-supervisor"))
        .args(["job", "watch", &id])
        .env("MCP_JOBS_DIR", dir.path())
        .output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("failed"));
}

#[test]
fn dead_supervisor_is_lost() {
    let (_dir, jobs) = setup();
    let id = jobs.start(spec("sleep 60")).unwrap();
    let record = jobs.get(&id).unwrap();
    let supervisor = rustix::process::Pid::from_raw(record.supervisor_pid.unwrap() as i32).unwrap();
    rustix::process::kill_process(supervisor, rustix::process::Signal::KILL).unwrap();
    assert_eq!(finished(&jobs, &id).state, JobState::Lost);
    if let Some(pgid) = record.pgid {
        let _ = rustix::process::kill_process_group(
            rustix::process::Pid::from_raw(pgid as i32).unwrap(), rustix::process::Signal::KILL);
    }
}
