//! Output format support: JUnit XML and JSON Lines for CI integration.

use std::time::Duration;

use clap::ValueEnum;
use serde::Serialize;

/// Output format for test results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable terminal output (default)
    Text,
    /// JUnit XML for CI systems (GitHub Actions, GitLab CI, Jenkins)
    Junit,
    /// JSON Lines — one JSON object per test run
    Jsonl,
}

/// Aggregated test session for output formatting.
pub struct TestSession {
    pub network: String,
    pub players: u8,
    #[allow(dead_code)]
    pub vm_count: usize,
    pub runs: Vec<RunResult>,
    pub total_duration: Duration,
    pub timestamp: String,
}

/// A single run within a test session.
pub struct RunResult {
    pub run_number: u32,
    pub status: RunStatus,
    pub exit_code: Option<i32>,
    pub exit_label: String,
    pub duration: Duration,
}

/// Run outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutKind {
    HeartbeatStall,
    NoProgressTimeout,
    HardTimeout,
}

impl TimeoutKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::HeartbeatStall => "HEARTBEAT_STALL",
            Self::NoProgressTimeout => "NO_PROGRESS_TIMEOUT",
            Self::HardTimeout => "HARD_TIMEOUT",
        }
    }

    fn json_status(self) -> &'static str {
        match self {
            Self::HeartbeatStall => "heartbeat_stall",
            Self::NoProgressTimeout => "no_progress_timeout",
            Self::HardTimeout => "hard_timeout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Pass,
    Fail,
    Timeout(TimeoutKind),
}

impl TestSession {
    pub fn new(network: &str, players: u8, vm_count: usize) -> Self {
        Self {
            network: network.to_string(),
            players,
            vm_count,
            runs: Vec::new(),
            total_duration: Duration::ZERO,
            timestamp: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        }
    }

    pub fn failed(&self) -> u32 {
        self.runs
            .iter()
            .filter(|r| r.status != RunStatus::Pass)
            .count() as u32
    }
}

/// Emit JUnit XML from a test session.
pub fn emit_junit(session: &TestSession) -> String {
    let tests = session.runs.len();
    let failures = session.failed();
    let time = session.total_duration.as_secs_f64();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"cluster-test\" tests=\"{tests}\" failures=\"{failures}\" time=\"{time:.1}\">\n"
    ));

    for run in &session.runs {
        let classname = format!("cluster-test.{}", session.network);
        let name = format!("run-{}", run.run_number);
        let run_time = run.duration.as_secs_f64();

        xml.push_str(&format!(
            "  <testcase classname=\"{}\" name=\"{}\" time=\"{run_time:.1}\"",
            xml_escape(&classname),
            xml_escape(&name),
        ));

        match run.status {
            RunStatus::Pass => xml.push_str(" />\n"),
            RunStatus::Fail => {
                xml.push_str(">\n");
                let msg = match run.exit_code {
                    Some(c) => format!("exit {} ({})", c, run.exit_label),
                    None => "signal".to_string(),
                };
                xml.push_str(&format!(
                    "    <failure message=\"{}\" />\n",
                    xml_escape(&msg)
                ));
                xml.push_str("  </testcase>\n");
            }
            RunStatus::Timeout(kind) => {
                xml.push_str(">\n");
                xml.push_str(&format!("    <failure message=\"{}\" />\n", kind.label()));
                xml.push_str("  </testcase>\n");
            }
        }
    }

    xml.push_str("</testsuite>\n");
    xml
}

#[derive(Serialize)]
struct JsonlRun {
    run: u32,
    status: &'static str,
    exit_code: Option<i32>,
    exit_label: String,
    duration_secs: f64,
    timestamp: String,
    network: String,
    players: u8,
}

/// Emit JSON Lines from a test session.
pub fn emit_jsonl(session: &TestSession) -> String {
    let mut out = String::new();
    for run in &session.runs {
        let entry = JsonlRun {
            run: run.run_number,
            status: match run.status {
                RunStatus::Pass => "pass",
                RunStatus::Fail => "fail",
                RunStatus::Timeout(kind) => kind.json_status(),
            },
            exit_code: run.exit_code,
            exit_label: run.exit_label.clone(),
            duration_secs: run.duration.as_secs_f64(),
            timestamp: session.timestamp.clone(),
            network: session.network.clone(),
            players: session.players,
        };
        if let Ok(json) = serde_json::to_string(&entry) {
            out.push_str(&json);
            out.push('\n');
        }
    }
    out
}

/// Emit JUnit XML from history results (durations unavailable, set to 0).
pub fn emit_junit_from_history(
    results: &[crate::history::TestResult],
    exit_code_map: &std::collections::HashMap<i32, String>,
) -> String {
    let total_runs: u32 = results.iter().map(|r| r.passed + r.failed).sum();
    let total_failures: u32 = results.iter().map(|r| r.failed).sum();

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"cluster-test-history\" tests=\"{total_runs}\" failures=\"{total_failures}\" time=\"0\">\n"
    ));

    for r in results {
        for (i, code) in r.exit_codes.iter().enumerate() {
            let name = format!("{}-run-{}", r.timestamp, i + 1);
            let classname = format!("cluster-test.{}", r.network);
            xml.push_str(&format!(
                "  <testcase classname=\"{}\" name=\"{}\" time=\"0\"",
                xml_escape(&classname),
                xml_escape(&name),
            ));
            match code {
                Some(0) | None => xml.push_str(" />\n"),
                Some(c) => {
                    let label = crate::test::exit_code_label(*c, exit_code_map);
                    xml.push_str(">\n");
                    xml.push_str(&format!(
                        "    <failure message=\"exit {} ({})\" />\n",
                        c,
                        xml_escape(&label)
                    ));
                    xml.push_str("  </testcase>\n");
                }
            }
        }
    }

    xml.push_str("</testsuite>\n");
    xml
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junit_basic() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_secs(10),
        });
        session.runs.push(RunResult {
            run_number: 2,
            status: RunStatus::Fail,
            exit_code: Some(11),
            exit_label: "DAG_VIOLATION".into(),
            duration: Duration::from_secs(5),
        });
        session.total_duration = Duration::from_secs(15);

        let xml = emit_junit(&session);
        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("DAG_VIOLATION"));
    }

    #[test]
    fn jsonl_basic() {
        let mut session = TestSession::new("steam", 4, 3);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_secs(60),
        });

        let jsonl = emit_jsonl(&session);
        let lines: Vec<&str> = jsonl.trim().lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"status\":\"pass\""));
    }

    #[test]
    fn xml_escape_special_chars() {
        assert_eq!(
            xml_escape("a<b>c&d\"e'f"),
            "a&lt;b&gt;c&amp;d&quot;e&apos;f"
        );
    }

    #[test]
    fn junit_all_pass() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_secs(10),
        });
        session.total_duration = Duration::from_secs(10);

        let xml = emit_junit(&session);
        assert!(xml.contains("tests=\"1\""));
        assert!(xml.contains("failures=\"0\""));
        assert!(!xml.contains("<failure"));
    }

    #[test]
    fn junit_timeout_run() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Timeout(TimeoutKind::HardTimeout),
            exit_code: None,
            exit_label: "HARD_TIMEOUT".into(),
            duration: Duration::from_secs(300),
        });
        session.total_duration = Duration::from_secs(300);

        let xml = emit_junit(&session);
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("message=\"HARD_TIMEOUT\""));
    }

    #[test]
    fn junit_signal_failure() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Fail,
            exit_code: None,
            exit_label: "SIGNAL".into(),
            duration: Duration::from_secs(5),
        });
        session.total_duration = Duration::from_secs(5);

        let xml = emit_junit(&session);
        assert!(xml.contains("message=\"signal\""));
    }

    #[test]
    fn junit_xml_well_formed() {
        let mut session = TestSession::new("steam", 4, 3);
        for i in 1..=5 {
            session.runs.push(RunResult {
                run_number: i,
                status: if i % 2 == 0 {
                    RunStatus::Fail
                } else {
                    RunStatus::Pass
                },
                exit_code: Some(if i % 2 == 0 { 11 } else { 0 }),
                exit_label: if i % 2 == 0 {
                    "DAG_VIOLATION"
                } else {
                    "SUCCESS"
                }
                .into(),
                duration: Duration::from_secs(i as u64 * 10),
            });
        }
        session.total_duration = Duration::from_secs(150);

        let xml = emit_junit(&session);
        assert!(xml.starts_with("<?xml version=\"1.0\""));
        assert!(xml.contains("<testsuite"));
        assert!(xml.ends_with("</testsuite>\n"));
        assert!(xml.contains("tests=\"5\""));
        assert!(xml.contains("failures=\"2\""));
    }

    #[test]
    fn jsonl_multiple_runs() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_secs(10),
        });
        session.runs.push(RunResult {
            run_number: 2,
            status: RunStatus::Fail,
            exit_code: Some(12),
            exit_label: "DESYNC".into(),
            duration: Duration::from_secs(5),
        });
        session.runs.push(RunResult {
            run_number: 3,
            status: RunStatus::Timeout(TimeoutKind::NoProgressTimeout),
            exit_code: None,
            exit_label: "NO_PROGRESS_TIMEOUT".into(),
            duration: Duration::from_secs(300),
        });

        let jsonl = emit_jsonl(&session);
        let lines: Vec<&str> = jsonl.trim().lines().collect();
        assert_eq!(lines.len(), 3);

        // Verify each line is valid JSON
        for line in &lines {
            assert!(
                serde_json::from_str::<serde_json::Value>(line).is_ok(),
                "Invalid JSON: {line}"
            );
        }

        assert!(lines[0].contains("\"status\":\"pass\""));
        assert!(lines[1].contains("\"status\":\"fail\""));
        assert!(lines[1].contains("\"exit_code\":12"));
        assert!(lines[2].contains("\"status\":\"no_progress_timeout\""));
    }

    #[test]
    fn jsonl_preserves_metadata() {
        let mut session = TestSession::new("steam", 8, 7);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_secs(60),
        });

        let jsonl = emit_jsonl(&session);
        let parsed: serde_json::Value = serde_json::from_str(jsonl.trim()).unwrap();
        assert_eq!(parsed["network"], "steam");
        assert_eq!(parsed["players"], 8);
        assert_eq!(parsed["run"], 1);
    }

    #[test]
    fn session_failed_count() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_secs(1),
        });
        session.runs.push(RunResult {
            run_number: 2,
            status: RunStatus::Fail,
            exit_code: Some(11),
            exit_label: "DAG".into(),
            duration: Duration::from_secs(1),
        });
        session.runs.push(RunResult {
            run_number: 3,
            status: RunStatus::Timeout(TimeoutKind::HeartbeatStall),
            exit_code: None,
            exit_label: "TO".into(),
            duration: Duration::from_secs(1),
        });
        assert_eq!(session.failed(), 2); // Fail + Timeout
    }

    #[test]
    fn junit_from_history_basic() {
        use std::collections::HashMap;
        let results = vec![crate::history::TestResult {
            timestamp: "2024-01-01T00:00:00".into(),
            network: "lan".into(),
            players: 2,
            vm_count: 1,
            run_number: 1,
            max_runs: 2,
            passed: 1,
            failed: 1,
            timed_out: 0,
            timeout_secs: 300,
            exit_codes: vec![Some(0), Some(11)],
            git_sha: None,
            duration_secs: None,
            run_durations: None,
        }];
        let empty = HashMap::new();
        let xml = emit_junit_from_history(&results, &empty);
        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("DAG_VIOLATION"));
    }

    #[test]
    fn xml_escape_no_change() {
        assert_eq!(xml_escape("hello world"), "hello world");
    }

    #[test]
    fn empty_session() {
        let session = TestSession::new("lan", 2, 1);
        let xml = emit_junit(&session);
        assert!(xml.contains("tests=\"0\""));
        assert!(xml.contains("failures=\"0\""));

        let jsonl = emit_jsonl(&session);
        assert!(jsonl.is_empty());
    }

    #[test]
    fn junit_classname_uses_network() {
        let mut session = TestSession::new("steam", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_secs(1),
        });
        let xml = emit_junit(&session);
        assert!(xml.contains("classname=\"cluster-test.steam\""));
    }

    #[test]
    fn junit_run_names_sequential() {
        let mut session = TestSession::new("lan", 2, 1);
        for i in 1..=3 {
            session.runs.push(RunResult {
                run_number: i,
                status: RunStatus::Pass,
                exit_code: Some(0),
                exit_label: "SUCCESS".into(),
                duration: Duration::from_secs(1),
            });
        }
        let xml = emit_junit(&session);
        assert!(xml.contains("name=\"run-1\""));
        assert!(xml.contains("name=\"run-2\""));
        assert!(xml.contains("name=\"run-3\""));
    }

    #[test]
    fn junit_time_attribute_present() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_millis(1500),
        });
        session.total_duration = Duration::from_millis(1500);
        let xml = emit_junit(&session);
        assert!(xml.contains("time=\"1.5\""));
    }

    #[test]
    fn junit_escape_in_failure_message() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Fail,
            exit_code: Some(99),
            exit_label: "ERR<&>".into(),
            duration: Duration::from_secs(1),
        });
        session.total_duration = Duration::from_secs(1);
        let xml = emit_junit(&session);
        assert!(xml.contains("ERR&lt;&amp;&gt;"));
        assert!(!xml.contains("ERR<&>"));
    }

    #[test]
    fn jsonl_timeout_has_null_exit_code() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Timeout(TimeoutKind::HardTimeout),
            exit_code: None,
            exit_label: "HARD_TIMEOUT".into(),
            duration: Duration::from_secs(300),
        });
        let jsonl = emit_jsonl(&session);
        let parsed: serde_json::Value = serde_json::from_str(jsonl.trim()).unwrap();
        assert!(parsed["exit_code"].is_null());
        assert_eq!(parsed["status"], "hard_timeout");
    }

    #[test]
    fn jsonl_duration_is_float() {
        let mut session = TestSession::new("lan", 2, 1);
        session.runs.push(RunResult {
            run_number: 1,
            status: RunStatus::Pass,
            exit_code: Some(0),
            exit_label: "SUCCESS".into(),
            duration: Duration::from_millis(1234),
        });
        let jsonl = emit_jsonl(&session);
        let parsed: serde_json::Value = serde_json::from_str(jsonl.trim()).unwrap();
        let dur = parsed["duration_secs"].as_f64().unwrap();
        assert!((dur - 1.234).abs() < 0.001);
    }

    #[test]
    fn junit_from_history_all_pass() {
        use std::collections::HashMap;
        let results = vec![crate::history::TestResult {
            timestamp: "2024-01-01T00:00:00".into(),
            network: "lan".into(),
            players: 2,
            vm_count: 1,
            run_number: 1,
            max_runs: 2,
            passed: 2,
            failed: 0,
            timed_out: 0,
            timeout_secs: 300,
            exit_codes: vec![Some(0), Some(0)],
            git_sha: None,
            duration_secs: None,
            run_durations: None,
        }];
        let empty = HashMap::new();
        let xml = emit_junit_from_history(&results, &empty);
        assert!(xml.contains("failures=\"0\""));
        assert!(!xml.contains("<failure"));
    }

    #[test]
    fn junit_from_history_empty() {
        use std::collections::HashMap;
        let empty_map = HashMap::new();
        let xml = emit_junit_from_history(&[], &empty_map);
        assert!(xml.contains("tests=\"0\""));
        assert!(xml.contains("failures=\"0\""));
    }

    #[test]
    fn junit_from_history_with_none_codes() {
        use std::collections::HashMap;
        let results = vec![crate::history::TestResult {
            timestamp: "2024-01-01T00:00:00".into(),
            network: "lan".into(),
            players: 2,
            vm_count: 1,
            run_number: 1,
            max_runs: 2,
            passed: 1,
            failed: 1,
            timed_out: 0,
            timeout_secs: 300,
            exit_codes: vec![Some(0), None],
            git_sha: None,
            duration_secs: None,
            run_durations: None,
        }];
        let empty = HashMap::new();
        let xml = emit_junit_from_history(&results, &empty);
        // None code should be treated as pass (no failure element)
        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("failures=\"1\""));
    }

    #[test]
    fn junit_from_history_custom_exit_codes() {
        use std::collections::HashMap;
        let results = vec![crate::history::TestResult {
            timestamp: "2024-01-01T00:00:00".into(),
            network: "lan".into(),
            players: 2,
            vm_count: 1,
            run_number: 1,
            max_runs: 1,
            passed: 0,
            failed: 1,
            timed_out: 0,
            timeout_secs: 300,
            exit_codes: vec![Some(42)],
            git_sha: None,
            duration_secs: None,
            run_durations: None,
        }];
        let mut custom = HashMap::new();
        custom.insert(42, "CUSTOM_FAIL".to_string());
        let xml = emit_junit_from_history(&results, &custom);
        assert!(xml.contains("CUSTOM_FAIL"));
    }

    #[test]
    fn junit_from_history_multiple_sessions() {
        use std::collections::HashMap;
        let results = vec![
            crate::history::TestResult {
                timestamp: "2024-01-01T00:00:00".into(),
                network: "lan".into(),
                players: 2,
                vm_count: 1,
                run_number: 1,
                max_runs: 1,
                passed: 1,
                failed: 0,
                timed_out: 0,
                timeout_secs: 300,
                exit_codes: vec![Some(0)],
                git_sha: None,
                duration_secs: None,
                run_durations: None,
            },
            crate::history::TestResult {
                timestamp: "2024-01-02T00:00:00".into(),
                network: "steam".into(),
                players: 4,
                vm_count: 3,
                run_number: 1,
                max_runs: 2,
                passed: 1,
                failed: 1,
                timed_out: 0,
                timeout_secs: 300,
                exit_codes: vec![Some(0), Some(11)],
                git_sha: None,
                duration_secs: None,
                run_durations: None,
            },
        ];
        let empty = HashMap::new();
        let xml = emit_junit_from_history(&results, &empty);
        assert!(xml.contains("tests=\"3\"")); // 1 + 2
        assert!(xml.contains("failures=\"1\""));
    }
}
