//! Process execution wrapper.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};

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

pub(crate) fn spawn_process_to_files(
    program: &str,
    args: &[String],
    stdout_path: &Path,
    stderr_path: &Path,
) -> CliResult<Child> {
    let stdout = File::create(stdout_path)?;
    let stderr = File::create(stderr_path)?;
    #[allow(
        clippy::disallowed_methods,
        reason = "this wrapper centralizes the one long-running capture process path"
    )]
    let child = Command::new(program)
        .args(args)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| {
            CliError::new(format!(
                "failed to spawn {} {}: {error}",
                program,
                args.join(" ")
            ))
        })?;
    Ok(child)
}

pub(crate) fn run_process_with_stdin(
    program: &str,
    args: &[String],
    stdin_text: &str,
    failure_mode: FailureMode,
) -> CliResult<ProcessOutput> {
    #[allow(
        clippy::disallowed_methods,
        reason = "this wrapper centralizes the stdin-fed process path"
    )]
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            CliError::new(format!(
                "failed to spawn {} {}: {error}",
                program,
                args.join(" ")
            ))
        })?;
    {
        let Some(mut stdin) = child.stdin.take() else {
            return Err(CliError::new("failed to open child stdin"));
        };
        stdin.write_all(stdin_text.as_bytes())?;
    }
    let output = child.wait_with_output()?;
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
