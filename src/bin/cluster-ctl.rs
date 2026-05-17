use clap::Parser;

fn main() {
    let cli = steampipe::Cli::parse();
    std::process::exit(match steampipe::run(cli) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err:#}");
            1
        }
    });
}
