//! Android application and ADB control helpers.

use serde_json::{Value, json};

use crate::error::CliResult;
use crate::process::{FailureMode, ProcessOutput, run_process};

const ACTION_PREFIX: &str = "org.inputdynamics.ime.action";
pub(crate) const DEFAULT_PACKAGE: &str = "org.inputdynamics.ime.debug";
pub(crate) const DEFAULT_REPO: &str = "grapexy/input-dynamics-keyboard";
const IME_CLASS: &str = "helium314.keyboard.latin.LatinIME";
const LOG_DIR: &str = "input_dynamics_logs";
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
        let status = self.read_status().ok();

        Ok(json!({
            "ok": true,
            "command": action_suffix,
            "package_name": self.package,
            "broadcast": broadcast_output.json(),
            "status": status,
        }))
    }

    fn receiver_component(&self) -> String {
        format!("{}{RECEIVER}", self.package)
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
