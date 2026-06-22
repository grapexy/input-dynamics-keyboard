//! Error handling for derived input-dynamics analysis.

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;

/// Result type for derivation operations.
pub type DeriveResult<T> = Result<T, DeriveError>;

/// Error returned while deriving higher-level analysis records.
#[derive(Debug)]
pub struct DeriveError {
    message: String,
}

impl DeriveError {
    pub(crate) fn new<T>(message: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            message: message.into(),
        }
    }
}

impl Display for DeriveError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DeriveError {}

impl From<io::Error> for DeriveError {
    fn from(error: io::Error) -> Self {
        Self::new(format!("I/O error: {error}"))
    }
}

impl From<serde_json::Error> for DeriveError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(format!("JSON error: {error}"))
    }
}
