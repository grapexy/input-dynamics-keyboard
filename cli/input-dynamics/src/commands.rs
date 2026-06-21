//! Command implementations.

use std::cmp::Reverse;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::app::{App, LOG_DIR};
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
    let ime_registered = ime_is_registered(ime_list.stdout(), &app.ime_component());
    let gh_available = gh_version.status_code == Some(0_i32);

    Ok(json!({
        "ok": adb_devices.status_code == Some(0_i32)
            && device_connected
            && ime_list.status_code == Some(0_i32)
            && ime_registered
            && gh_available,
        "package_name": app.package(),
        "ime_component": app.ime_component(),
        "device_connected": device_connected,
        "ime_registered": ime_registered,
        "gh_available": gh_available,
        "adb_devices": adb_devices.json(),
        "ime_list": ime_list.json(),
        "gh_version": gh_version.json(),
    }))
}

fn install(app: &App, apk: Option<&Path>, repo: &str, dir: &Path) -> CliResult<Value> {
    let apk_path = if let Some(path) = apk {
        path.to_path_buf()
    } else {
        let download_dir = fresh_download_dir(dir)?;
        let release_tag = latest_release_tag(repo)?;
        let gh_args = vec![
            String::from("release"),
            String::from("download"),
            release_tag,
            String::from("--repo"),
            String::from(repo),
            String::from("--pattern"),
            String::from("*debug.apk"),
            String::from("--dir"),
            path_string(&download_dir)?,
            String::from("--clobber"),
        ];
        let _download = run_process("gh", &gh_args, FailureMode::RequireSuccess)?;
        latest_debug_apk(&download_dir)?
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
    let ok = json_ok(&enable) && json_ok(&start);

    Ok(json!({
        "ok": ok,
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
        FailureMode::AllowFailure,
    )?;

    if pull_output.status_code == Some(0_i32) {
        return Ok(json!({
            "ok": true,
            "package_name": app.package(),
            "remote_log_dir": app.remote_log_dir(),
            "output_dir": path_string(out)?,
            "pull_method": "external_adb_pull",
            "pull": pull_output.json(),
        }));
    }

    let internal_pull = pull_internal_logs(app, out)?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "remote_log_dir": app.remote_log_dir(),
        "internal_log_dir": App::internal_log_dir(),
        "output_dir": path_string(out)?,
        "pull_method": "run_as_internal_fallback",
        "external_pull": pull_output.json(),
        "internal_pull": internal_pull,
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

fn pull_internal_logs(app: &App, out: &Path) -> CliResult<Value> {
    let local_log_dir = out.join(LOG_DIR);
    fs::create_dir_all(&local_log_dir)?;
    let list_output = app.adb_shell(
        vec![
            String::from("run-as"),
            String::from(app.package()),
            String::from("ls"),
            App::internal_log_dir(),
        ],
        FailureMode::RequireSuccess,
    )?;
    let mut pulled_files = Vec::new();
    for line in list_output.stdout().lines() {
        let file_name = line.trim();
        if file_name.is_empty() {
            continue;
        }
        if file_name.contains('/') || file_name == "." || file_name == ".." {
            return Err(CliError::new(format!(
                "unexpected internal log file name: {file_name}"
            )));
        }
        let remote_file = format!("{}/{file_name}", App::internal_log_dir());
        let file_output = app.adb_shell(
            vec![
                String::from("run-as"),
                String::from(app.package()),
                String::from("cat"),
                remote_file,
            ],
            FailureMode::RequireSuccess,
        )?;
        fs::write(local_log_dir.join(file_name), file_output.stdout())?;
        pulled_files.push(String::from(file_name));
    }

    Ok(json!({
        "status_code": 0,
        "file_count": pulled_files.len(),
        "files": pulled_files,
        "list": list_output.json(),
    }))
}

fn fresh_download_dir(base_dir: &Path) -> CliResult<PathBuf> {
    let download_dir = base_dir.join(format!("download-{}", std::process::id()));
    if download_dir.exists() {
        fs::remove_dir_all(&download_dir)?;
    }
    fs::create_dir_all(&download_dir)?;
    Ok(download_dir)
}

fn latest_debug_apk(dir: &Path) -> CliResult<PathBuf> {
    let mut candidates = Vec::new();
    for entry_result in fs::read_dir(dir)? {
        let entry = entry_result?;
        let path = entry.path();
        if !is_debug_apk(&path) {
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
        .ok_or_else(|| CliError::new(format!("no debug APK was found in {}", dir.display())))
}

fn latest_release_tag(repo: &str) -> CliResult<String> {
    let gh_args = vec![
        String::from("release"),
        String::from("list"),
        String::from("--repo"),
        String::from(repo),
        String::from("--json"),
        String::from("tagName,isDraft,isPrerelease,createdAt"),
        String::from("--limit"),
        String::from("50"),
    ];
    let output = run_process("gh", &gh_args, FailureMode::RequireSuccess)?;
    latest_release_tag_from_json(output.stdout())
}

fn latest_release_tag_from_json(json_text: &str) -> CliResult<String> {
    let value: Value = serde_json::from_str(json_text)?;
    let releases = value
        .as_array()
        .ok_or_else(|| CliError::new("gh release list did not return an array"))?;
    for release in releases {
        let is_draft = release
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_draft {
            continue;
        }
        let Some(tag_name) = release.get("tagName").and_then(Value::as_str) else {
            continue;
        };
        if !tag_name.is_empty() {
            return Ok(String::from(tag_name));
        }
    }
    Err(CliError::new("no non-draft GitHub release was found"))
}

fn is_debug_apk(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|file_name| file_name.ends_with("-debug.apk"))
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

fn ime_is_registered(ime_list_stdout: &str, ime_component: &str) -> bool {
    ime_list_stdout
        .lines()
        .any(|line| line.trim() == ime_component)
}

fn json_ok(value: &Value) -> bool {
    value.get("ok").and_then(Value::as_bool) == Some(true)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use proptest::strategy::Strategy;

    use crate::commands::{
        has_connected_device, ime_is_registered, is_debug_apk, latest_release_tag_from_json,
    };

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

    #[test]
    fn ime_registration_requires_exact_component_line() {
        let ime = "org.inputdynamics.ime.debug/helium314.keyboard.latin.LatinIME";
        let ime_list = format!("other/.Ime\n{ime}\n");

        assert!(
            ime_is_registered(&ime_list, ime),
            "exact IME component should be detected"
        );
        assert!(
            !ime_is_registered("org.inputdynamics.ime.debug/.OtherIme\n", ime),
            "different IME component should not be accepted"
        );
    }

    #[test]
    fn debug_apk_filter_rejects_unrelated_apk_names() {
        assert!(
            is_debug_apk(Path::new("InputDynamicsKeyboard-v0.1.0-debug.apk")),
            "debug release asset should be accepted"
        );
        assert!(
            !is_debug_apk(Path::new("other.apk")),
            "unrelated APK should be rejected"
        );
        assert!(
            !is_debug_apk(Path::new("notdebug.apk")),
            "APK names must use the release debug suffix"
        );
    }

    #[test]
    fn release_tag_parser_accepts_prereleases_and_skips_drafts() {
        let releases = r#"
            [
                {"tagName":"v0.2.0-draft","isDraft":true,"isPrerelease":false,"createdAt":"2026-06-21T01:00:00Z"},
                {"tagName":"v0.1.0","isDraft":false,"isPrerelease":true,"createdAt":"2026-06-21T00:00:00Z"}
            ]
        "#;
        let tag_result = latest_release_tag_from_json(releases);

        assert!(
            tag_result.is_ok(),
            "latest non-draft release tag should parse"
        );
        assert_eq!(
            tag_result.as_deref().ok(),
            Some("v0.1.0"),
            "prereleases should be valid install sources"
        );
    }
}
