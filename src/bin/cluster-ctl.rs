use clap::Parser;

fn main() {
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
