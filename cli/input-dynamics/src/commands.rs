//! Command implementations.

use std::cmp::Reverse;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::app::{App, LOG_DIR};
use crate::args::{Commands, ControllerCommand, PressKey, SessionCommand, TouchCommand};
use crate::controller::{self, RunConfig, SessionStartPermit};
use crate::error::{CliError, CliResult};
use crate::layout::{json_number_to_shell_arg, key_matches};
use crate::process::{FailureMode, run_process};
use crate::record::{RecordConfig, record_run};
use crate::uinput::{self, TapSpec};
use crate::validate::validate_logs;

const LAYOUT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const LAYOUT_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) fn run_command(app: &App, command: Commands) -> CliResult<Value> {
    match command {
        Commands::Doctor => doctor(app),
        Commands::Install { apk, repo, dir } => install(app, apk.as_deref(), &repo, &dir),
        Commands::SelectIme => select_ime(app),
        Commands::EnableLogging => app.broadcast("ENABLE", Vec::new()),
        Commands::DisableLogging => app.broadcast("DISABLE", Vec::new()),
        Commands::Start {
            run_id,
            input_actor,
            input_controller,
            input_cadence_policy,
        } => start(
            app,
            &run_id,
            &input_actor,
            input_controller.as_deref(),
            &input_cadence_policy,
        ),
        Commands::Stop => app.broadcast("STOP", Vec::new()),
        Commands::Status => app.broadcast("STATUS", Vec::new()),
        Commands::Layout {
            wait_visible,
            wait_hidden,
        } => {
            let wait = if wait_visible {
                LayoutWait::Visible
            } else if wait_hidden {
                LayoutWait::Hidden
            } else {
                LayoutWait::Current
            };
            layout(app, wait)
        }
        Commands::HideKeyboard => hide_keyboard(app),
        Commands::ListLogs => app.broadcast("LIST_LOGS", Vec::new()),
        Commands::ClearLogs => app.broadcast("CLEAR_LOGS", Vec::new()),
        Commands::Pull { out } => pull_logs(app, &out),
        Commands::Validate { path, run_id } => validate_logs(&path, run_id.as_deref()),
        Commands::Record {
            run_id,
            out,
            duration_ms,
            input_actor,
            input_controller,
            input_cadence_policy,
        } => {
            let config = RecordConfig {
                run_id,
                out,
                duration_ms,
                input_actor,
                input_controller,
                input_cadence_policy,
            };
            record_run(app, &config)
        }
        Commands::Session {
            command: session_command,
        } => session(app, session_command),
        Commands::Tap { label, code } => tap_key(app, label.as_deref(), code),
        Commands::Press { key } => press_key(app, key),
        Commands::Touch {
            command: touch_command,
        } => touch(app, &touch_command),
        Commands::Controller {
            command: controller_command,
        } => run_controller_command(app, controller_command),
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

fn start(
    app: &App,
    run_id: &str,
    input_actor: &str,
    input_controller: Option<&str>,
    input_cadence_policy: &str,
) -> CliResult<Value> {
    let mut extras = vec![
        String::from("--es"),
        String::from("run_id"),
        String::from(run_id),
        String::from("--es"),
        String::from("input_actor"),
        String::from(input_actor),
        String::from("--es"),
        String::from("input_cadence_policy"),
        String::from(input_cadence_policy),
    ];
    if let Some(controller) = input_controller {
        extras.extend([
            String::from("--es"),
            String::from("input_controller"),
            String::from(controller),
        ]);
    }
    let enable = app.broadcast("ENABLE", Vec::new())?;
    if enable.get("ok").and_then(Value::as_bool) == Some(false) {
        return Ok(json!({
            "ok": false,
            "package_name": app.package(),
            "error": "failed to enable logging before starting session",
            "enable": enable,
        }));
    }
    app.broadcast("START", extras)
}

fn session(app: &App, command: SessionCommand) -> CliResult<Value> {
    match command {
        SessionCommand::Start {
            run_id,
            input_actor,
            input_controller,
            input_cadence_policy,
        } => session_start(
            app,
            &run_id,
            &input_actor,
            &input_controller,
            &input_cadence_policy,
        ),
        SessionCommand::Status => session_status(app),
        SessionCommand::Stop => session_stop(app),
    }
}

fn session_start(
    app: &App,
    run_id: &str,
    input_actor: &str,
    input_controller: &str,
    input_cadence_policy: &str,
) -> CliResult<Value> {
    let mut session_lock = match controller::acquire_session_start(app, run_id)? {
        SessionStartPermit::Acquired(session_lock) => session_lock,
        SessionStartPermit::Busy(status) => return Ok(status),
    };
    let select = select_ime(app)?;
    let ime = start(
        app,
        run_id,
        input_actor,
        Some(input_controller),
        input_cadence_policy,
    )?;
    if ime.get("ok").and_then(Value::as_bool) == Some(false) {
        return Ok(json!({
            "ok": false,
            "package_name": app.package(),
            "error": "failed to start IME logging session",
            "select_ime": select,
            "ime": ime,
        }));
    }

    match controller::start(app, run_id) {
        Ok(mut input) => {
            let input_ok = input.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let stop_after_input_failure = if input_ok {
                session_lock.activate(&input)?;
                input = controller::status(app);
                Value::Null
            } else {
                app.broadcast("STOP", Vec::new()).unwrap_or_else(|error| {
                    json!({
                        "ok": false,
                        "error": error.to_string(),
                    })
                })
            };
            Ok(json!({
                "ok": input_ok,
                "package_name": app.package(),
                "run_id": run_id,
                "select_ime": select,
                "ime": ime,
                "input": input,
                "stop_after_input_failure": stop_after_input_failure,
            }))
        }
        Err(error) => {
            let stop_result = app.broadcast("STOP", Vec::new()).ok();
            Err(CliError::new(format!(
                "failed to start input controller after IME session start: {error}; IME stop attempted: {}",
                stop_result.as_ref().map_or("unavailable", |_| "available")
            )))
        }
    }
}

fn session_status(app: &App) -> CliResult<Value> {
    let ime = app.broadcast("STATUS", Vec::new())?;
    let input = controller::status(app);
    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "ime": ime,
        "input": input,
    }))
}

fn session_stop(app: &App) -> CliResult<Value> {
    let input = controller::stop(app)?;
    let ime = app.broadcast("STOP", Vec::new())?;
    controller::clear_session_lock(app)?;
    Ok(json!({
        "ok": ime.get("ok").and_then(Value::as_bool).unwrap_or(false)
            && input.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "package_name": app.package(),
        "input": input,
        "ime": ime,
    }))
}

fn run_controller_command(app: &App, command: ControllerCommand) -> CliResult<Value> {
    match command {
        ControllerCommand::Run {
            socket,
            state,
            uinput_stdout,
            uinput_stderr,
            run_id,
        } => {
            let config = RunConfig {
                socket,
                state,
                uinput_stdout,
                uinput_stderr,
                run_id,
            };
            controller::run(app, &config)
        }
    }
}

fn layout(app: &App, wait: LayoutWait) -> CliResult<Value> {
    match wait {
        LayoutWait::Current => app.broadcast("KEYBOARD_LAYOUT", Vec::new()),
        LayoutWait::Visible | LayoutWait::Hidden => wait_for_layout(app, wait),
    }
}

fn hide_keyboard(app: &App) -> CliResult<Value> {
    let before = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
    if !layout_available(&before)? {
        return Ok(json!({
            "ok": true,
            "package_name": app.package(),
            "already_hidden": true,
            "layout": before,
        }));
    }

    let hide_output = app.adb_shell(
        vec![
            String::from("input"),
            String::from("keyevent"),
            String::from("KEYCODE_BACK"),
        ],
        FailureMode::RequireSuccess,
    )?;
    let after = wait_for_layout(app, LayoutWait::Hidden)?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "already_hidden": false,
        "hide": hide_output.json(),
        "layout": after,
    }))
}

fn wait_for_layout(app: &App, wait: LayoutWait) -> CliResult<Value> {
    let start = Instant::now();
    loop {
        let status = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
        if layout_matches(&status, wait)? {
            return Ok(with_layout_wait_metadata(status, wait, start.elapsed()));
        }
        if start.elapsed() >= LAYOUT_WAIT_TIMEOUT {
            return Ok(json!({
                "ok": false,
                "package_name": app.package(),
                "error": format!("timed out waiting for keyboard layout to be {}", wait.description()),
                "wait": wait.description(),
                "timeout_ms": millis_u64(LAYOUT_WAIT_TIMEOUT),
                "layout": status,
            }));
        }
        std::thread::sleep(LAYOUT_POLL_INTERVAL);
    }
}

fn with_layout_wait_metadata(mut status: Value, wait: LayoutWait, elapsed: Duration) -> Value {
    if let Some(object) = status.as_object_mut() {
        object.insert(String::from("wait"), json!(wait.description()));
        object.insert(String::from("wait_elapsed_ms"), json!(millis_u64(elapsed)));
    }
    status
}

fn layout_matches(status: &Value, wait: LayoutWait) -> CliResult<bool> {
    let available = layout_available(status)?;
    Ok(match wait {
        LayoutWait::Current => true,
        LayoutWait::Visible => available,
        LayoutWait::Hidden => !available,
    })
}

fn layout_available(status: &Value) -> CliResult<bool> {
    let layout = status
        .get("keyboard_layout")
        .ok_or_else(|| CliError::new("keyboard_layout was not present"))?;
    Ok(layout
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn press_key(app: &App, key: PressKey) -> CliResult<Value> {
    let code = press_key_code(key);
    let mut result = tap_key(app, None, Some(code))?;
    if let Some(object) = result.as_object_mut() {
        object.insert(String::from("pressed_key"), json!(press_key_name(key)));
    }
    Ok(result)
}

fn touch(app: &App, command: &TouchCommand) -> CliResult<Value> {
    match *command {
        TouchCommand::Doctor => uinput::doctor(app),
        TouchCommand::Tap { x, y, hold_ms } => uinput::tap(app, TapSpec { x, y, hold_ms }),
    }
}

const fn press_key_code(key: PressKey) -> i64 {
    match key {
        PressKey::Delete => -7,
        PressKey::Enter => 10,
        PressKey::Space => 32,
    }
}

const fn press_key_name(key: PressKey) -> &'static str {
    match key {
        PressKey::Delete => "delete",
        PressKey::Enter => "enter",
        PressKey::Space => "space",
    }
}

#[derive(Clone, Copy)]
enum LayoutWait {
    Current,
    Visible,
    Hidden,
}

impl LayoutWait {
    const fn description(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Visible => "visible",
            Self::Hidden => "hidden",
        }
    }
}

fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn pull_logs(app: &App, out: &Path) -> CliResult<Value> {
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
    let layout = layout_result
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
    let x_arg = key
        .get("tap_center_screen_x_px")
        .and_then(json_number_to_shell_arg)
        .ok_or_else(|| CliError::new("key is missing tap_center_screen_x_px"))?;
    let y_arg = key
        .get("tap_center_screen_y_px")
        .and_then(json_number_to_shell_arg)
        .ok_or_else(|| CliError::new("key is missing tap_center_screen_y_px"))?;
    let x = parse_tap_coordinate(&x_arg, "x")?;
    let y = parse_tap_coordinate(&y_arg, "y")?;

    let touch_output = controller::tap(app, TapSpec::new(x, y))?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "input_backend": "uinput",
        "key": key,
        "touch": touch_output,
    }))
}

fn parse_tap_coordinate(value: &str, axis: &str) -> CliResult<i32> {
    value.parse::<i32>().map_err(|error| {
        CliError::new(format!(
            "invalid {axis} tap coordinate from keyboard layout: {value}: {error}"
        ))
    })
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
