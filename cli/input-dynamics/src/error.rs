//! Error type shared by CLI modules.

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;

use serde_json::{Map, Value, json};

#[derive(Debug)]
pub(crate) struct CliError {
    message: String,
    details: Option<Value>,
}

impl CliError {
    pub(crate) fn new<T>(message: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn with_details<T>(message: T, details: Value) -> Self
    where
        T: Into<String>,
    {
        Self {
            message: message.into(),
            details: Some(details),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut payload = Map::new();
        payload.insert(String::from("ok"), json!(false));
        payload.insert(String::from("error"), json!(self.message));
        payload.insert(String::from("message"), json!(self.message));
        if let Some(details) = self.details.as_ref() {
            if let Some(error_code) = details.get("error_code").and_then(Value::as_str) {
                payload.insert(String::from("error_code"), json!(error_code));
            }
            payload.insert(String::from("details"), details.clone());
        }
        Value::Object(payload)
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::error::CliError;

    #[test]
    fn cli_error_json_exposes_message_and_error_code() {
        let error = CliError::with_details(
            "duration is required",
            json!({"error_code": "record_stdin_unavailable"}),
        );

        let payload = error.to_json();

        assert_eq!(payload.get("ok").and_then(Value::as_bool), Some(false));
        assert_eq!(
            payload.get("message").and_then(Value::as_str),
            Some("duration is required")
        );
        assert_eq!(
            payload.get("error").and_then(Value::as_str),
            Some("duration is required")
        );
        assert_eq!(
            payload.get("error_code").and_then(Value::as_str),
            Some("record_stdin_unavailable")
        );
    }
}
