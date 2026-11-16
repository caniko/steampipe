fn main() -> anyhow::Result<()> {
    if mcp_layer::jobs::run_if_internal()? { return Ok(()); }
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() == 3 && args[0] == "job" && args[1] == "watch",
        "usage: job-supervisor job watch <id> (MCP_JOBS_DIR selects the jobs directory)");
    let dir = std::env::var_os("MCP_JOBS_DIR").unwrap_or_else(|| ".".into());
    let jobs = mcp_layer::jobs::Jobs::new(dir, std::env::current_exe()?);
    let mut cursor = 0;
    loop {
        let (record, lines, next) = jobs.watch(&args[2], 30, cursor)?;
        print!("{lines}");
        cursor = next;
        let code = match record.state {
            mcp_layer::jobs::JobState::Pending | mcp_layer::jobs::JobState::Running => continue,
            mcp_layer::jobs::JobState::Passed => 0,
            mcp_layer::jobs::JobState::TimedOut => 124,
            mcp_layer::jobs::JobState::Cancelled => 130,
            mcp_layer::jobs::JobState::Lost => 3,
            mcp_layer::jobs::JobState::Failed => 1,
        };
        std::process::exit(code);
    }
}
