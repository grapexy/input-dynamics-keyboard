//! Android application and ADB control helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::error::{CliError, CliResult};
use crate::process::{FailureMode, ProcessOutput, run_process};

const ACTION_PREFIX: &str = "org.inputdynamics.ime.action";
pub(crate) const DEFAULT_PACKAGE: &str = "org.inputdynamics.ime.debug";
pub(crate) const DEFAULT_REPO: &str = "grapexy/input-dynamics-keyboard";
const IME_CLASS: &str = "helium314.keyboard.latin.LatinIME";
pub(crate) const LOG_DIR: &str = "input_dynamics_logs";
const RECEIVER: &str = ".control.InputDynamicsControlReceiver";
const STATUS_FILE: &str = "input_dynamics_control_status.json";
const RESULT_FILE_PREFIX: &str = "input_dynamics_control_result_";
const RESULT_FILE_SUFFIX: &str = ".json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(50);
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct App {
    adb: String,
    package: String,
    serial: Option<String>,
}

impl App {
    pub(crate) const fn new(adb: String, package: String, serial: Option<String>) -> Self {
        Self {
            adb,
            package,
            serial,
        }
    }

    pub(crate) fn package(&self) -> &str {
        &self.package
    }

    pub(crate) fn adb_program(&self) -> &str {
        &self.adb
    }

    pub(crate) fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    pub(crate) fn selected_device_serial(&self) -> CliResult<String> {
        let output = self.adb_host(&[String::from("devices")], FailureMode::RequireSuccess)?;
        self.selected_device_serial_from_output(output.stdout())
    }

    pub(crate) fn scoped_adb_args(&self, args: &[String]) -> CliResult<Vec<String>> {
        let serial = self.selected_device_serial()?;
        Ok(scoped_adb_args_for_serial(args, &serial))
    }

    pub(crate) fn device_selection_json(&self, adb_devices_stdout: &str) -> Value {
        let devices = parse_adb_devices(adb_devices_stdout);
        let selection = self.selected_device_serial_from_devices(&devices);
        let selected_serial = selection.as_deref().ok();
        json!({
            "ok": selection.is_ok(),
            "requested_serial": self.serial(),
            "selected_serial": selected_serial,
            "connected_device_count": devices.iter().filter(|device| device.state == "device").count(),
            "devices": devices.iter().map(|device| {
                json!({
                    "serial": device.serial,
                    "state": device.state,
                    "selected": selected_serial == Some(device.serial.as_str()),
                })
            }).collect::<Vec<_>>(),
            "error": selection.err().map(|error| error.to_string()),
        })
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
        let scoped_args = self.scoped_adb_args(args)?;
        run_process(&self.adb, &scoped_args, failure_mode)
    }

    pub(crate) fn adb_host(
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

    fn remote_result_file(&self, request_id: &str) -> String {
        format!(
            "{}/{}",
            self.remote_log_dir(),
            control_result_file_name(request_id)
        )
    }

    fn wait_for_request_status(&self, request_id: &str, timeout: Duration) -> CliResult<Value> {
        let start = Instant::now();
        let mut last_seen_request_id = None;
        let mut last_error = None;
        loop {
            for status_result in [
                self.read_external_request_result(request_id),
                self.read_internal_request_result(request_id),
                self.read_external_status(),
                self.read_internal_status(),
            ] {
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

    fn read_external_request_result(&self, request_id: &str) -> CliResult<Value> {
        let output = self.adb_shell(
            vec![String::from("cat"), self.remote_result_file(request_id)],
            FailureMode::RequireSuccess,
        )?;
        let status = serde_json::from_str(output.stdout().trim())?;
        Ok(status)
    }

    fn read_internal_request_result(&self, request_id: &str) -> CliResult<Value> {
        let output = self.adb_shell(
            vec![
                String::from("run-as"),
                String::from(self.package()),
                String::from("cat"),
                format!(
                    "{}/{}",
                    Self::internal_log_dir(),
                    control_result_file_name(request_id)
                ),
            ],
            FailureMode::RequireSuccess,
        )?;
        let status = serde_json::from_str(output.stdout().trim())?;
        Ok(status)
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

    fn selected_device_serial_from_output(&self, adb_devices_stdout: &str) -> CliResult<String> {
        self.selected_device_serial_from_devices(&parse_adb_devices(adb_devices_stdout))
    }

    fn selected_device_serial_from_devices(&self, devices: &[AdbDevice]) -> CliResult<String> {
        if let Some(requested_serial) = self.serial() {
            let Some(device) = devices
                .iter()
                .find(|device| device.serial == requested_serial)
            else {
                return Err(CliError::new(format!(
                    "adb device serial {requested_serial} was not found; connected devices: {}",
                    connected_device_list(devices)
                )));
            };
            if device.state == "device" {
                return Ok(device.serial.clone());
            }
            return Err(CliError::new(format!(
                "adb device serial {requested_serial} is in state {}; expected device",
                device.state
            )));
        }

        let connected_devices = devices
            .iter()
            .filter(|device| device.state == "device")
            .collect::<Vec<_>>();
        if connected_devices.is_empty() {
            return Err(CliError::new("no connected adb device in state device"));
        }
        if connected_devices.len() == 1 {
            let Some(device) = connected_devices.first() else {
                return Err(CliError::new("no connected adb device in state device"));
            };
            return Ok(device.serial.clone());
        }
        Err(CliError::new(format!(
            "multiple connected adb devices; pass --serial <adb-serial>. Connected devices: {}",
            connected_device_list(devices)
        )))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AdbDevice {
    serial: String,
    state: String,
}

fn scoped_adb_args_for_serial(args: &[String], serial: &str) -> Vec<String> {
    let mut scoped_args = Vec::with_capacity(args.len().saturating_add(2));
    scoped_args.push(String::from("-s"));
    scoped_args.push(String::from(serial));
    scoped_args.extend_from_slice(args);
    scoped_args
}

fn parse_adb_devices(adb_devices_stdout: &str) -> Vec<AdbDevice> {
    adb_devices_stdout
        .lines()
        .filter_map(parse_adb_device_line)
        .collect()
}

fn parse_adb_device_line(line: &str) -> Option<AdbDevice> {
    let trimmed = line.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("List of devices attached")
        || trimmed.starts_with('*')
    {
        return None;
    }
    let mut fields = trimmed.split_whitespace();
    let serial = fields.next()?;
    let state = fields.next()?;
    Some(AdbDevice {
        serial: String::from(serial),
        state: String::from(state),
    })
}

fn connected_device_list(devices: &[AdbDevice]) -> String {
    let text = devices
        .iter()
        .map(|device| format!("{}:{}", device.serial, device.state))
        .collect::<Vec<_>>()
        .join(", ");
    if text.is_empty() {
        String::from("<none>")
    } else {
        text
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

fn control_result_file_name(request_id: &str) -> String {
    let safe_request_id = request_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{RESULT_FILE_PREFIX}{safe_request_id}{RESULT_FILE_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use proptest::strategy::Strategy;

    use crate::app::{
        App, control_result_file_name, parse_adb_devices, request_id, scoped_adb_args_for_serial,
    };

    #[test]
    fn receiver_component_uses_package_slash_shorthand() {
        let app = App::new(
            String::from("adb"),
            String::from("org.inputdynamics.ime.debug"),
            None,
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

    #[test]
    fn control_result_file_name_sanitizes_path_chars() {
        assert_eq!(
            control_result_file_name("id/with space:and.dots"),
            "input_dynamics_control_result_id_with_space_and_dots.json"
        );
    }

    proptest::proptest! {
        #[test]
        fn adb_device_parser_accepts_any_serial_with_device_state(serial in serial_text()) {
            let output = format!("List of devices attached\n{serial}\tdevice\n");
            let devices = parse_adb_devices(&output);
            let device = devices.first();

            proptest::prop_assert_eq!(devices.len(), 1);
            proptest::prop_assert_eq!(device.map(|value| value.serial.as_str()), Some(serial.as_str()));
            proptest::prop_assert_eq!(device.map(|value| value.state.as_str()), Some("device"));
        }

        #[test]
        fn adb_device_parser_keeps_non_device_state(serial in serial_text(), state in non_device_state()) {
            let output = format!("List of devices attached\n{serial}\t{state}\n");
            let devices = parse_adb_devices(&output);
            let device = devices.first();

            proptest::prop_assert_eq!(devices.len(), 1);
            proptest::prop_assert_eq!(device.map(|value| value.serial.as_str()), Some(serial.as_str()));
            proptest::prop_assert_eq!(device.map(|value| value.state.as_str()), Some(state.as_str()));
        }
    }

    fn serial_text() -> impl Strategy<Value = String> {
        "[A-Za-z0-9._:-]{1,64}"
    }

    fn non_device_state() -> impl Strategy<Value = String> {
        "(offline|unauthorized|recovery|sideload|bootloader)"
    }

    #[test]
    fn selected_device_serial_rejects_ambiguous_device_list() {
        let app = App::new(
            String::from("adb"),
            String::from("org.inputdynamics.ime.debug"),
            None,
        );
        let output = "List of devices attached\nabc\tdevice\ndef\tdevice\n";

        let result = app.selected_device_serial_from_output(output);

        assert!(
            result.is_err_and(|error| error.to_string().contains("multiple connected adb devices")),
            "unscoped CLI should reject multiple connected devices"
        );
    }

    #[test]
    fn explicit_serial_selects_matching_device() {
        let app = App::new(
            String::from("adb"),
            String::from("org.inputdynamics.ime.debug"),
            Some(String::from("def")),
        );
        let output = "List of devices attached\nabc\tdevice\ndef\tdevice\n";

        let selected = app.selected_device_serial_from_output(output);

        assert_eq!(selected.ok().as_deref(), Some("def"));
    }

    #[test]
    fn scoped_adb_args_prepend_selected_serial() {
        assert_eq!(
            scoped_adb_args_for_serial(&[String::from("shell"), String::from("true")], "abc"),
            ["-s", "abc", "shell", "true"].map(String::from),
        );
    }
}
