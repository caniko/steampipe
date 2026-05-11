use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Output from an SSH command.
#[derive(Debug)]
pub struct SshOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// SSH client that shells out to the `ssh` binary.
#[derive(Debug, Clone)]
pub struct SshClient {
    key: std::path::PathBuf,
    user: String,
}

impl SshClient {
    pub fn new(key: &Path, user: &str) -> Self {
        Self {
            key: key.to_owned(),
            user: user.into(),
        }
    }

    fn ssh_args(&self, ip: &str) -> Vec<String> {
        self.ssh_args_extra(ip, &[])
    }

    /// Build SSH args with additional options inserted before the destination.
    fn ssh_args_extra(&self, ip: &str, extra_opts: &[&str]) -> Vec<String> {
        let mut args = vec![
            "-i".into(),
            self.key.to_string_lossy().into_owned(),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            "ConnectTimeout=2".into(),
            "-o".into(),
            "StrictHostKeyChecking=no".into(),
            "-o".into(),
            "UserKnownHostsFile=/dev/null".into(),
            "-o".into(),
            "LogLevel=ERROR".into(),
            "-o".into(),
            "ServerAliveInterval=1".into(),
            "-o".into(),
            "ServerAliveCountMax=3".into(),
        ];
        for opt in extra_opts {
            args.push((*opt).into());
        }
        args.push(format!("{}@{}", self.user, ip));
        args
    }

    /// Spawn a long-running SSH command with stdout piped for streaming.
    pub fn spawn_streaming(
        &self,
        ip: &str,
        cmd: &str,
        extra_opts: &[&str],
    ) -> std::io::Result<tokio::process::Child> {
        let mut args = self.ssh_args_extra(ip, extra_opts);
        args.push(cmd.into());
        tokio::process::Command::new("ssh")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    }

    /// Run a command with a timeout. Returns a "timed out" error on expiry.
    pub async fn run_with_timeout(&self, ip: &str, cmd: &str, timeout: Duration) -> SshOutput {
        match tokio::time::timeout(timeout, self.run(ip, cmd)).await {
            Ok(output) => output,
            Err(_) => SshOutput {
                stdout: String::new(),
                stderr: "SSH command timed out".into(),
                success: false,
            },
        }
    }

    /// Run a command on a remote host (async).
    pub async fn run(&self, ip: &str, cmd: &str) -> SshOutput {
        let mut args = self.ssh_args(ip);
        args.push(cmd.into());

        let result = tokio::process::Command::new("ssh")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match result {
            Ok(output) => SshOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                success: output.status.success(),
            },
            Err(e) => SshOutput {
                stdout: String::new(),
                stderr: format!("SSH failed to execute: {e}"),
                success: false,
            },
        }
    }

    /// Check if SSH is reachable (async). Times out after 5s to avoid hanging
    /// when a VM is half-alive (TCP connects but auth/command hangs).
    pub async fn is_reachable(&self, ip: &str) -> bool {
        self.run_with_timeout(ip, "true", Duration::from_secs(5))
            .await
            .success
    }

    /// Wait for SSH to become reachable, retrying with exponential backoff.
    /// Starts with 500ms delay, doubles up to 4s, total up to `timeout_secs`.
    pub async fn wait_ready(&self, ip: &str, timeout_secs: u32) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs as u64);
        let mut delay = Duration::from_millis(500);

        loop {
            if self.is_reachable(ip).await {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            let remaining = deadline - tokio::time::Instant::now();
            tokio::time::sleep(delay.min(remaining)).await;
            delay = (delay * 2).min(Duration::from_secs(4));
        }
    }

    /// rsync files to a remote destination.
    pub async fn rsync(&self, sources: &[&Path], ip: &str, dest: &str) -> anyhow::Result<()> {
        let rsh = format!(
            "ssh -i {} -o IdentitiesOnly=yes -o ConnectTimeout=2 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR",
            self.key.display()
        );

        let mut args: Vec<String> = vec!["-az".into(), "--delete".into(), "-e".into(), rsh];
        for src in sources {
            args.push(src.to_string_lossy().into_owned());
        }
        args.push(format!("{}@{}:{}", self.user, ip, dest));

        let output = tokio::process::Command::new("rsync")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("rsync to {ip}:{dest} failed: {stderr}");
        }
        Ok(())
    }

    /// rsync files from a remote source into a local destination directory.
    pub async fn fetch(&self, ip: &str, source: &str, dest: &Path) -> anyhow::Result<()> {
        let rsh = format!(
            "ssh -i {} -o IdentitiesOnly=yes -o ConnectTimeout=2 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR",
            self.key.display()
        );

        std::fs::create_dir_all(dest)?;
        let args: Vec<String> = vec![
            "-az".into(),
            "-e".into(),
            rsh,
            format!("{}@{}:{}", self.user, ip, source),
            dest.to_string_lossy().into_owned(),
        ];

        let output = tokio::process::Command::new("rsync")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("rsync from {ip}:{source} failed: {stderr}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> SshClient {
        SshClient::new(Path::new("/tmp/key"), "deploy")
    }

    #[test]
    fn ssh_args_includes_key_and_destination() {
        let args = client().ssh_args("10.0.0.1");
        // Key is the first value-arg after `-i`
        let i_pos = args.iter().position(|s| s == "-i").unwrap();
        assert_eq!(args[i_pos + 1], "/tmp/key");
        // Destination is the last arg
        assert_eq!(args.last().unwrap(), "deploy@10.0.0.1");
    }

    #[test]
    fn ssh_args_sets_security_and_timeout_options() {
        let args = client().ssh_args("10.0.0.1");
        let joined = args.join(" ");
        assert!(joined.contains("IdentitiesOnly=yes"));
        assert!(joined.contains("StrictHostKeyChecking=no"));
        assert!(joined.contains("UserKnownHostsFile=/dev/null"));
        assert!(joined.contains("ConnectTimeout=2"));
        assert!(joined.contains("ServerAliveInterval=1"));
        assert!(joined.contains("ServerAliveCountMax=3"));
        assert!(joined.contains("LogLevel=ERROR"));
    }

    #[test]
    fn ssh_args_extra_inserts_before_destination() {
        let args = client().ssh_args_extra("10.0.0.1", &["-o", "ConnectTimeout=10"]);
        let dest_idx = args.iter().position(|s| s == "deploy@10.0.0.1").unwrap();
        assert_eq!(dest_idx, args.len() - 1);
        // The extra option appears before the destination
        let timeout_idx = args
            .iter()
            .position(|s| s == "ConnectTimeout=10")
            .expect("extra opt should be present");
        assert!(timeout_idx < dest_idx);
    }

    #[test]
    fn ssh_args_extra_with_no_extras_matches_ssh_args() {
        let c = client();
        assert_eq!(c.ssh_args("1.2.3.4"), c.ssh_args_extra("1.2.3.4", &[]));
    }

    #[test]
    fn new_stores_key_path_and_user() {
        let c = SshClient::new(Path::new("/etc/keys/id_ed25519"), "alice");
        let args = c.ssh_args("host");
        assert!(args.contains(&"/etc/keys/id_ed25519".to_string()));
        assert_eq!(args.last().unwrap(), "alice@host");
    }
}
