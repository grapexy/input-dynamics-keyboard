//! Process execution wrapper.

use std::process::Command;

use serde_json::{Value, json};

use crate::error::{CliError, CliResult};

#[derive(Clone, Copy, Debug)]
pub(crate) enum FailureMode {
    AllowFailure,
    RequireSuccess,
}

#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub(crate) status_code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl ProcessOutput {
    pub(crate) fn stdout(&self) -> &str {
        &self.stdout
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "status_code": self.status_code,
            "stdout": self.stdout.trim(),
            "stderr": self.stderr.trim(),
        })
    }
}

pub(crate) fn run_process(
    program: &str,
    args: &[String],
    failure_mode: FailureMode,
) -> CliResult<ProcessOutput> {
    let output = Command::new(program).args(args).output().map_err(|error| {
        CliError::new(format!(
            "failed to run {} {}: {error}",
            program,
            args.join(" ")
        ))
    })?;

    let process_output = ProcessOutput {
        status_code: output.status.code(),
        stdout: String::from_utf8(output.stdout)?,
        stderr: String::from_utf8(output.stderr)?,
    };

    match failure_mode {
        FailureMode::AllowFailure => Ok(process_output),
        FailureMode::RequireSuccess => {
            if output.status.success() {
                Ok(process_output)
            } else {
                Err(CliError::new(format!(
                    "command failed: {} {}\nstdout: {}\nstderr: {}",
                    program,
                    args.join(" "),
                    process_output.stdout.trim(),
                    process_output.stderr.trim()
                )))
            }
        }
    }
}
