//! Thin binary: CLI parse and exit-code mapping. The pipeline and the
//! `--dump-index` branch live in `src/lib.rs`; stdout is formatted in `output.rs`.

use clap::Parser;
use starsolver::{Args, run};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("Field not solved");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::from(2)
        }
    }
}
