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

#[test]
fn cleanup_kills_separate_group_before_unlock() {
    let (dir, jobs) = setup();
    let mut s = spec("setsid sleep 60 & echo $! > child; wait");
    s.cwd = dir.path().into();
    s.grace_secs = 1;
    s.locks.push("exclusive".into());
    s.cleanup = Some(vec!["sh".into(), "-c".into(),
        // The job's descendant-group kill may already have stopped the child.
        "touch cleaning; sleep 0.5; kill $(cat child) 2>/dev/null; echo done".into()]);
    let id = jobs.start(s.clone()).unwrap();
    while !dir.path().join("child").exists() { thread::sleep(Duration::from_millis(10)); }
    let pid: u32 = std::fs::read_to_string(dir.path().join("child")).unwrap().trim().parse().unwrap();
    jobs.cancel(&id).unwrap();
    while !dir.path().join("cleaning").exists() { thread::sleep(Duration::from_millis(10)); }
    assert!(jobs.start(s.clone()).is_err());
    assert_eq!(finished(&jobs, &id).state, JobState::Cancelled);
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    assert!(stat.is_empty() || stat.split(") ").nth(1).unwrap().starts_with('Z'));
    assert!(std::fs::read_to_string(dir.path().join(id).join("output.log"))
        .unwrap().contains("== cleanup ==\ndone"));
    let next = jobs.start(s).unwrap();
    jobs.cancel(&next).unwrap();
    finished(&jobs, &next);
}

#[test]
fn cancel_stops_descendants_in_their_own_group_without_cleanup() {
    let (dir, jobs) = setup();
    let mut s = spec("setsid sleep 60 & echo $! > child; wait");
    s.cwd = dir.path().into();
    s.grace_secs = 1;
    let id = jobs.start(s).unwrap();
    while !dir.path().join("child").exists() { thread::sleep(Duration::from_millis(10)); }
    let pid: u32 = std::fs::read_to_string(dir.path().join("child")).unwrap().trim().parse().unwrap();
    jobs.cancel(&id).unwrap();
    assert_eq!(finished(&jobs, &id).state, JobState::Cancelled);
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    assert!(stat.is_empty() || stat.split(") ").nth(1).unwrap().starts_with('Z'), "{stat}");
}

#[test]
fn artifact_marker_fails_job() {
    let (dir, jobs) = setup();
    let mut s = spec("echo FATAL > artifact.log");
    s.cwd = dir.path().into();
    s.artifacts.push("artifact.log".into());
    s.fail_markers.push("FATAL".into());
    let id = jobs.start(s).unwrap();
    let record = finished(&jobs, &id);
    assert_eq!(record.state, JobState::Failed);
    assert_eq!(record.marker_hits[0].text, "FATAL");
}

#[test]
fn lost_supervisor_runs_cleanup() {
    let (dir, jobs) = setup();
    // A descendant in its own group must not survive Lost recovery either.
    let mut s = spec("setsid sleep 60 & echo $! > child; wait");
    s.cwd = dir.path().into();
    s.cleanup = Some(vec!["sh".into(), "-c".into(), "echo recovered > recovered".into()]);
    let id = jobs.start(s).unwrap();
    while !dir.path().join("child").exists() { thread::sleep(Duration::from_millis(10)); }
    let child: u32 = std::fs::read_to_string(dir.path().join("child")).unwrap().trim().parse().unwrap();
    let record = jobs.get(&id).unwrap();
    let pid = rustix::process::Pid::from_raw(record.supervisor_pid.unwrap() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::KILL).unwrap();
    assert_eq!(finished(&jobs, &id).state, JobState::Lost);
    assert_eq!(std::fs::read_to_string(dir.path().join("recovered")).unwrap(), "recovered\n");
    let deadline = Instant::now() + Duration::from_secs(2);
    let gone = || {
        let stat = std::fs::read_to_string(format!("/proc/{child}/stat")).unwrap_or_default();
        stat.is_empty() || stat.split(") ").nth(1).unwrap().starts_with('Z')
    };
    while !gone() && Instant::now() < deadline { thread::sleep(Duration::from_millis(20)); }
    assert!(gone(), "separate-group descendant survived Lost recovery");
}

#[test]
fn declared_artifacts_cover_every_cell_in_its_own_cwd() {
    let (dir, jobs) = setup();
    let (first_dir, second_dir) = (dir.path().join("one"), dir.path().join("two"));
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();
    let mut first = spec("true");
    first.cwd = first_dir.clone();
    first.artifacts.push("a.log".into());
    let mut second = spec("true");
    second.cwd = second_dir.clone();
    second.artifacts.push("b.log".into());
    let id = jobs.start_sequence(vec![first, second], true).unwrap();
    finished(&jobs, &id);
    assert_eq!(jobs.declared_artifacts(&id).unwrap(), vec![
        ("a.log".to_string(), first_dir.join("a.log")),
        ("b.log".to_string(), second_dir.join("b.log")),
    ]);
}

#[test]
fn timeout_cell_continues_sequence() {
    let (_dir, jobs) = setup();
    let mut first = spec("sleep 30");
    first.timeout_secs = 1;
    first.grace_secs = 0;
    let id = jobs.start_sequence(vec![first, spec("echo SECOND-RAN")], false).unwrap();
    let (record, log, _) = jobs.watch(&id, 10, 0).unwrap();
    assert_eq!(record.state, JobState::TimedOut);
    assert!(log.contains("SECOND-RAN"));
}

#[test]
fn sequence_has_single_deadline() {
    let (_dir, jobs) = setup();
    let mut s = spec("sleep 0.75");
    s.sequence_timeout_secs = Some(1);
    s.grace_secs = 0;
    let start = Instant::now();
    let id = jobs.start_sequence(vec![s; 3], false).unwrap();
    assert_eq!(finished(&jobs, &id).state, JobState::TimedOut);
    assert!(start.elapsed() < Duration::from_millis(1500));
}

#[test]
fn cli_watch_drains_large_log() {
    let (dir, jobs) = setup();
    let id = jobs.start(spec("head -c 100000 /dev/zero | tr '\\0' x; echo; echo END"))
        .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_job-supervisor"))
        .args(["job", "watch", &id]).env("MCP_JOBS_DIR", dir.path()).output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).ends_with("END\n"));
}
