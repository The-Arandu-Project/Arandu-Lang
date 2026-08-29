//! Arandu Compiler Command-Line Interface (CLI).
//!
//! Thin process adapter delegating command parsing, pipeline execution,
//! and process status code resolution to modular subsystems.

#![allow(clippy::collapsible_if)]

mod args;
mod artifact;
mod cli_error;
mod commands;
mod linker;
mod manifest_io;
mod pipeline;
mod project;
mod test_runner;
mod watch;

use std::env;

fn main() {
    let raw_args: Vec<String> = env::args().collect();
    let result = commands::run(raw_args);
    pipeline::finish(result);
}
