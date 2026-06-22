//! Error type shared by CLI modules.

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;

#[derive(Debug)]
pub(crate) struct CliError {
    message: String,
}

impl CliError {
    pub(crate) fn new<T>(message: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            message: message.into(),
        }
    }
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CliError {}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::new(format!("I/O error: {error}"))
    }
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(format!("JSON error: {error}"))
    }
}

impl From<input_dynamics_analysis::getevent::NormalizeError> for CliError {
    fn from(error: input_dynamics_analysis::getevent::NormalizeError) -> Self {
        Self::new(format!("getevent normalization error: {error}"))
    }
}

impl From<input_dynamics_analysis::derivation::DeriveError> for CliError {
    fn from(error: input_dynamics_analysis::derivation::DeriveError) -> Self {
        Self::new(format!("derivation error: {error}"))
    }
}

impl From<std::string::FromUtf8Error> for CliError {
    fn from(error: std::string::FromUtf8Error) -> Self {
        Self::new(format!("UTF-8 error: {error}"))
    }
}

pub(crate) type CliResult<T> = Result<T, CliError>;
