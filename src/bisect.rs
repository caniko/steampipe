//! Binary search git history to find the commit that introduced a test failure.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::cli::{DisplayMode, NetworkMode};
use crate::config::ClusterConfig;
use crate::output::OutputFormat;

/// Configuration for a bisect run.
pub struct BisectConfig {
    pub good_sha: String,
    pub bad_sha: String,
    pub network: NetworkMode,
    pub players: u8,
    pub timeout: Duration,
    pub runs_per_step: u32,
    pub pass_threshold: f64,
    pub display: DisplayMode,
    pub vm_args: Option<String>,
    pub host_args: Option<String>,
}

/// Guard that restores the original git checkout on drop.
struct CheckoutGuard {
    project_root: std::path::PathBuf,
    original_ref: String,
    active: bool,
}

impl Drop for CheckoutGuard {
    fn drop(&mut self) {
        if self.active {
            eprintln!("[bisect] Restoring original checkout: {}", self.original_ref);
            let _ = Command::new("git")
                .args(["checkout", &self.original_ref])
                .current_dir(&self.project_root)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

fn git(project_root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(project_root)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_working_tree_clean(project_root: &Path) -> bool {
    Command::new("git")
        .args(["diff", "--quiet"])
        .current_dir(project_root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(project_root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

/// Run bisection: binary search through commits to find the first bad one.
pub async fn run<S: Send + Sync>(
    config: &ClusterConfig<S>,
    project_root: &Path,
    bisect_config: BisectConfig,
) -> anyhow::Result<()> {
    // Validate SHAs exist
    let good = git(project_root, &["rev-parse", "--verify", &bisect_config.good_sha])?;
    let bad = git(project_root, &["rev-parse", "--verify", &bisect_config.bad_sha])?;

    // Check working tree is clean
    if !is_working_tree_clean(project_root) {
        anyhow::bail!(
            "Working tree has uncommitted changes. Commit or stash them before bisecting."
        );
    }

    // Get current HEAD for restoration
    let original_ref = git(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let original_ref = if original_ref == "HEAD" {
        git(project_root, &["rev-parse", "HEAD"])?
    } else {
        original_ref
    };

    println!("[bisect] Will restore to '{original_ref}' when done");

    let mut guard = CheckoutGuard {
        project_root: project_root.to_path_buf(),
        original_ref: original_ref.clone(),
        active: true,
    };

    // Get commit list between good and bad
    let commit_list_str = git(
        project_root,
        &["rev-list", "--ancestry-path", &format!("{good}..{bad}")],
    )?;
    let mut commits: Vec<&str> = commit_list_str.lines().collect();
    commits.reverse(); // oldest first (good end → bad end)

    if commits.is_empty() {
        anyhow::bail!("No commits found between {good} and {bad}");
    }

    println!(
        "[bisect] Searching {} commits between {} (good) and {} (bad)",
        commits.len(),
        &good[..8.min(good.len())],
        &bad[..8.min(bad.len())],
    );

    let exit_codes: HashMap<i32, String> = config.exit_codes.clone();

    // Binary search
    let mut lo = 0usize;
    let mut hi = commits.len(); // commits[hi-1] is bad
    let mut step = 0u32;

    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        let sha = commits[mid];
        step += 1;

        let short_sha = &sha[..8.min(sha.len())];
        let msg = git(project_root, &["log", "--oneline", "-1", sha]).unwrap_or_default();
        println!("\n[bisect] Step {step}: testing {short_sha} ({msg})");

        // Checkout
        git(project_root, &["checkout", sha])?;

        // Build
        println!("[bisect] Building...");
        let build_result = crate::deploy::build_release(project_root, &config.cargo_package).await;
        if let Err(e) = build_result {
            println!("[bisect] Build FAILED at {short_sha}: {e}");
            println!("[bisect] Treating build failure as BAD");
            hi = mid;
            continue;
        }

        // Deploy
        println!("[bisect] Deploying...");
        let deploy_result = crate::deploy::deploy_to_vms(
            &config.backend,
            &config.vms,
            config,
            project_root,
            false,
        )
        .await;
        if let Err(e) = deploy_result {
            println!("[bisect] Deploy FAILED at {short_sha}: {e}");
            hi = mid;
            continue;
        }

        // Run tests
        let mut pass_count = 0u32;
        for run in 1..=bisect_config.runs_per_step {
            println!("[bisect] Test run {run}/{}", bisect_config.runs_per_step);
            let test_config = crate::test::TestConfig {
                network: bisect_config.network,
                players: bisect_config.players,
                vm_args: bisect_config.vm_args.clone(),
                host_args: bisect_config.host_args.clone(),
                max_runs: 1,
                timeout: bisect_config.timeout,
                shutdown_timeout: Duration::from_secs(5),
                stop_on_failure: true,
                deploy: false, // already deployed
                build: false,  // already built
                filter_pattern: None,
                output_file: None,
                capture_on_failure: false,
                display: bisect_config.display,
                verbose: false,
                chaos: None,
                heartbeat_stall_secs: config.heartbeat_stall_secs,
                output_format: OutputFormat::Text,
                on_complete: None,
                on_failure: None,
                exit_codes: exit_codes.clone(),
            };
            match crate::test::run(config, project_root, test_config).await {
                Ok(_) => pass_count += 1,
                Err(e) => {
                    let msg = format!("{e}");
                    if msg.contains("GPU check failed")
                        || msg.contains("Compositor setup failed")
                    {
                        anyhow::bail!("Bisect aborted — infrastructure failure: {e}");
                    }
                    // Actual test failure — count as bad
                }
            }
        }

        let pass_rate = pass_count as f64 / bisect_config.runs_per_step as f64;
        let is_good = pass_rate >= bisect_config.pass_threshold;

        println!(
            "[bisect] {short_sha}: {pass_count}/{} passed ({:.0}%) → {}",
            bisect_config.runs_per_step,
            pass_rate * 100.0,
            if is_good { "GOOD" } else { "BAD" },
        );

        if is_good {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    // Result
    let first_bad = commits[hi.min(commits.len() - 1)];
    let first_bad_msg = git(project_root, &["log", "--oneline", "-1", first_bad]).unwrap_or_default();

    println!("\n=== BISECT RESULT ===");
    println!("First bad commit: {first_bad_msg}");
    println!("Full SHA: {first_bad}");
    println!("Steps taken: {step}");
    println!("Total commits searched: {}", commits.len());

    // Restore original checkout
    git(project_root, &["checkout", &original_ref])?;
    guard.active = false;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn checkout_guard_inactive_does_not_run() {
        // Guard with active=false should not attempt git checkout on drop
        let guard = CheckoutGuard {
            project_root: PathBuf::from("/nonexistent"),
            original_ref: "main".into(),
            active: false,
        };
        drop(guard); // should not panic
    }

    #[test]
    fn checkout_guard_active_attempts_restore() {
        // Guard with active=true on a non-git dir fails silently (no panic)
        let guard = CheckoutGuard {
            project_root: std::env::temp_dir(),
            original_ref: "main".into(),
            active: true,
        };
        drop(guard); // git checkout fails but Drop doesn't panic
    }

    #[test]
    fn git_helper_on_real_repo() {
        // This test runs on the steampipe repo itself.
        // In a Nix build sandbox there is no .git directory, so skip gracefully.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if !root.join(".git").exists() {
            // Also check parent dirs — Cargo workspaces place .git above CARGO_MANIFEST_DIR
            let has_git = root.ancestors().any(|p| p.join(".git").exists());
            if !has_git {
                return; // no git repo available (e.g. Nix sandbox)
            }
        }
        let result = git(&root, &["rev-parse", "HEAD"]);
        assert!(result.is_ok());
        let sha = result.unwrap();
        assert!(sha.len() >= 7, "SHA should be at least 7 chars: {sha}");
    }

    #[test]
    fn git_helper_bad_command_errors() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // In a Nix build sandbox there is no .git directory; git would fail
        // for a different reason than a bad ref. Skip gracefully.
        let has_git = root.ancestors().any(|p| p.join(".git").exists());
        if !has_git {
            return;
        }
        let result = git(&root, &["rev-parse", "--verify", "nonexistent_ref_zzz"]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("git rev-parse --verify nonexistent_ref_zzz failed"));
    }

    #[test]
    fn git_helper_on_non_repo_errors() {
        let result = git(std::path::Path::new("/tmp"), &["rev-parse", "HEAD"]);
        assert!(result.is_err());
    }

    #[test]
    fn is_working_tree_clean_on_real_repo() {
        // This is informational — the test passes regardless of dirty state
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let _clean = is_working_tree_clean(&root);
        // Just assert it doesn't panic
    }

    #[test]
    fn is_working_tree_clean_non_repo_returns_false() {
        assert!(!is_working_tree_clean(std::path::Path::new("/tmp")));
    }

    #[test]
    fn bisect_config_construction() {
        let config = BisectConfig {
            good_sha: "abc".into(),
            bad_sha: "def".into(),
            network: NetworkMode::Lan,
            players: 2,
            timeout: Duration::from_secs(300),
            runs_per_step: 3,
            pass_threshold: 0.67,
            display: DisplayMode::Headless,
            vm_args: Some("--auto-join".into()),
            host_args: None,
        };
        assert_eq!(config.good_sha, "abc");
        assert_eq!(config.bad_sha, "def");
        assert_eq!(config.runs_per_step, 3);
        assert!((config.pass_threshold - 0.67).abs() < f64::EPSILON);
    }
}
