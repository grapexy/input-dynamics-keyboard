//! Host-side ADB helper for Input Dynamics Keyboard.

#![forbid(unsafe_code)]

pub(crate) mod app;
pub(crate) mod args;
pub(crate) mod commands;
pub(crate) mod error;
pub(crate) mod layout;
pub(crate) mod output;
pub(crate) mod process;
pub(crate) mod record;
pub(crate) mod validate;

use std::process::ExitCode;

use clap::Parser;

use crate::app::App;
use crate::args::Cli;
use crate::commands::run_command;
use crate::output::{success_exit_code, write_failure, write_success};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let app = App::new(cli.adb, cli.package);
    let outcome = run_command(&app, cli.command);

    match outcome {
        Ok(value) => write_success(&value, success_exit_code(&value)),
        Err(error) => write_failure(&error),
    }
}
