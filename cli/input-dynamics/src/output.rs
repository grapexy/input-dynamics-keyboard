//! JSON output and exit-code handling.

use std::io::{self, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::error::{CliError, CliResult};

pub(crate) fn success_exit_code(value: &Value) -> ExitCode {
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(crate) fn write_success(value: &Value, exit_code: ExitCode) -> ExitCode {
    let mut stdout = io::stdout().lock();
    match write_json(&mut stdout, value) {
        Ok(()) => exit_code,
        Err(_) => ExitCode::from(70),
    }
}

pub(crate) fn write_failure(error: &CliError) -> ExitCode {
    let payload = json!({
        "ok": false,
        "error": error.to_string(),
    });
    let mut stderr = io::stderr().lock();
    match write_json(&mut stderr, &payload) {
        Ok(()) => ExitCode::from(1),
        Err(_) => ExitCode::from(70),
    }
}

fn write_json(writer: &mut impl Write, value: &Value) -> CliResult<()> {
    serde_json::to_writer_pretty(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::output::success_exit_code;

    #[test]
    fn false_ok_result_maps_to_failure_exit() {
        let value = json!({
            "ok": false
        });

        assert_eq!(
            success_exit_code(&value),
            std::process::ExitCode::from(1),
            "handled unsuccessful command should return non-zero"
        );
    }
}
