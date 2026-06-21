//! Android application and ADB control helpers.

use serde_json::{Value, json};

use crate::error::CliResult;
use crate::process::{FailureMode, ProcessOutput, run_process};

const ACTION_PREFIX: &str = "org.inputdynamics.ime.action";
pub(crate) const DEFAULT_PACKAGE: &str = "org.inputdynamics.ime.debug";
pub(crate) const DEFAULT_REPO: &str = "grapexy/input-dynamics-keyboard";
const IME_CLASS: &str = "helium314.keyboard.latin.LatinIME";
pub(crate) const LOG_DIR: &str = "input_dynamics_logs";
const RECEIVER: &str = ".control.InputDynamicsControlReceiver";
const STATUS_FILE: &str = "input_dynamics_control_status.json";

#[derive(Debug)]
pub(crate) struct App {
    adb: String,
    package: String,
}

impl App {
    pub(crate) const fn new(adb: String, package: String) -> Self {
        Self { adb, package }
    }

    pub(crate) fn package(&self) -> &str {
        &self.package
    }

    pub(crate) fn ime_component(&self) -> String {
        format!("{}/{IME_CLASS}", self.package)
    }

    pub(crate) fn remote_log_dir(&self) -> String {
        format!("/sdcard/Android/data/{}/files/{LOG_DIR}", self.package)
    }

    pub(crate) fn internal_log_dir() -> String {
        format!("files/{LOG_DIR}")
    }

    pub(crate) fn adb(
        &self,
        args: &[String],
        failure_mode: FailureMode,
    ) -> CliResult<ProcessOutput> {
        run_process(&self.adb, args, failure_mode)
    }

    pub(crate) fn adb_shell(
        &self,
        shell_args: Vec<String>,
        failure_mode: FailureMode,
    ) -> CliResult<ProcessOutput> {
        let mut args = Vec::with_capacity(shell_args.len().saturating_add(1));
        args.push(String::from("shell"));
        args.extend(shell_args);
        self.adb(&args, failure_mode)
    }

    pub(crate) fn broadcast(&self, action_suffix: &str, extras: Vec<String>) -> CliResult<Value> {
        let action = format!("{ACTION_PREFIX}.{action_suffix}");
        let mut shell_args = vec![
            String::from("am"),
            String::from("broadcast"),
            String::from("-n"),
            self.receiver_component(),
            String::from("-a"),
            action,
        ];
        shell_args.extend(extras);
        let broadcast_output = self.adb_shell(shell_args, FailureMode::RequireSuccess)?;
        let status_result = self
            .read_status()
            .map(|status| (String::from("status_file"), status))
            .or_else(|_| {
                status_from_broadcast_stdout(broadcast_output.stdout())
                    .map(|status| (String::from("broadcast_stdout"), status))
            });

        match status_result {
            Ok((status_source, status)) => {
                let ok = status.get("ok").and_then(Value::as_bool) == Some(true);
                Ok(json!({
                    "ok": ok,
                    "command": action_suffix,
                    "package_name": self.package,
                    "broadcast": broadcast_output.json(),
                    "status_source": status_source,
                    "status": status,
                }))
            }
            Err(error) => Ok(json!({
                "ok": false,
                "command": action_suffix,
                "package_name": self.package,
                "broadcast": broadcast_output.json(),
                "status_source": null,
                "status": null,
                "status_error": error.to_string(),
            })),
        }
    }

    fn receiver_component(&self) -> String {
        format!("{}/{}", self.package, RECEIVER)
    }

    fn remote_status_file(&self) -> String {
        format!("{}/{}", self.remote_log_dir(), STATUS_FILE)
    }

    fn read_status(&self) -> CliResult<Value> {
        let output = self.adb_shell(
            vec![String::from("cat"), self.remote_status_file()],
            FailureMode::RequireSuccess,
        )?;
        let status = serde_json::from_str(output.stdout().trim())?;
        Ok(status)
    }
}

fn status_from_broadcast_stdout(stdout: &str) -> CliResult<Value> {
    for line in stdout.lines().rev() {
        if let Some(raw_json_text) = broadcast_data_raw(line) {
            if let Ok(status) = serde_json::from_str(raw_json_text) {
                return Ok(status);
            }
        }
        if let Some(literal) = broadcast_data_literal(line) {
            let json_text: String = serde_json::from_str(literal)?;
            let status = serde_json::from_str(&json_text)?;
            return Ok(status);
        }
    }
    Err(crate::error::CliError::new(
        "broadcast result did not include status data",
    ))
}

fn broadcast_data_raw(line: &str) -> Option<&str> {
    let marker = "data=\"";
    let data_start = line.find(marker)?.saturating_add(marker.len());
    let tail = line.get(data_start..)?;
    if let Some(end_offset) = tail.rfind("\", extras") {
        return tail.get(..end_offset);
    }
    let end_offset = tail.rfind('"')?;
    tail.get(..end_offset)
}

fn broadcast_data_literal(line: &str) -> Option<&str> {
    let data_offset = line.find("data=")?;
    let tail = line.get(data_offset.saturating_add(5)..)?;
    let start_offset = tail.find('"')?;
    let start = data_offset.saturating_add(5).saturating_add(start_offset);
    let mut escaped = false;
    for (offset, character) in line.get(start.saturating_add(1)..)?.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            let end = start
                .saturating_add(1)
                .saturating_add(offset.saturating_add(1));
            return line.get(start..end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::app::{App, status_from_broadcast_stdout};

    #[test]
    fn receiver_component_uses_package_slash_shorthand() {
        let app = App::new(
            String::from("adb"),
            String::from("org.inputdynamics.ime.debug"),
        );

        assert_eq!(
            app.receiver_component(),
            "org.inputdynamics.ime.debug/.control.InputDynamicsControlReceiver",
            "am broadcast -n requires package/class component syntax"
        );
    }

    #[test]
    fn parses_broadcast_status_data() {
        let stdout = "Broadcast completed: result=-1, data=\"{\\\"ok\\\":true,\\\"command\\\":\\\"status\\\"}\"";
        let status = status_from_broadcast_stdout(stdout);

        assert!(status.is_ok(), "broadcast status data should parse");
        assert_eq!(
            status
                .as_ref()
                .ok()
                .and_then(|value| value.get("ok"))
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "status ok should be preserved"
        );
    }

    #[test]
    fn parses_raw_android_broadcast_status_data() {
        let stdout = r#"Broadcast completed: result=-1, data="{"ok":true,"command":"status"}", extras: Bundle[mParcelledData.dataSize=128]"#;
        let status = status_from_broadcast_stdout(stdout);

        assert!(status.is_ok(), "raw broadcast status data should parse");
        assert_eq!(
            status
                .as_ref()
                .ok()
                .and_then(|value| value.get("command"))
                .and_then(serde_json::Value::as_str),
            Some("status"),
            "status command should be preserved"
        );
    }
}
