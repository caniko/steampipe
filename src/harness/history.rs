use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunVisualSummary {
    pub passed: usize,
    pub failed: usize,
}

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
    #[serde(default)]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub run_durations: Option<Vec<f64>>,
    #[serde(default)]
    pub visual_results: Vec<crate::game::visual::VisualRunResult>,
    #[serde(default)]
    pub gpu_preflight: Vec<crate::vm::preflight::GpuPreflightReport>,
    #[serde(default)]
    pub readiness: Vec<crate::game::compositor::CompositorStatusRow>,
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
    git_sha: Option<String>,
    duration_secs: f64,
    run_durations: Vec<f64>,
    visual_results: Vec<crate::game::visual::VisualRunResult>,
    gpu_preflight: Vec<crate::vm::preflight::GpuPreflightReport>,
    readiness: Vec<crate::game::compositor::CompositorStatusRow>,
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
        git_sha,
        duration_secs: Some(duration_secs),
        run_durations: Some(run_durations),
        visual_results,
        gpu_preflight,
        readiness,
    }
}

pub fn summarize_visual_results(
    visual_results: &[crate::game::visual::VisualRunResult],
) -> Option<RunVisualSummary> {
    if visual_results.is_empty() {
        return None;
    }
    Some(RunVisualSummary {
        passed: visual_results.iter().filter(|result| result.passed).count(),
        failed: visual_results
            .iter()
            .filter(|result| !result.passed)
            .count(),
    })
}

fn format_visual_cell(visual_results: &[crate::game::visual::VisualRunResult]) -> String {
    match summarize_visual_results(visual_results) {
        Some(summary) if summary.failed == 0 => {
            format!("PASS ({}/{})", summary.passed, summary.passed)
        }
        Some(summary) => format!(
            "FAIL ({}/{})",
            summary.failed,
            summary.passed + summary.failed
        ),
        None => "─".to_string(),
    }
}

/// Display test history.
pub fn show(
    state_dir: &Path,
    last_n: Option<usize>,
    exit_code_map: &HashMap<i32, String>,
) -> anyhow::Result<()> {
    let history = load(state_dir);

    if history.results.is_empty() {
        println!("No test history found.");
        return Ok(());
    }

    let results = slice_results(&history.results, last_n);

    println!(
        "{:<20} {:<7} {:<4} {:<4} {:<6} {:<6} {:<6} {:<8} {:<12} {:<10}",
        "Timestamp", "Net", "P", "VMs", "Pass", "Fail", "T/O", "SHA", "Visual", "Result"
    );
    println!("{}", "─".repeat(92));

    for r in results {
        let result_str = if r.failed == 0 { "PASS" } else { "FAIL" };
        let sha = r.git_sha.as_deref().unwrap_or("─");
        println!(
            "{:<20} {:<7} {:<4} {:<4} {:<6} {:<6} {:<6} {:<8} {:<12} {:<10}",
            r.timestamp,
            r.network,
            r.players,
            r.vm_count,
            r.passed,
            r.failed,
            r.timed_out,
            sha,
            format_visual_cell(&r.visual_results),
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
    print_failure_breakdown(results, exit_code_map);

    Ok(())
}

fn print_failure_breakdown(results: &[TestResult], exit_code_map: &HashMap<i32, String>) {
    let mut code_counts = HashMap::new();
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
        codes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (code, count) in codes {
            println!(
                "    exit {code} ({}): {count}x",
                crate::test::exit_code_label(code, exit_code_map)
            );
        }
    }
}

fn slice_results(results: &[TestResult], last_n: Option<usize>) -> &[TestResult] {
    if let Some(n) = last_n {
        let start = results.len().saturating_sub(n);
        &results[start..]
    } else {
        results
    }
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

// ── Trend Analysis ──

/// Regression alert: an exit code whose frequency increased between windows.
pub struct RegressionAlert {
    pub exit_code: i32,
    pub label: String,
    pub recent_count: u32,
    pub previous_count: u32,
    pub multiplier: f64,
}

/// Show trend analysis comparing recent vs previous windows.
pub fn show_trends(
    state_dir: &Path,
    window: usize,
    last_n: Option<usize>,
    exit_code_map: &HashMap<i32, String>,
) -> anyhow::Result<()> {
    let history = load(state_dir);
    if history.results.is_empty() {
        println!("No test history found.");
        return Ok(());
    }

    let results = slice_results(&history.results, last_n);
    let total_runs: u32 = results.iter().map(|r| r.passed + r.failed).sum();
    let total_passed: u32 = results.iter().map(|r| r.passed).sum();
    let overall_pass_rate = if total_runs > 0 {
        total_passed as f64 / total_runs as f64 * 100.0
    } else {
        0.0
    };

    println!("=== TREND ANALYSIS ===\n");
    println!("Overall: {total_passed}/{total_runs} passed ({overall_pass_rate:.1}% pass rate)\n");

    if results.len() < window {
        println!(
            "Not enough history for window comparison (have {}, need {window}).",
            results.len()
        );
        print_failure_breakdown(results, exit_code_map);
        return Ok(());
    }

    let mid = results.len().saturating_sub(window);
    let recent = &results[mid..];
    let previous_start = mid.saturating_sub(window);
    let previous = &results[previous_start..mid];

    let recent_runs: u32 = recent.iter().map(|r| r.passed + r.failed).sum();
    let recent_passed: u32 = recent.iter().map(|r| r.passed).sum();
    let recent_rate = if recent_runs > 0 {
        recent_passed as f64 / recent_runs as f64 * 100.0
    } else {
        0.0
    };

    let prev_runs: u32 = previous.iter().map(|r| r.passed + r.failed).sum();
    let prev_passed: u32 = previous.iter().map(|r| r.passed).sum();
    let prev_rate = if prev_runs > 0 {
        prev_passed as f64 / prev_runs as f64 * 100.0
    } else {
        0.0
    };

    let trend = if recent_rate > prev_rate + 5.0 {
        "IMPROVING"
    } else if recent_rate < prev_rate - 5.0 {
        "DEGRADING"
    } else {
        "STABLE"
    };

    println!("Recent {window} sessions:   {recent_passed}/{recent_runs} ({recent_rate:.1}%)");
    println!("Previous {window} sessions: {prev_passed}/{prev_runs} ({prev_rate:.1}%)");
    println!("Trend: {trend}\n");

    // Detect regressions
    let recent_codes = count_failure_codes(recent);
    let prev_codes = count_failure_codes(previous);

    let mut regressions: Vec<RegressionAlert> = Vec::new();
    for (&code, &recent_count) in &recent_codes {
        let prev_count = prev_codes.get(&code).copied().unwrap_or(0);
        let multiplier = if prev_count == 0 {
            f64::INFINITY
        } else {
            recent_count as f64 / prev_count as f64
        };
        if multiplier >= 2.0 || (prev_count == 0 && recent_count > 0) {
            regressions.push(RegressionAlert {
                exit_code: code,
                label: crate::test::exit_code_label(code, exit_code_map).into_owned(),
                recent_count,
                previous_count: prev_count,
                multiplier,
            });
        }
    }

    if regressions.is_empty() {
        println!("No regressions detected.");
    } else {
        regressions.sort_by(|a, b| {
            b.multiplier
                .partial_cmp(&a.multiplier)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        println!("Regressions:");
        for r in &regressions {
            let mult = if r.multiplier.is_infinite() {
                "NEW".to_string()
            } else {
                format!("{:.1}x", r.multiplier)
            };
            println!(
                "  exit {} ({}): {} in recent vs {} in previous ({mult})",
                r.exit_code, r.label, r.recent_count, r.previous_count,
            );
        }
    }

    Ok(())
}

fn count_failure_codes(results: &[TestResult]) -> HashMap<i32, u32> {
    let mut counts = HashMap::new();
    for r in results {
        for c in r.exit_codes.iter().flatten() {
            if *c != 0 {
                *counts.entry(*c).or_insert(0) += 1;
            }
        }
    }
    counts
}

// ── Flaky Test Detection ──

/// Flakiness classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlakPattern {
    /// Appears in >80% of failing runs
    Consistent,
    /// Appears in 20-80% of runs
    Intermittent,
    /// Appears in <20% of runs
    Rare,
}

/// A flakiness entry for a specific exit code.
pub struct FlakEntry {
    pub exit_code: i32,
    pub label: String,
    pub occurrences: u32,
    pub total_runs: u32,
    pub flakiness_score: f64,
    pub pattern: FlakPattern,
}

/// Compute flakiness from a set of exit codes (e.g., from a single test session).
pub fn compute_flakiness(
    exit_codes: &[Option<i32>],
    exit_code_map: &HashMap<i32, String>,
) -> Vec<FlakEntry> {
    let total = exit_codes.len() as u32;
    if total == 0 {
        return Vec::new();
    }

    let mut code_counts: HashMap<i32, u32> = HashMap::new();
    for code in exit_codes.iter().flatten() {
        if *code != 0 {
            *code_counts.entry(*code).or_insert(0) += 1;
        }
    }

    code_counts
        .into_iter()
        .map(|(code, count)| {
            let score = count as f64 / total as f64;
            let pattern = if score > 0.8 {
                FlakPattern::Consistent
            } else if score >= 0.2 {
                FlakPattern::Intermittent
            } else {
                FlakPattern::Rare
            };
            FlakEntry {
                exit_code: code,
                label: crate::test::exit_code_label(code, exit_code_map).into_owned(),
                occurrences: count,
                total_runs: total,
                flakiness_score: score,
                pattern,
            }
        })
        .collect()
}

/// Show flaky test detection from history.
pub fn show_flakiness(
    state_dir: &Path,
    last_n: Option<usize>,
    exit_code_map: &HashMap<i32, String>,
) -> anyhow::Result<()> {
    let history = load(state_dir);
    if history.results.is_empty() {
        println!("No test history found.");
        return Ok(());
    }

    let results = slice_results(&history.results, last_n);

    // Collect all exit codes across results
    let all_codes: Vec<Option<i32>> = results
        .iter()
        .flat_map(|r| r.exit_codes.iter().copied())
        .collect();
    let entries = compute_flakiness(&all_codes, exit_code_map);

    if entries.is_empty() {
        println!("No failures found in history.");
        return Ok(());
    }

    println!("=== FLAKY TEST DETECTION ===\n");
    println!(
        "{:<8} {:<22} {:<8} {:<8} {:<8} {:<12}",
        "Code", "Label", "Count", "Total", "Score", "Pattern"
    );
    println!("{}", "─".repeat(66));

    let mut sorted = entries;
    sorted.sort_by(|a, b| {
        b.flakiness_score
            .partial_cmp(&a.flakiness_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for e in &sorted {
        let pattern_str = match e.pattern {
            FlakPattern::Consistent => "CONSISTENT",
            FlakPattern::Intermittent => "FLAKY",
            FlakPattern::Rare => "RARE",
        };
        println!(
            "{:<8} {:<22} {:<8} {:<8} {:<8.0}% {:<12}",
            e.exit_code,
            e.label,
            e.occurrences,
            e.total_runs,
            e.flakiness_score * 100.0,
            pattern_str,
        );
    }

    Ok(())
}

// ── Export ──

/// Export history in a structured format.
pub fn export(
    state_dir: &Path,
    last_n: Option<usize>,
    format: crate::harness::output::OutputFormat,
    output: Option<&Path>,
    exit_code_map: &HashMap<i32, String>,
) -> anyhow::Result<()> {
    let history = load(state_dir);
    if history.results.is_empty() {
        println!("No test history found.");
        return Ok(());
    }

    let results = slice_results(&history.results, last_n);

    let content = match format {
        crate::harness::output::OutputFormat::Junit => {
            crate::harness::output::emit_junit_from_history(results, exit_code_map)
        }
        crate::harness::output::OutputFormat::Jsonl => {
            let mut out = String::new();
            for r in results {
                if let Ok(json) = serde_json::to_string(r) {
                    out.push_str(&json);
                    out.push('\n');
                }
            }
            out
        }
        crate::harness::output::OutputFormat::Text => {
            show(state_dir, last_n, exit_code_map)?;
            return Ok(());
        }
    };

    match output {
        Some(path) => {
            std::fs::write(path, &content)?;
            println!("Exported to {}", path.display());
        }
        None => print!("{content}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_result(passed: u32, failed: u32, exit_codes: Vec<Option<i32>>) -> TestResult {
        TestResult {
            timestamp: "2024-01-01T00:00:00".into(),
            network: "lan".into(),
            players: 2,
            vm_count: 1,
            run_number: 1,
            max_runs: passed + failed,
            passed,
            failed,
            timed_out: 0,
            timeout_secs: 300,
            exit_codes,
            git_sha: None,
            duration_secs: None,
            run_durations: None,
            visual_results: Vec::new(),
            gpu_preflight: Vec::new(),
            readiness: Vec::new(),
        }
    }

    // ── Flakiness tests ─────────────────────────────────────────────────

    #[test]
    fn flakiness_scoring() {
        let codes = vec![Some(11), Some(0), Some(11), Some(0), Some(12)];
        let empty = HashMap::new();
        let mut entries = compute_flakiness(&codes, &empty);
        entries.sort_by_key(|e| e.exit_code);

        assert_eq!(entries.len(), 2);
        let e11 = entries.iter().find(|e| e.exit_code == 11).unwrap();
        assert_eq!(e11.occurrences, 2);
        assert!(matches!(e11.pattern, FlakPattern::Intermittent));
        let e12 = entries.iter().find(|e| e.exit_code == 12).unwrap();
        assert_eq!(e12.occurrences, 1);
        assert!(matches!(e12.pattern, FlakPattern::Intermittent));
    }

    #[test]
    fn flakiness_consistent() {
        let codes = vec![Some(11), Some(11), Some(11), Some(11), Some(11)];
        let empty = HashMap::new();
        let entries = compute_flakiness(&codes, &empty);
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].pattern, FlakPattern::Consistent));
    }

    #[test]
    fn flakiness_rare() {
        let codes: Vec<Option<i32>> = (0..10)
            .map(|i| if i == 0 { Some(11) } else { Some(0) })
            .collect();
        let empty = HashMap::new();
        let entries = compute_flakiness(&codes, &empty);
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].pattern, FlakPattern::Rare));
    }

    #[test]
    fn flakiness_empty_codes() {
        let empty = HashMap::new();
        let entries = compute_flakiness(&[], &empty);
        assert!(entries.is_empty());
    }

    #[test]
    fn flakiness_all_success_no_entries() {
        let codes = vec![Some(0), Some(0), Some(0)];
        let empty = HashMap::new();
        let entries = compute_flakiness(&codes, &empty);
        assert!(entries.is_empty());
    }

    #[test]
    fn flakiness_uses_custom_labels() {
        let codes = vec![Some(42), Some(0), Some(42)];
        let mut custom = HashMap::new();
        custom.insert(42, "MY_ERROR".to_string());
        let entries = compute_flakiness(&codes, &custom);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "MY_ERROR");
    }

    #[test]
    fn flakiness_boundary_20_percent() {
        // 1/5 = 20% → boundary of Intermittent (>= 0.2)
        let codes = vec![Some(11), Some(0), Some(0), Some(0), Some(0)];
        let empty = HashMap::new();
        let entries = compute_flakiness(&codes, &empty);
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].pattern, FlakPattern::Intermittent));
    }

    #[test]
    fn flakiness_boundary_just_below_20_percent() {
        // 1/6 ≈ 16.7% → Rare
        let codes = vec![Some(11), Some(0), Some(0), Some(0), Some(0), Some(0)];
        let empty = HashMap::new();
        let entries = compute_flakiness(&codes, &empty);
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].pattern, FlakPattern::Rare));
    }

    // ── count_failure_codes tests ───────────────────────────────────────

    #[test]
    fn count_failure_codes_basic() {
        let results = vec![
            make_test_result(1, 2, vec![Some(0), Some(11), Some(12)]),
            make_test_result(0, 1, vec![Some(11)]),
        ];
        let counts = count_failure_codes(&results);
        assert_eq!(counts.get(&11), Some(&2));
        assert_eq!(counts.get(&12), Some(&1));
        assert!(!counts.contains_key(&0));
    }

    #[test]
    fn count_failure_codes_empty() {
        let counts = count_failure_codes(&[]);
        assert!(counts.is_empty());
    }

    #[test]
    fn count_failure_codes_ignores_success() {
        let results = vec![make_test_result(3, 0, vec![Some(0), Some(0), Some(0)])];
        let counts = count_failure_codes(&results);
        assert!(counts.is_empty());
    }

    // ── slice_results tests ─────────────────────────────────────────────

    #[test]
    fn slice_results_none_returns_all() {
        let results = vec![
            make_test_result(1, 0, vec![Some(0)]),
            make_test_result(1, 0, vec![Some(0)]),
        ];
        assert_eq!(slice_results(&results, None).len(), 2);
    }

    #[test]
    fn slice_results_with_n() {
        let results = vec![
            make_test_result(1, 0, vec![Some(0)]),
            make_test_result(1, 0, vec![Some(0)]),
            make_test_result(1, 0, vec![Some(0)]),
        ];
        assert_eq!(slice_results(&results, Some(2)).len(), 2);
    }

    #[test]
    fn slice_results_n_exceeds_length() {
        let results = vec![make_test_result(1, 0, vec![Some(0)])];
        assert_eq!(slice_results(&results, Some(100)).len(), 1);
    }

    // ── make_result tests ───────────────────────────────────────────────

    #[test]
    fn make_result_includes_new_fields() {
        let result = make_result(
            "lan",
            2,
            1,
            1,
            1,
            1,
            0,
            0,
            300,
            vec![Some(0)],
            Some("abc1234".into()),
            15.5,
            vec![15.5],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(result.git_sha.as_deref(), Some("abc1234"));
        assert_eq!(result.duration_secs, Some(15.5));
        assert_eq!(result.run_durations.as_ref().unwrap().len(), 1);
    }

    // ── Backward-compatible deserialization ──────────────────────────────

    #[test]
    fn deserialize_old_format_without_new_fields() {
        let json = r#"{
            "timestamp": "2024-01-01T00:00:00",
            "network": "lan",
            "players": 2,
            "vm_count": 1,
            "run_number": 1,
            "max_runs": 1,
            "passed": 1,
            "failed": 0,
            "timed_out": 0,
            "timeout_secs": 300,
            "exit_codes": [0]
        }"#;
        let result: TestResult = serde_json::from_str(json).unwrap();
        assert!(result.git_sha.is_none());
        assert!(result.duration_secs.is_none());
        assert!(result.run_durations.is_none());
    }

    #[test]
    fn deserialize_new_format_with_all_fields() {
        let json = r#"{
            "timestamp": "2024-01-01T00:00:00",
            "network": "steam",
            "players": 4,
            "vm_count": 3,
            "run_number": 5,
            "max_runs": 10,
            "passed": 3,
            "failed": 2,
            "timed_out": 1,
            "timeout_secs": 600,
            "exit_codes": [0, 11, null],
            "git_sha": "abc1234",
            "duration_secs": 120.5,
            "run_durations": [40.1, 35.2, 45.2]
        }"#;
        let result: TestResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.git_sha.as_deref(), Some("abc1234"));
        assert_eq!(result.duration_secs, Some(120.5));
        assert_eq!(result.run_durations.as_ref().unwrap().len(), 3);
    }

    // ── Multiple failure codes ────────────────────────────────────────

    #[test]
    fn flakiness_multiple_codes_mixed() {
        // 10 runs: 5x code 11, 3x code 12, 2x code 0
        let codes = vec![
            Some(11),
            Some(11),
            Some(11),
            Some(11),
            Some(11),
            Some(12),
            Some(12),
            Some(12),
            Some(0),
            Some(0),
        ];
        let empty = HashMap::new();
        let mut entries = compute_flakiness(&codes, &empty);
        entries.sort_by_key(|e| e.exit_code);

        assert_eq!(entries.len(), 2);
        // code 11: 5/10 = 50% → Intermittent
        let e11 = entries.iter().find(|e| e.exit_code == 11).unwrap();
        assert_eq!(e11.occurrences, 5);
        assert!((e11.flakiness_score - 0.5).abs() < 0.01);
        assert!(matches!(e11.pattern, FlakPattern::Intermittent));
        // code 12: 3/10 = 30% → Intermittent
        let e12 = entries.iter().find(|e| e.exit_code == 12).unwrap();
        assert_eq!(e12.occurrences, 3);
        assert!(matches!(e12.pattern, FlakPattern::Intermittent));
    }

    #[test]
    fn flakiness_with_none_codes() {
        // None codes (signal kills) should be skipped
        let codes = vec![Some(11), None, Some(0), None];
        let empty = HashMap::new();
        let entries = compute_flakiness(&codes, &empty);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].exit_code, 11);
        // total is 4, occurrences is 1 → 25% Intermittent
        assert_eq!(entries[0].total_runs, 4);
        assert_eq!(entries[0].occurrences, 1);
        assert!(matches!(entries[0].pattern, FlakPattern::Intermittent));
    }

    // ── count_failure_codes across sessions ─────────────────────────────

    #[test]
    fn count_failure_codes_multiple_sessions() {
        let results = vec![
            make_test_result(2, 1, vec![Some(0), Some(0), Some(11)]),
            make_test_result(1, 2, vec![Some(0), Some(12), Some(11)]),
            make_test_result(0, 3, vec![Some(13), Some(11), Some(12)]),
        ];
        let counts = count_failure_codes(&results);
        assert_eq!(counts.get(&11), Some(&3));
        assert_eq!(counts.get(&12), Some(&2));
        assert_eq!(counts.get(&13), Some(&1));
        assert!(!counts.contains_key(&0));
    }

    #[test]
    fn count_failure_codes_with_none_values() {
        let results = vec![make_test_result(0, 1, vec![None, Some(11)])];
        let counts = count_failure_codes(&results);
        assert_eq!(counts.get(&11), Some(&1));
        assert_eq!(counts.len(), 1);
    }

    // ── Serialization roundtrip ─────────────────────────────────────────

    #[test]
    fn test_result_serialization_roundtrip() {
        let result = make_result(
            "steam",
            4,
            3,
            5,
            10,
            3,
            2,
            1,
            600,
            vec![Some(0), Some(11), None],
            Some("deadbeef".into()),
            120.5,
            vec![40.1, 35.2, 45.2],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: TestResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.network, "steam");
        assert_eq!(deserialized.players, 4);
        assert_eq!(deserialized.passed, 3);
        assert_eq!(deserialized.failed, 2);
        assert_eq!(deserialized.git_sha.as_deref(), Some("deadbeef"));
        assert_eq!(deserialized.duration_secs, Some(120.5));
        assert_eq!(deserialized.run_durations.unwrap().len(), 3);
    }

    // ── History save/load roundtrip ─────────────────────────────────────

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("steampipe-test-history-roundtrip");
        let _ = std::fs::create_dir_all(&dir);

        // Clean up from previous test run
        let _ = std::fs::remove_file(dir.join("test_history.json"));

        let result = make_result(
            "lan",
            2,
            1,
            1,
            1,
            1,
            0,
            0,
            300,
            vec![Some(0)],
            Some("deadbeef".into()),
            42.0,
            vec![42.0],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        save_result(&dir, result).unwrap();

        let history = load(&dir);
        assert_eq!(history.results.len(), 1);
        assert_eq!(history.results[0].git_sha.as_deref(), Some("deadbeef"));
        assert_eq!(history.results[0].duration_secs, Some(42.0));

        // Clean up
        let _ = std::fs::remove_file(dir.join("test_history.json"));
    }
}
