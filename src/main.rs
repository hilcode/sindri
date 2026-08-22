use clap::Parser;
use sindri::cli::Arguments;
use sindri::cli::run;
use sindri::runtime::SystemFileSystem;
use sindri::types::BuildStart;
use std::process::exit;

fn main() {
    // Capture the start instant before anything else so durations are as honest as possible.
    let start: BuildStart = BuildStart::now();
    let arguments: Arguments = Arguments::parse();
    if let Err(error) = run(start, arguments, SystemFileSystem) {
        eprintln!("{:?}", error);
        exit(1);
    }
}
