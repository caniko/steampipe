use std::process::Command;
use std::time::Duration;

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Clone)]
pub struct SteampipeMcp {
    tool_router: ToolRouter<Self>,
}

impl SteampipeMcp {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NixRunInput {
    /// Flake output target (e.g. "cluster-1v1-up", "cluster-tournament-test").
    pub target: String,

    /// Extra arguments passed after `--` to the flake app.
    #[serde(default)]
    pub args: Option<Vec<String>>,

    /// Flake reference (default: ".").
    #[serde(default)]
    pub flake: Option<String>,

    /// Timeout in seconds (default: 300).
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[tool_router]
impl SteampipeMcp {
    #[tool(
        description = "Run a Nix flake app. Executes `nix run <flake>#<target> -- <args...>`. Use for any `nix run .#<target>` command."
    )]
    fn nix_run(
        &self,
        Parameters(input): Parameters<NixRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let flake = input.flake.as_deref().unwrap_or(".");
        let flake_ref = format!("{flake}#{}", input.target);
        let timeout_secs = input.timeout.unwrap_or(300);

        let mut cmd = Command::new("nix");
        cmd.arg("run").arg(&flake_ref);

        if let Some(ref args) = input.args {
            if !args.is_empty() {
                cmd.arg("--");
                cmd.args(args);
            }
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            ErrorData::internal_error(format!("Failed to spawn nix run: {e}"), None)
        })?;

        // Wait with timeout
        let output = wait_with_timeout(child, Duration::from_secs(timeout_secs)).map_err(|e| {
            ErrorData::internal_error(format!("nix run failed: {e}"), None)
        })?;

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let text = strip_ansi(&combined);

        if output.status.success() {
            Ok(CallToolResult::success(vec![Content::text(text)]))
        } else {
            let code = output
                .status
                .code()
                .map(|c| format!(" (exit code {c})"))
                .unwrap_or_default();
            Ok(CallToolResult::success(vec![Content::text(format!(
                "nix run {flake_ref} failed{code}:\n{text}"
            ))]))
        }
    }
}

fn wait_with_timeout(
    child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    use std::sync::mpsc;
    use std::thread;

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result.map_err(|e| format!("IO error: {e}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "timed out after {}s",
            timeout.as_secs()
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("child process thread panicked".into()),
    }
}

#[tool_handler]
impl ServerHandler for SteampipeMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Steampipe: generic cluster orchestration tools. Run Nix flake apps and manage VM clusters."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

pub async fn serve() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    // Log to stderr so stdout stays clean for MCP JSON-RPC protocol
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let service = SteampipeMcp::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
