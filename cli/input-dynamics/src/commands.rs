//! Command implementations.

use std::cmp::Reverse;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::app::App;
use crate::args::Commands;
use crate::error::{CliError, CliResult};
use crate::layout::{json_number_to_shell_arg, key_matches};
use crate::process::{FailureMode, run_process};
use crate::validate::validate_logs;

pub(crate) fn run_command(app: &App, command: Commands) -> CliResult<Value> {
    match command {
        Commands::Doctor => doctor(app),
        Commands::Install { apk, repo, dir } => install(app, apk.as_deref(), &repo, &dir),
        Commands::SelectIme => select_ime(app),
        Commands::EnableLogging => app.broadcast("ENABLE", Vec::new()),
        Commands::DisableLogging => app.broadcast("DISABLE", Vec::new()),
        Commands::Start { run_id } => start(app, &run_id),
        Commands::Stop => app.broadcast("STOP", Vec::new()),
        Commands::Status => app.broadcast("STATUS", Vec::new()),
        Commands::Layout => app.broadcast("KEYBOARD_LAYOUT", Vec::new()),
        Commands::ListLogs => app.broadcast("LIST_LOGS", Vec::new()),
        Commands::ClearLogs => app.broadcast("CLEAR_LOGS", Vec::new()),
        Commands::Pull { out } => pull_logs(app, &out),
        Commands::Validate { path, run_id } => validate_logs(&path, run_id.as_deref()),
        Commands::Tap { label, code } => tap_key(app, label.as_deref(), code),
    }
}

fn doctor(app: &App) -> CliResult<Value> {
    let adb_devices = app.adb(&[String::from("devices")], FailureMode::AllowFailure)?;
    let ime_list = app.adb_shell(
        vec![
            String::from("ime"),
            String::from("list"),
            String::from("-s"),
        ],
        FailureMode::AllowFailure,
    )?;
    let gh_version = run_process(
        "gh",
        &[String::from("--version")],
        FailureMode::AllowFailure,
    )?;
    let device_connected = has_connected_device(adb_devices.stdout());

    Ok(json!({
        "ok": adb_devices.status_code == Some(0) && device_connected,
        "package_name": app.package(),
        "ime_component": app.ime_component(),
        "device_connected": device_connected,
        "adb_devices": adb_devices.json(),
        "ime_list": ime_list.json(),
        "gh_version": gh_version.json(),
    }))
}

fn install(app: &App, apk: Option<&Path>, repo: &str, dir: &Path) -> CliResult<Value> {
    let apk_path = if let Some(path) = apk {
        path.to_path_buf()
    } else {
        fs::create_dir_all(dir)?;
        let gh_args = vec![
            String::from("release"),
            String::from("download"),
            String::from("--repo"),
            String::from(repo),
            String::from("--pattern"),
            String::from("*debug.apk"),
            String::from("--dir"),
            path_string(dir)?,
            String::from("--clobber"),
        ];
        let _download = run_process("gh", &gh_args, FailureMode::RequireSuccess)?;
        latest_apk(dir)?
    };

    let install_output = app.adb(
        &[
            String::from("install"),
            String::from("-r"),
            path_string(&apk_path)?,
        ],
        FailureMode::RequireSuccess,
    )?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "apk": path_string(&apk_path)?,
        "install": install_output.json(),
    }))
}

fn select_ime(app: &App) -> CliResult<Value> {
    let component = app.ime_component();
    let enable_output = app.adb_shell(
        vec![
            String::from("ime"),
            String::from("enable"),
            component.clone(),
        ],
        FailureMode::RequireSuccess,
    )?;
    let set_output = app.adb_shell(
        vec![String::from("ime"), String::from("set"), component.clone()],
        FailureMode::RequireSuccess,
    )?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "ime_component": component,
        "enable": enable_output.json(),
        "set": set_output.json(),
    }))
}

fn start(app: &App, run_id: &str) -> CliResult<Value> {
    let enable = app.broadcast("ENABLE", Vec::new())?;
    let start = app.broadcast(
        "START",
        vec![
            String::from("--es"),
            String::from("run_id"),
            String::from(run_id),
        ],
    )?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "external_run_id": run_id,
        "enable": enable,
        "start": start,
    }))
}

fn pull_logs(app: &App, out: &Path) -> CliResult<Value> {
    fs::create_dir_all(out)?;
    let pull_output = app.adb(
        &[
            String::from("pull"),
            app.remote_log_dir(),
            path_string(out)?,
        ],
        FailureMode::RequireSuccess,
    )?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "remote_log_dir": app.remote_log_dir(),
        "output_dir": path_string(out)?,
        "pull": pull_output.json(),
    }))
}

fn tap_key(app: &App, label: Option<&str>, code: Option<i64>) -> CliResult<Value> {
    if label.is_none() && code.is_none() {
        return Err(CliError::new("tap requires --label or --code"));
    }

    let layout_result = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
    let status = layout_result
        .get("status")
        .ok_or_else(|| CliError::new("layout status was not available"))?;
    let layout = status
        .get("keyboard_layout")
        .ok_or_else(|| CliError::new("keyboard_layout was not present"))?;
    let available = layout
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !available {
        return Err(CliError::new("keyboard layout is not available"));
    }

    let keys = layout
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| CliError::new("keyboard_layout.keys was not an array"))?;
    let key = keys
        .iter()
        .find(|candidate| key_matches(candidate, label, code))
        .ok_or_else(|| CliError::new("requested key was not found in keyboard layout"))?;
    let x = key
        .get("tap_center_screen_x_px")
        .and_then(json_number_to_shell_arg)
        .ok_or_else(|| CliError::new("key is missing tap_center_screen_x_px"))?;
    let y = key
        .get("tap_center_screen_y_px")
        .and_then(json_number_to_shell_arg)
        .ok_or_else(|| CliError::new("key is missing tap_center_screen_y_px"))?;

    let tap_output = app.adb_shell(
        vec![String::from("input"), String::from("tap"), x, y],
        FailureMode::RequireSuccess,
    )?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "key": key,
        "tap": tap_output.json(),
    }))
}

fn latest_apk(dir: &Path) -> CliResult<PathBuf> {
    let mut candidates = Vec::new();
    for entry_result in fs::read_dir(dir)? {
        let entry = entry_result?;
        let path = entry.path();
        if path.extension().and_then(OsStr::to_str) != Some("apk") {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified = match metadata.modified() {
            Ok(time) => time,
            Err(_) => UNIX_EPOCH,
        };
        candidates.push((modified, path));
    }
    candidates.sort_by_key(|candidate| Reverse(candidate.0));
    candidates
        .first()
        .map(|candidate| candidate.1.clone())
        .ok_or_else(|| CliError::new(format!("no APK was found in {}", dir.display())))
}

pub(crate) fn path_string(path: &Path) -> CliResult<String> {
    path.to_str()
        .map(String::from)
        .ok_or_else(|| CliError::new(format!("path is not valid UTF-8: {}", path.display())))
}

fn has_connected_device(adb_devices_stdout: &str) -> bool {
    adb_devices_stdout.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let serial = fields.next();
        let state = fields.next();
        serial.is_some() && state == Some("device")
    })
}

#[cfg(test)]
mod tests {
    use proptest::strategy::Strategy;

    use crate::commands::has_connected_device;

    proptest::proptest! {
        #[test]
        fn device_parser_accepts_any_serial_with_device_state(serial in serial_text()) {
            let output = format!("List of devices attached\n{serial}\tdevice\n");

            proptest::prop_assert!(
                has_connected_device(&output),
                "adb device state should be accepted"
            );
        }

        #[test]
        fn device_parser_rejects_non_device_state(serial in serial_text(), state in non_device_state()) {
            let output = format!("List of devices attached\n{serial}\t{state}\n");

            proptest::prop_assert!(
                !has_connected_device(&output),
                "only the literal device state should be accepted"
            );
        }
    }

    fn serial_text() -> impl Strategy<Value = String> {
        "[A-Za-z0-9._:-]{1,64}"
    }

    fn non_device_state() -> impl Strategy<Value = String> {
        "(offline|unauthorized|recovery|sideload|bootloader)"
    }
}
