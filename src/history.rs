use std::path::{Path, PathBuf};

use chrono::Local;
use serde::{Deserialize, Serialize};

/// A single test run result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub timestamp: String,
    pub network: String,
    pub players: u8,
    pub vm_count: usize,
    pub run_number: u32,
    pub max_runs: u32,
    pub passed: u32,
    pub failed: u32,
    pub timed_out: u32,
    pub timeout_secs: u64,
    pub exit_codes: Vec<Option<i32>>,
}

/// All stored test results.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TestHistory {
    pub results: Vec<TestResult>,
}

fn history_path(state_dir: &Path) -> PathBuf {
    state_dir.join("test_history.json")
}

/// Load test history from state dir.
pub fn load(state_dir: &Path) -> TestHistory {
    let path = history_path(state_dir);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Save a test result to history.
pub fn save_result(state_dir: &Path, result: TestResult) -> anyhow::Result<()> {
    let mut history = load(state_dir);
    history.results.push(result);
    let path = history_path(state_dir);
    std::fs::write(&path, serde_json::to_string_pretty(&history)?)?;
    Ok(())
}

/// Create a TestResult from test run data.
pub fn make_result(
    network: &str,
    players: u8,
    vm_count: usize,
    run_number: u32,
    max_runs: u32,
    passed: u32,
    failed: u32,
    timed_out: u32,
    timeout_secs: u64,
    exit_codes: Vec<Option<i32>>,
) -> TestResult {
    TestResult {
        timestamp: Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        network: network.to_string(),
        players,
        vm_count,
        run_number,
        max_runs,
        passed,
        failed,
        timed_out,
        timeout_secs,
        exit_codes,
    }
}

/// Display test history.
pub fn show(state_dir: &Path, last_n: Option<usize>) -> anyhow::Result<()> {
    let history = load(state_dir);

    if history.results.is_empty() {
        println!("No test history found.");
        return Ok(());
    }

    let results: &[TestResult] = if let Some(n) = last_n {
        let start = history.results.len().saturating_sub(n);
        &history.results[start..]
    } else {
        &history.results
    };

    println!(
        "{:<20} {:<7} {:<4} {:<4} {:<6} {:<6} {:<6} {:<10}",
        "Timestamp", "Net", "P", "VMs", "Pass", "Fail", "T/O", "Result"
    );
    println!("{}", "─".repeat(70));

    for r in results {
        let result_str = if r.failed == 0 { "PASS" } else { "FAIL" };
        println!(
            "{:<20} {:<7} {:<4} {:<4} {:<6} {:<6} {:<6} {:<10}",
            r.timestamp,
            r.network,
            r.players,
            r.vm_count,
            r.passed,
            r.failed,
            r.timed_out,
            result_str,
        );
    }

    // Summary statistics
    let total_runs: u32 = results.iter().map(|r| r.passed + r.failed).sum();
    let total_passed: u32 = results.iter().map(|r| r.passed).sum();
    let total_failed: u32 = results.iter().map(|r| r.failed).sum();
    let sessions = results.len();

    println!();
    println!(
        "  {sessions} session(s), {total_runs} run(s): {total_passed} passed, {total_failed} failed ({:.1}% pass rate)",
        if total_runs > 0 {
            total_passed as f64 / total_runs as f64 * 100.0
        } else {
            0.0
        }
    );

    // Failure code breakdown
    let mut code_counts = std::collections::HashMap::new();
    for r in results {
        for c in r.exit_codes.iter().flatten() {
            if *c != 0 {
                *code_counts.entry(*c).or_insert(0u32) += 1;
            }
        }
    }
    if !code_counts.is_empty() {
        println!("\n  Failure breakdown:");
        let mut codes: Vec<_> = code_counts.into_iter().collect();
        codes.sort_by(|a, b| b.1.cmp(&a.1));
        for (code, count) in codes {
            println!(
                "    exit {code} ({}): {count}x",
                crate::test::exit_code_label(code)
            );
        }
    }

    Ok(())
}

/// Clear test history.
pub fn clear(state_dir: &Path) -> anyhow::Result<()> {
    let path = history_path(state_dir);
    if path.exists() {
        std::fs::remove_file(&path)?;
        println!("Test history cleared.");
    } else {
        println!("No test history to clear.");
    }
    Ok(())
}
