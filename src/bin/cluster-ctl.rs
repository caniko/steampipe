use clap::Parser;

fn main() {
    match mcp_layer::jobs::run_if_internal() {
        Ok(true) => return,
        Ok(false) => {}
        Err(err) => {
            eprintln!("{err:#}");
            std::process::exit(1);
        }
    }
    // `job watch <id>` follows an MCP job (see steampipe::mcp::jobs_dir); it is
    // handled before clap so the cluster CLI's global flags stay unchanged.
    let args: Vec<String> = std::env::args().collect();
    if let [_, job, watch, id] = args.as_slice()
        && job == "job"
        && watch == "watch"
    {
        std::process::exit(steampipe::mcp::job_watch_main(id).unwrap_or_else(|err| {
            eprintln!("{err:#}");
            2
        }));
    }
    let cli = steampipe::Cli::parse();
    std::process::exit(match steampipe::run(cli) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err:#}");
            if err
                .downcast_ref::<steampipe::game::steam::GuardCodeNeeded>()
                .is_some()
            {
                steampipe::game::steam::GUARD_CODE_NEEDED_EXIT_CODE
            } else {
                1
            }
        }
    });
}
