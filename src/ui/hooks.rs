//! Notification hooks: template expansion and shell execution after test runs.

use std::time::Duration;

/// Variables available for template expansion in hook commands.
pub struct HookVars {
    pub pass_count: u32,
    pub fail_count: u32,
    pub total_count: u32,
    pub timeout_count: u32,
    pub pass_rate: u32,
    pub duration: Duration,
    pub exit_label: String,
    pub network: String,
    pub players: u8,
    pub cluster_name: String,
}

/// Expand template variables in a hook command string.
pub fn expand_template(template: &str, vars: &HookVars) -> String {
    let duration_str = format!("{}m{}s", vars.duration.as_secs() / 60, vars.duration.as_secs() % 60);
    template
        .replace("{pass_count}", &vars.pass_count.to_string())
        .replace("{fail_count}", &vars.fail_count.to_string())
        .replace("{total_count}", &vars.total_count.to_string())
        .replace("{timeout_count}", &vars.timeout_count.to_string())
        .replace("{pass_rate}", &vars.pass_rate.to_string())
        .replace("{duration}", &duration_str)
        .replace("{duration_secs}", &vars.duration.as_secs().to_string())
        .replace("{exit_label}", &vars.exit_label)
        .replace("{network}", &vars.network)
        .replace("{players}", &vars.players.to_string())
        .replace("{cluster_name}", &vars.cluster_name)
}

/// Execute a hook command via `sh -c`. Non-blocking; failure is a warning, not an error.
pub async fn run_hook(command_template: &str, vars: &HookVars) {
    let expanded = expand_template(command_template, vars);
    eprintln!("[cluster-ctl] Running hook: {expanded}");

    let result = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&expanded)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            eprintln!("[cluster-ctl] Hook completed successfully");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "[cluster-ctl] Hook failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            );
        }
        Err(e) => {
            eprintln!("[cluster-ctl] Hook execution error: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_vars() -> HookVars {
        HookVars {
            pass_count: 8,
            fail_count: 2,
            total_count: 10,
            timeout_count: 1,
            pass_rate: 80,
            duration: Duration::from_secs(332),
            exit_label: "DESYNC".into(),
            network: "lan".into(),
            players: 4,
            cluster_name: "1v1".into(),
        }
    }

    #[test]
    fn expand_all_vars() {
        let tpl = "echo {pass_count}/{total_count} passed ({pass_rate}%) in {duration} ({duration_secs}s) - {exit_label} - {network} {players}p - {cluster_name}";
        let result = expand_template(tpl, &test_vars());
        assert_eq!(
            result,
            "echo 8/10 passed (80%) in 5m32s (332s) - DESYNC - lan 4p - 1v1"
        );
    }

    #[test]
    fn expand_no_vars() {
        let result = expand_template("echo hello", &test_vars());
        assert_eq!(result, "echo hello");
    }

    #[test]
    fn expand_repeated_var() {
        let result = expand_template("{pass_count} and {pass_count} again", &test_vars());
        assert_eq!(result, "8 and 8 again");
    }

    #[test]
    fn expand_duration_secs() {
        let result = expand_template("took {duration_secs}s", &test_vars());
        assert_eq!(result, "took 332s");
    }

    #[test]
    fn expand_duration_human() {
        let result = expand_template("took {duration}", &test_vars());
        assert_eq!(result, "took 5m32s");
    }

    #[test]
    fn expand_preserves_unknown_braces() {
        let result = expand_template("{unknown} stays", &test_vars());
        assert_eq!(result, "{unknown} stays");
    }

    #[test]
    fn expand_empty_template() {
        let result = expand_template("", &test_vars());
        assert_eq!(result, "");
    }

    #[test]
    fn expand_json_body() {
        let result = expand_template(
            r#"{"text": "Tests: {pass_count}/{total_count} ({pass_rate}%)"}"#,
            &test_vars(),
        );
        assert_eq!(
            result,
            r#"{"text": "Tests: 8/10 (80%)"}"#
        );
    }

    #[tokio::test]
    async fn run_hook_success() {
        // Simple echo — should complete without error
        let vars = test_vars();
        run_hook("echo {pass_count}", &vars).await;
        // No assert needed — just verifying it doesn't panic
    }

    #[tokio::test]
    async fn run_hook_failing_command() {
        // Should not panic even when command fails
        let vars = test_vars();
        run_hook("exit 1", &vars).await;
    }
}
