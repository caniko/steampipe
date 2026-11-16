//! Durable, process-supervised jobs.

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use rustix::{fs::{flock, FlockOperation},
    process::{kill_process_group, test_kill_process_group, Pid, Signal}};
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
}

fn default_grace() -> u64 { 5 }

impl Default for JobSpec {
    fn default() -> Self {
        Self { runner: String::new(), argv: Vec::new(), cwd: PathBuf::new(), env: Vec::new(), timeout_secs: 0,
            grace_secs: default_grace(), locks: Vec::new(), fail_markers: Vec::new(),
            artifacts: Vec::new() }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum JobState { Pending, Running, Passed, Failed, TimedOut, Cancelled, Lost }

impl JobState {
    fn active(self) -> bool { matches!(self, Self::Pending | Self::Running) }
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
pub struct Jobs { dir: PathBuf, supervisor: PathBuf }

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.bytes().all(|c|
        c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.')
        && name != "." && name != ".."
}

fn lock(path: &Path, nonblocking: bool) -> Result<File> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    flock(&file, if nonblocking { FlockOperation::NonBlockingLockExclusive }
        else { FlockOperation::LockExclusive })?;
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
    stat.rsplit_once(") ").is_some_and(|(_, state)| !state.starts_with('Z'))
}

impl Jobs {
    pub fn new(dir: impl Into<PathBuf>, supervisor: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), supervisor: supervisor.into() }
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

    fn start_inner(&self, specs: Vec<JobSpec>, stop_on_failure: bool, sequence: bool)
        -> Result<String>
    {
        ensure!(!specs.is_empty(), "empty sequence");
        for spec in &specs {
        ensure!(!spec.argv.is_empty() && !spec.argv[0].is_empty(), "empty argv");
        ensure!(spec.timeout_secs > 0 && spec.timeout_secs <= 14_400, "invalid timeout");
        for marker in &spec.fail_markers { Regex::new(marker)?; }
        for key in &spec.locks { ensure!(valid_name(key), "invalid lock name"); }
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
        ensure!(records.iter().filter(|r| r.state.active()).count() < 8, "job limit reached");
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
            id: id.clone(), spec, state: JobState::Pending, supervisor_pid: None, pid: None,
            pgid: None, start: now(), end: None, exit_code: None, signal: None,
            marker_hits: Vec::new(), log_path: path.join("output.log"),
        };
        write_record(&path, &record)?;
        if sequence {
            fs::write(path.join("sequence.json"), serde_json::to_vec(&(specs, stop_on_failure))?)?;
        }
        let spawned = Command::new(&self.supervisor).arg("__job").arg(&path)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .process_group(0).spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let mut failed = record;
                failed.state = JobState::Failed;
                failed.end = Some(now());
                fs::write(&failed.log_path, format!("supervisor spawn failed: {error}\n"))?;
                write_record(&path, &failed)?;
                return Err(error).context("spawn supervisor");
            }
        };
        fs::write(path.join("supervisor.pid"), child.id().to_string())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let r: JobRecord = serde_json::from_slice(&fs::read(path.join("record.json"))?)?;
            if r.state != JobState::Pending {
                thread::spawn(move || { let _ = child.wait(); });
                return Ok(id);
            }
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let mut failed: JobRecord =
                    serde_json::from_slice(&fs::read(path.join("record.json"))?)?;
                if failed.state.active() {
                    if let Some(pid) = failed.pgid.and_then(|p| Pid::from_raw(p as i32)) {
                        let _ = kill_process_group(pid, Signal::KILL);
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

    pub fn get(&self, id: &str) -> Result<JobRecord> {
        let dir = self.job_dir(id)?;
        let mut record: JobRecord = serde_json::from_slice(&fs::read(dir.join("record.json"))?)?;
        let pid = record.supervisor_pid.or_else(|| fs::read_to_string(dir.join("supervisor.pid"))
            .ok().and_then(|s| s.parse().ok()));
        if record.state.active() && pid.is_some_and(|pid| !alive(pid)) {
            record.state = JobState::Lost;
        }
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<JobRecord>> {
        if !self.dir.exists() { return Ok(Vec::new()); }
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
    pub fn watch(&self, id: &str, wait_secs: u64, cursor: u64)
        -> Result<(JobRecord, String, u64)>
    {
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
    if args.next().as_deref() != Some(std::ffi::OsStr::new("__job")) { return Ok(false); }
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
    let total = specs.len();
    for (index, spec) in specs.iter().enumerate() {
        if headers {
            writeln!(OpenOptions::new().create(true).append(true).open(&record.log_path)?,
                "== cell {}/{}: {} ==", index + 1, total, spec.runner)?;
        }
        let offset = fs::metadata(&record.log_path).map_or(0, |m| m.len());
        run_cell(dir, &mut record, spec, offset)?;
        if matches!(record.state, JobState::Cancelled | JobState::TimedOut) { break; }
        if record.state == JobState::Failed {
            failed = true;
            if stop { break; }
        }
        record.state = JobState::Running;
    }
    if record.state == JobState::Running {
        record.state = if failed { JobState::Failed } else { JobState::Passed };
    }
    record.end = Some(now());
    write_record(dir, &record)?;
    drop(held);
    Ok(())
}

fn run_cell(dir: &Path, record: &mut JobRecord, spec: &JobSpec, offset: u64) -> Result<()> {
    if dir.join("cancel").exists() {
        record.state = JobState::Cancelled;
        return Ok(());
    }
    let log = OpenOptions::new().create(true).append(true).open(&record.log_path)?;
    let mut command = Command::new(&spec.argv[0]);
    command.args(&spec.argv[1..]).current_dir(&spec.cwd)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).process_group(0);
    record.supervisor_pid = Some(std::process::id());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            writeln!(OpenOptions::new().append(true).open(&record.log_path)?,
                "spawn failed: {error}")?;
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
        let reason = if dir.join("cancel").exists() { Some(JobState::Cancelled) }
            else if started.elapsed() >= Duration::from_secs(spec.timeout_secs) {
                Some(JobState::TimedOut)
            } else { None };
        if let Some(reason) = reason {
            record.state = reason;
            break if let Some(pid) = Pid::from_raw(child.id() as i32) {
                let _ = kill_process_group(pid, Signal::TERM);
                let until = Instant::now() + Duration::from_secs(spec.grace_secs);
                let mut status = None;
                loop {
                    if status.is_none() {
                        status = child.try_wait()?;
                    }
                    if test_kill_process_group(pid).is_err() && let Some(status) = status {
                        break status;
                    }
                    if Instant::now() >= until {
                        let _ = kill_process_group(pid, Signal::KILL);
                        break match status { Some(status) => status, None => child.wait()? };
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            } else {
                child.wait()?
            };
        }
        if let Some(status) = child.try_wait()? { break status; }
        thread::sleep(Duration::from_millis(50));
    };
    record.exit_code = status.code();
    record.signal = status.signal();
    let patterns: Vec<_> = spec.fail_markers.iter()
        .map(|s| Regex::new(s)).collect::<std::result::Result<_, _>>()?;
    let mut file = File::open(&record.log_path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let content = String::from_utf8_lossy(&bytes);
    for (number, line) in content.lines().enumerate() {
        for (pattern, re) in spec.fail_markers.iter().zip(&patterns) {
            if re.is_match(line) && record.marker_hits.len() < 20 {
                record.marker_hits.push(MarkerHit { line: number + 1,
                    text: line.chars().take(300).collect(), pattern: pattern.clone() });
            }
        }
    }
    if record.state == JobState::Running {
        record.state = if status.success() && !patterns.iter().any(|re| re.is_match(&content)) {
            JobState::Passed
        } else { JobState::Failed };
    }
    Ok(())
}
