//! Android `getevent -lt` normalization.

mod normalize;
mod parser;
mod touch_state;

pub use normalize::{GETEVENT_SCHEMA, NormalizeStats, normalize_file, normalize_reader};

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;

/// Error returned while normalizing `getevent` output.
#[derive(Debug)]
pub struct NormalizeError {
    message: String,
}

impl NormalizeError {
    pub(crate) fn new<T>(message: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            message: message.into(),
        }
    }
}

impl Display for NormalizeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for NormalizeError {}

impl From<io::Error> for NormalizeError {
    fn from(error: io::Error) -> Self {
        Self::new(format!("I/O error: {error}"))
    }
}

impl From<serde_json::Error> for NormalizeError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(format!("JSON error: {error}"))
    }
}

pub(crate) type NormalizeResult<T> = Result<T, NormalizeError>;
