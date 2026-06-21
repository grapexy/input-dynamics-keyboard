//! Android application and ADB control helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::error::{CliError, CliResult};
use crate::process::{FailureMode, ProcessOutput, run_process};

const ACTION_PREFIX: &str = "org.inputdynamics.ime.action";
pub(crate) const DEFAULT_PACKAGE: &str = "org.inputdynamics.ime.debug";
pub(crate) const DEFAULT_REPO: &str = "grapexy/input-dynamics-keyboard";
const IME_CLASS: &str = "helium314.keyboard.latin.LatinIME";
pub(crate) const LOG_DIR: &str = "input_dynamics_logs";
const RECEIVER: &str = ".control.InputDynamicsControlReceiver";
const STATUS_FILE: &str = "input_dynamics_control_status.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(50);
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

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

    pub(crate) fn adb_program(&self) -> &str {
        &self.adb
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
        let request_id = request_id(action_suffix);
        let action = format!("{ACTION_PREFIX}.{action_suffix}");
        let mut shell_args = vec![
            String::from("am"),
            String::from("broadcast"),
            String::from("-n"),
            self.receiver_component(),
            String::from("-a"),
            action,
            String::from("--es"),
            String::from("request_id"),
            request_id.clone(),
        ];
        shell_args.extend(extras);
        let _broadcast_output = self.adb_shell(shell_args, FailureMode::RequireSuccess)?;
        self.wait_for_request_status(&request_id, REQUEST_TIMEOUT)
    }

    fn receiver_component(&self) -> String {
        format!("{}/{}", self.package, RECEIVER)
    }

    fn remote_status_file(&self) -> String {
        format!("{}/{}", self.remote_log_dir(), STATUS_FILE)
    }

    fn wait_for_request_status(&self, request_id: &str, timeout: Duration) -> CliResult<Value> {
        let start = Instant::now();
        let mut last_seen_request_id = None;
        let mut last_error = None;
        loop {
            for status_result in [self.read_external_status(), self.read_internal_status()] {
                match status_result {
                    Ok(status) => {
                        let status_request_id = status.get("request_id").and_then(Value::as_str);
                        if status_request_id == Some(request_id) {
                            return Ok(status);
                        }
                        last_seen_request_id = status_request_id.map(String::from);
                    }
                    Err(error) => {
                        last_error = Some(error.to_string());
                    }
                }
            }

            if start.elapsed() >= timeout {
                return Err(CliError::new(format!(
                    "timed out waiting for request_id {request_id}; last_seen_request_id={}; last_error={}",
                    last_seen_request_id.as_deref().unwrap_or("<none>"),
                    last_error.as_deref().unwrap_or("<none>")
                )));
            }
            thread::sleep(REQUEST_POLL_INTERVAL);
        }
    }

    fn read_external_status(&self) -> CliResult<Value> {
        let output = self.adb_shell(
            vec![String::from("cat"), self.remote_status_file()],
            FailureMode::RequireSuccess,
        )?;
        let status = serde_json::from_str(output.stdout().trim())?;
        Ok(status)
    }

    fn read_internal_status(&self) -> CliResult<Value> {
        let output = self.adb_shell(
            vec![
                String::from("run-as"),
                String::from(self.package()),
                String::from("cat"),
                format!("{}/{}", Self::internal_log_dir(), STATUS_FILE),
            ],
            FailureMode::RequireSuccess,
        )?;
        let status = serde_json::from_str(output.stdout().trim())?;
        Ok(status)
    }
}

fn request_id(action_suffix: &str) -> String {
    let counter = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_millis());
    format!(
        "idk-{}-{}-{millis}-{counter}",
        std::process::id(),
        action_suffix.to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use crate::app::{App, request_id};

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
    fn generated_request_id_includes_action_hint() {
        let generated = request_id("KEYBOARD_LAYOUT");
        assert!(
            generated.contains("keyboard_layout"),
            "request id should keep a readable action hint"
        );
    }
}
