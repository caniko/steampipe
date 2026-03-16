use std::path::Path;
use std::process::Stdio;

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
        vec![
            "-i".into(),
            self.key.to_string_lossy().into_owned(),
            "-o".into(), "IdentitiesOnly=yes".into(),
            "-o".into(), "ConnectTimeout=2".into(),
            "-o".into(), "StrictHostKeyChecking=no".into(),
            "-o".into(), "UserKnownHostsFile=/dev/null".into(),
            "-o".into(), "LogLevel=ERROR".into(),
            format!("{}@{}", self.user, ip),
        ]
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

    /// Check if SSH is reachable (async).
    pub async fn is_reachable(&self, ip: &str) -> bool {
        self.run(ip, "true").await.success
    }

    /// Wait for SSH to become reachable, retrying up to `timeout_secs`.
    pub async fn wait_ready(&self, ip: &str, timeout_secs: u32) -> bool {
        for _ in 0..timeout_secs {
            if self.is_reachable(ip).await {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        false
    }

    /// rsync files to a remote destination.
    pub async fn rsync(
        &self,
        sources: &[&Path],
        ip: &str,
        dest: &str,
    ) -> anyhow::Result<()> {
        let rsh = format!(
            "ssh -i {} -o IdentitiesOnly=yes -o ConnectTimeout=2 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR",
            self.key.display()
        );

        let mut args: Vec<String> = vec![
            "-az".into(),
            "--delete".into(),
            "-e".into(),
            rsh,
        ];
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
}
