//! Durable, process-supervised jobs.

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use rustix::{
    fs::{FlockOperation, flock},
    process::{Pid, Signal, kill_process_group, test_kill_process_group},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobSpec {
    #[serde(default)]
    pub runner: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub timeout_secs: u64,
    #[serde(default = "default_grace")]
    pub grace_secs: u64,
    pub locks: Vec<String>,
    pub fail_markers: Vec<String>,
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub cleanup: Option<Vec<String>>,
    #[serde(default)]
    pub sequence_timeout_secs: Option<u64>,
}

fn default_grace() -> u64 {
    5
}

impl Default for JobSpec {
    fn default() -> Self {
        Self {
            runner: String::new(),
            argv: Vec::new(),
            cwd: PathBuf::new(),
            env: Vec::new(),
            timeout_secs: 0,
            grace_secs: default_grace(),
            locks: Vec::new(),
            fail_markers: Vec::new(),
            artifacts: Vec::new(),
            cleanup: None,
            sequence_timeout_secs: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum JobState {
    Pending,
    Running,
    Passed,
    Failed,
    TimedOut,
    Cancelled,
    Lost,
}

impl JobState {
    fn active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarkerHit {
    pub line: usize,
    pub text: String,
    pub pattern: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub spec: JobSpec,
    pub state: JobState,
    pub supervisor_pid: Option<u32>,
    pub pid: Option<u32>,
    pub pgid: Option<u32>,
    pub start: u64,
    pub end: Option<u64>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub marker_hits: Vec<MarkerHit>,
    pub log_path: PathBuf,
}

#[derive(Clone)]
pub struct Jobs {
    dir: PathBuf,
    supervisor: PathBuf,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.')
        && name != "."
        && name != ".."
}

fn lock(path: &Path, nonblocking: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    flock(
        &file,
        if nonblocking {
            FlockOperation::NonBlockingLockExclusive
        } else {
            FlockOperation::LockExclusive
        },
    )?;
    Ok(file)
}

fn write_record(dir: &Path, record: &JobRecord) -> Result<()> {
    let tmp = dir.join(format!("record.{}.tmp", std::process::id()));
    fs::write(&tmp, serde_json::to_vec(record)?)?;
    fs::rename(tmp, dir.join("record.json"))?;
    Ok(())
}

// ponytail: PID liveness only; compare process start times if PID reuse causes false positives.
fn alive(pid: u32) -> bool {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    stat.rsplit_once(") ")
        .is_some_and(|(_, state)| !state.starts_with('Z'))
}

impl Jobs {
    pub fn new(dir: impl Into<PathBuf>, supervisor: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            supervisor: supervisor.into(),
        }
    }

    fn job_dir(&self, id: &str) -> Result<PathBuf> {
        ensure!(valid_name(id), "invalid job id");
        Ok(self.dir.join(id))
    }

    pub fn start(&self, spec: JobSpec) -> Result<String> {
        self.start_inner(vec![spec], true, false)
    }

    /// Runs all cells in one durable supervisor and record, holding their combined locks
    /// for the entire sequence. Timeouts and markers are evaluated per cell.
    pub fn start_sequence(&self, specs: Vec<JobSpec>, stop_on_failure: bool) -> Result<String> {
        self.start_inner(specs, stop_on_failure, true)
    }

    fn start_inner(
        &self,
        specs: Vec<JobSpec>,
        stop_on_failure: bool,
        sequence: bool,
    ) -> Result<String> {
        ensure!(!specs.is_empty(), "empty sequence");
        for spec in &specs {
            ensure!(
                !spec.argv.is_empty() && !spec.argv[0].is_empty(),
                "empty argv"
            );
            ensure!(
                spec.timeout_secs > 0 && spec.timeout_secs <= 14_400,
                "invalid timeout"
            );
            ensure!(
                spec.sequence_timeout_secs
                    .is_none_or(|t| t > 0 && t <= 14_400),
                "invalid sequence timeout"
            );
            ensure!(
                spec.cleanup
                    .as_ref()
                    .is_none_or(|c| !c.is_empty() && !c[0].is_empty()),
                "empty cleanup argv"
            );
            for marker in &spec.fail_markers {
                Regex::new(marker)?;
            }
            for key in &spec.locks {
                ensure!(valid_name(key), "invalid lock name");
            }
            let mut keys = spec.locks.clone();
            keys.sort();
            keys.dedup();
            ensure!(keys.len() == spec.locks.len(), "duplicate lock name");
        }
        let mut spec = specs[0].clone();
        let mut keys: Vec<_> = specs.iter().flat_map(|s| s.locks.iter().cloned()).collect();
        keys.sort();
        keys.dedup();
        spec.locks.clone_from(&keys);
        fs::create_dir_all(self.dir.join("locks"))?;
        let _gate = lock(&self.dir.join("start.lock"), false)?;
        let records = self.list()?;
        ensure!(
            records.iter().filter(|r| r.state.active()).count() < 8,
            "job limit reached"
        );
        self.prune_inner(&records, 50)?;
        // Check under the start gate; the supervisor takes the locks before releasing it.
        for key in &keys {
            let _check = lock(&self.dir.join("locks").join(key), true)
                .with_context(|| format!("lock {key} is held"))?;
        }
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let id = format!("job-{nanos}-{}", std::process::id());
        let path = self.job_dir(&id)?;
        fs::create_dir(&path)?;
        let path = fs::canonicalize(path)?;
        let record = JobRecord {
            id: id.clone(),
            spec,
            state: JobState::Pending,
            supervisor_pid: None,
            pid: None,
            pgid: None,
            start: now(),
            end: None,
            exit_code: None,
            signal: None,
            marker_hits: Vec::new(),
            log_path: path.join("output.log"),
        };
        write_record(&path, &record)?;
        if sequence {
            fs::write(
                path.join("sequence.json"),
                serde_json::to_vec(&(specs, stop_on_failure))?,
            )?;
        }
        let spawned = Command::new(&self.supervisor)
            .arg("__job")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let mut failed = record;
                failed.state = JobState::Failed;
                failed.end = Some(now());
                fs::write(
                    &failed.log_path,
                    format!("supervisor spawn failed: {error}\n"),
                )?;
                write_record(&path, &failed)?;
                return Err(error).context("spawn supervisor");
            }
        };
        fs::write(path.join("supervisor.pid"), child.id().to_string())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let r: JobRecord = serde_json::from_slice(&fs::read(path.join("record.json"))?)?;
            if r.state != JobState::Pending {
                thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(id);
            }
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let mut failed: JobRecord =
                    serde_json::from_slice(&fs::read(path.join("record.json"))?)?;
                if failed.state.active() {
                    if let Some(pid) = failed.pgid.and_then(|p| Pid::from_raw(p as i32)) {
                        for group in process_groups_under(pid) {
                            let _ = kill_process_group(group, Signal::KILL);
                        }
                    }
                    failed.state = JobState::Lost;
                    failed.end = Some(now());
                    write_record(&path, &failed)?;
                }
                bail!("supervisor failed to start job {id}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Every declared artifact of a job as `(declared path, resolved path)`, across all
    /// cells of a sequence, each resolved against its own cell's working directory.
    pub fn declared_artifacts(&self, id: &str) -> Result<Vec<(String, PathBuf)>> {
        let dir = self.job_dir(id)?;
        let specs = match fs::read(dir.join("sequence.json")) {
            Ok(bytes) => serde_json::from_slice::<(Vec<JobSpec>, bool)>(&bytes)?.0,
            Err(_) => vec![self.get(id)?.spec],
        };
        Ok(specs
            .iter()
            .flat_map(|spec| {
                spec.artifacts
                    .iter()
                    .map(|artifact| (artifact.clone(), spec.cwd.join(artifact)))
            })
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<JobRecord> {
        let dir = self.job_dir(id)?;
        let mut record: JobRecord = serde_json::from_slice(&fs::read(dir.join("record.json"))?)?;
        let pid = record.supervisor_pid.or_else(|| {
            fs::read_to_string(dir.join("supervisor.pid"))
                .ok()
                .and_then(|s| s.parse().ok())
        });
        if record.state.active() && pid.is_some_and(|pid| !alive(pid)) {
            let Ok(_gate) = lock(&self.dir.join("start.lock"), true) else {
                return Ok(record);
            };
            record = serde_json::from_slice(&fs::read(dir.join("record.json"))?)?;
            if !record.state.active() {
                return Ok(record);
            }
            let mut held = Vec::new();
            for key in &record.spec.locks {
                held.push(lock(&self.dir.join("locks").join(key), false)?);
            }
            if let Some(pid) = record.pgid.and_then(|p| Pid::from_raw(p as i32)) {
                for group in process_groups_under(pid) {
                    let _ = kill_process_group(group, Signal::KILL);
                }
            }
            cleanup(&record.spec, &record.log_path)?;
            record.state = JobState::Lost;
            record.end = Some(now());
            write_record(&dir, &record)?;
        }
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<JobRecord>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.path().join("record.json").exists() {
                records.push(self.get(&entry.file_name().to_string_lossy())?);
            }
        }
        records.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(records)
    }

    pub fn cancel(&self, id: &str) -> Result<()> {
        let record = self.get(id)?;
        ensure!(record.state.active(), "job is not running");
        fs::write(self.job_dir(id)?.join("cancel"), b"")?;
        Ok(())
    }

    /// Wait for completion, returning up to 64 KiB of output from a byte cursor.
    pub fn watch(&self, id: &str, wait_secs: u64, cursor: u64) -> Result<(JobRecord, String, u64)> {
        let deadline = Instant::now() + Duration::from_secs(wait_secs.min(300));
        loop {
            let record = self.get(id)?;
            if !record.state.active() || Instant::now() >= deadline {
                let mut bytes = Vec::new();
                if let Ok(mut file) = File::open(&record.log_path) {
                    ensure!(cursor <= file.metadata()?.len(), "invalid log cursor");
                    file.seek(SeekFrom::Start(cursor))?;
                    file.take(65_536).read_to_end(&mut bytes)?;
                } else {
                    ensure!(cursor == 0, "invalid log cursor");
                }
                let next = cursor + bytes.len() as u64;
                return Ok((record, String::from_utf8_lossy(&bytes).into_owned(), next));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn prune(&self, keep: usize) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let _gate = lock(&self.dir.join("start.lock"), false)?;
        self.prune_inner(&self.list()?, keep)
    }

    fn prune_inner(&self, records: &[JobRecord], keep: usize) -> Result<()> {
        for record in records.iter().filter(|r| !r.state.active()).skip(keep) {
            fs::remove_dir_all(self.job_dir(&record.id)?)?;
        }
        Ok(())
    }
}

/// Call before parsing a server's normal command-line arguments.
pub fn run_if_internal() -> Result<bool> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("__job")) {
        return Ok(false);
    }
    let dir = PathBuf::from(args.next().context("missing job directory")?);
    ensure!(args.next().is_none(), "unexpected supervisor arguments");
    supervise(&dir)?;
    Ok(true)
}

fn supervise(dir: &Path) -> Result<()> {
    let mut record: JobRecord = serde_json::from_slice(&fs::read(dir.join("record.json"))?)?;
    let jobs_dir = dir.parent().context("missing jobs directory")?;
    let mut held = Vec::new();
    for key in &record.spec.locks {
        held.push(lock(&jobs_dir.join("locks").join(key), true)?);
    }
    let sequence: Option<(Vec<JobSpec>, bool)> = match fs::read(dir.join("sequence.json")) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let (specs, stop, headers) = match sequence {
        Some((specs, stop)) => (specs, stop, true),
        None => (vec![record.spec.clone()], true, false),
    };
    record.supervisor_pid = Some(std::process::id());
    record.state = JobState::Running;
    let mut failed = false;
    let mut timed_out = false;
    let deadline = specs[0]
        .sequence_timeout_secs
        .map(|s| Instant::now() + Duration::from_secs(s));
    let total = specs.len();
    for (index, spec) in specs.iter().enumerate() {
        record.spec.cleanup.clone_from(&spec.cleanup);
        record.spec.cwd.clone_from(&spec.cwd);
        record.spec.env.clone_from(&spec.env);
        if headers {
            writeln!(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&record.log_path)?,
                "== cell {}/{}: {} ==",
                index + 1,
                total,
                spec.runner
            )?;
        }
        let offset = fs::metadata(&record.log_path).map_or(0, |m| m.len());
        if deadline.is_some_and(|d| Instant::now() >= d) {
            record.state = JobState::TimedOut;
            break;
        }
        run_cell(dir, &mut record, spec, offset, deadline)?;
        if record.state == JobState::Cancelled {
            break;
        }
        if record.state == JobState::TimedOut {
            timed_out = true;
            if stop || deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
        }
        if record.state == JobState::Failed {
            failed = true;
            if stop {
                break;
            }
        }
        record.state = JobState::Running;
    }
    if record.state == JobState::Running || (timed_out && record.state == JobState::Failed) {
        record.state = if timed_out {
            JobState::TimedOut
        } else if failed {
            JobState::Failed
        } else {
            JobState::Passed
        };
    }
    record.end = Some(now());
    write_record(dir, &record)?;
    drop(held);
    Ok(())
}

fn run_cell(
    dir: &Path,
    record: &mut JobRecord,
    spec: &JobSpec,
    offset: u64,
    deadline: Option<Instant>,
) -> Result<()> {
    if dir.join("cancel").exists() {
        record.state = JobState::Cancelled;
        return Ok(());
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&record.log_path)?;
    let mut command = Command::new(&spec.argv[0]);
    command
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    record.supervisor_pid = Some(std::process::id());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            writeln!(
                OpenOptions::new().append(true).open(&record.log_path)?,
                "spawn failed: {error}"
            )?;
            record.state = JobState::Failed;
            return Ok(());
        }
    };
    record.pid = Some(child.id());
    record.pgid = Some(child.id());
    record.state = JobState::Running;
    write_record(dir, record)?;
    let started = Instant::now();
    let status = loop {
        let reason = if dir.join("cancel").exists() {
            Some(JobState::Cancelled)
        } else if started.elapsed() >= Duration::from_secs(spec.timeout_secs)
            || deadline.is_some_and(|d| Instant::now() >= d)
        {
            Some(JobState::TimedOut)
        } else {
            None
        };
        if let Some(reason) = reason {
            record.state = reason;
            break if let Some(pid) = Pid::from_raw(child.id() as i32) {
                // Runners may start processes in their own groups (cluster-ctl's local
                // host game does); snapshot every descendant group before signalling.
                let groups = process_groups_under(pid);
                let signal_all = |signal| {
                    for group in &groups {
                        let _ = kill_process_group(*group, signal);
                    }
                };
                signal_all(Signal::TERM);
                let until = Instant::now() + Duration::from_secs(spec.grace_secs);
                let mut status = None;
                loop {
                    if status.is_none() {
                        status = child.try_wait()?;
                    }
                    let gone = groups
                        .iter()
                        .all(|group| test_kill_process_group(*group).is_err());
                    if gone && let Some(status) = status {
                        break status;
                    }
                    if Instant::now() >= until {
                        signal_all(Signal::KILL);
                        break match status {
                            Some(status) => status,
                            None => child.wait()?,
                        };
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            } else {
                child.wait()?
            };
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(50));
    };
    record.exit_code = status.code();
    record.signal = status.signal();
    if matches!(record.state, JobState::Cancelled | JobState::TimedOut) {
        cleanup(spec, &record.log_path)?;
    }
    let patterns: Vec<_> = spec
        .fail_markers
        .iter()
        .map(|s| Regex::new(s))
        .collect::<std::result::Result<_, _>>()?;
    let mut file = File::open(&record.log_path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut contents = vec![String::from_utf8_lossy(&bytes).into_owned()];
    for artifact in &spec.artifacts {
        if let Ok(bytes) = fs::read(spec.cwd.join(artifact)) {
            contents.push(String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    let mut hit = false;
    for content in &contents {
        for (number, line) in content.lines().enumerate() {
            for (pattern, re) in spec.fail_markers.iter().zip(&patterns) {
                if re.is_match(line) {
                    hit = true;
                    if record.marker_hits.len() < 20 {
                        record.marker_hits.push(MarkerHit {
                            line: number + 1,
                            text: line.chars().take(300).collect(),
                            pattern: pattern.clone(),
                        });
                    }
                }
            }
        }
    }
    if record.state == JobState::Running {
        record.state = if status.success() && !hit {
            JobState::Passed
        } else {
            JobState::Failed
        };
    }
    Ok(())
}

/// The job's own group plus the process group of every live descendant of `root`,
/// read from /proc (Linux). Orphans already reparented away are not found.
fn process_groups_under(root: Pid) -> Vec<Pid> {
    let mut groups = vec![root];
    let mut queue = vec![root.as_raw_nonzero().get()];
    while let Some(pid) = queue.pop() {
        let tasks = std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten();
        for task in tasks.flatten() {
            let children =
                std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            for child in children
                .split_whitespace()
                .filter_map(|c| c.parse::<i32>().ok())
            {
                queue.push(child);
                let stat =
                    std::fs::read_to_string(format!("/proc/{child}/stat")).unwrap_or_default();
                // Field 5 (pgrp) follows the parenthesised command name.
                let pgrp = stat
                    .rsplit_once(')')
                    .and_then(|(_, rest)| rest.split_whitespace().nth(2)?.parse::<i32>().ok());
                if let Some(group) = pgrp.and_then(Pid::from_raw)
                    && !groups.contains(&group)
                {
                    groups.push(group);
                }
            }
        }
    }
    groups
}

/// Blocking `job watch <id>`: stream the job's log to stdout through EOF and return
/// the process exit code for its final state (0 pass, 1 fail, 124 timeout,
/// 130 cancelled, 3 lost). Shared by every layer's binary.
pub fn watch_to_exit(jobs: &Jobs, id: &str) -> Result<i32> {
    let mut cursor = 0;
    loop {
        let (record, lines, next) = jobs.watch(id, 30, cursor)?;
        print!("{lines}");
        cursor = next;
        let finished = !matches!(record.state, JobState::Pending | JobState::Running);
        if !finished || std::fs::metadata(&record.log_path).is_ok_and(|m| cursor < m.len()) {
            continue;
        }
        std::io::Write::flush(&mut std::io::stdout())?;
        return Ok(match record.state {
            JobState::Pending | JobState::Running => unreachable!("checked above"),
            JobState::Passed => 0,
            JobState::TimedOut => 124,
            JobState::Cancelled => 130,
            JobState::Lost => 3,
            JobState::Failed => 1,
        });
    }
}

fn cleanup(spec: &JobSpec, log_path: &Path) -> Result<()> {
    let Some(argv) = &spec.cleanup else {
        return Ok(());
    };
    if argv.is_empty() {
        return Ok(());
    }
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(log, "== cleanup ==")?;
    let child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            writeln!(log, "cleanup spawn failed: {e}")?;
            return Ok(());
        }
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            if let Some(pid) = Pid::from_raw(child.id() as i32) {
                let _ = kill_process_group(pid, Signal::KILL);
            }
            let _ = child.wait();
            writeln!(log, "cleanup timed out")?;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
