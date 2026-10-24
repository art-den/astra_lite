//! Thin binary: parse the CLI (`Args`) and map `starsolver::run` onto the exit
//! codes. The pipeline and the `--dump-index` branch live in the library crate
//! (`src/lib.rs`); stdout is formatted in `output.rs`.

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
