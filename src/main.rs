use agenvil::cli::{Cli, run};
use agenvil::diagnostics::guard;
use clap::Parser;

fn main() -> std::process::ExitCode {
    // clap exits 0 for --help/--version and 2 for usage errors by itself.
    let cli = Cli::parse();
    guard(|| run(&cli)).into()
}
