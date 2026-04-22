use clap::Parser;

fn main() {
    memhub::logging::init();

    let cli = memhub::cli::Cli::parse();
    if let Err(error) = memhub::cli::run(cli) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
