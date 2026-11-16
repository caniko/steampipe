fn main() -> anyhow::Result<()> {
    if mcp_layer::jobs::run_if_internal()? {
        return Ok(());
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 3 && args[0] == "job" && args[1] == "watch",
        "usage: job-supervisor job watch <id> (MCP_JOBS_DIR selects the jobs directory)"
    );
    let dir = std::env::var_os("MCP_JOBS_DIR").unwrap_or_else(|| ".".into());
    let jobs = mcp_layer::jobs::Jobs::new(dir, std::env::current_exe()?);
    std::process::exit(mcp_layer::jobs::watch_to_exit(&jobs, &args[2])?);
}
