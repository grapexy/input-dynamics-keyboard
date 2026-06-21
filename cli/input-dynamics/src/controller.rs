//! Stateful local input controller for live uinput sessions.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::App;
use crate::error::{CliError, CliResult};
use crate::process::{
    FailureMode, StdinProcess, spawn_process_to_files, spawn_process_with_stdin_to_files,
};
use crate::uinput::{self, PathSpec, TapSpec};

const RUNTIME_DIR_ENV: &str = "INPUT_DYNAMICS_RUNTIME_DIR";
const START_TIMEOUT: Duration = Duration::from_secs(8);
const START_POLL_INTERVAL: Duration = Duration::from_millis(50);
const START_LOCK_STALE_MS: u128 = 120_000;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STOP_TAIL_MS: u64 = 100;

#[derive(Debug)]
pub(crate) struct RunConfig {
    pub(crate) socket: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) uinput_stdout: PathBuf,
    pub(crate) uinput_stderr: PathBuf,
    pub(crate) run_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimePaths {
    dir: PathBuf,
    device_serial: String,
    socket: PathBuf,
    state: PathBuf,
    session_lock: PathBuf,
    controller_stdout: PathBuf,
    controller_stderr: PathBuf,
    uinput_stdout: PathBuf,
    uinput_stderr: PathBuf,
}

#[derive(Debug)]
pub(crate) enum SessionStartPermit {
    Acquired(SessionStartLock),
    Busy(Value),
}

#[derive(Debug)]
pub(crate) struct SessionStartLock {
    path: PathBuf,
    package_name: String,
    device_serial: String,
    run_id: String,
    persist: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum ControllerRequest {
    Status,
    Tap { x: i32, y: i32, hold_ms: u64 },
    Path { spec: PathSpec },
    Stop,
}

pub(crate) fn acquire_session_start(app: &App, run_id: &str) -> CliResult<SessionStartPermit> {
    let paths = RuntimePaths::for_app(app)?;
    fs::create_dir_all(&paths.dir)?;
    remove_stale_runtime(&paths)?;

    let current_status = status(app)?;
    if value_bool(&current_status, "active") {
        return Ok(SessionStartPermit::Busy(session_busy(
            app,
            &paths,
            "input session is already active",
            &current_status,
        )));
    }

    match read_lock_json(&paths.session_lock) {
        Some(lock) if lock_is_recent(&lock) => {
            return Ok(SessionStartPermit::Busy(session_busy(
                app,
                &paths,
                "input session start is already in progress",
                &current_status,
            )));
        }
        Some(_) => remove_file_if_exists(&paths.session_lock)?,
        None => {}
    }

    let lock_json = initial_lock_json(app, &paths, run_id);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&paths.session_lock)
    {
        Ok(mut file) => {
            let json_text = serde_json::to_string_pretty(&lock_json)?;
            file.write_all(json_text.as_bytes())?;
            file.write_all(b"\n")?;
            Ok(SessionStartPermit::Acquired(SessionStartLock {
                path: paths.session_lock,
                package_name: String::from(app.package()),
                device_serial: paths.device_serial,
                run_id: String::from(run_id),
                persist: false,
            }))
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let race_status = status(app)?;
            Ok(SessionStartPermit::Busy(session_busy(
                app,
                &paths,
                "input session is already starting",
                &race_status,
            )))
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn clear_session_lock(app: &App) -> CliResult<()> {
    remove_file_if_exists(&RuntimePaths::for_app(app)?.session_lock)
}

pub(crate) fn start(app: &App, run_id: &str) -> CliResult<Value> {
    let paths = RuntimePaths::for_app(app)?;
    fs::create_dir_all(&paths.dir)?;
    remove_stale_runtime(&paths)?;

    let existing_status = status(app)?;
    if existing_status
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(json!({
            "ok": false,
            "package_name": app.package(),
            "device_serial": paths.device_serial.as_str(),
            "error": "input controller is already active",
            "controller": existing_status,
        }));
    }

    let executable = env::current_exe()?;
    let executable_text = path_string(&executable)?;
    let args = controller_args(app, &paths, run_id)?;
    let child = spawn_process_to_files(
        &executable_text,
        &args,
        &paths.controller_stdout,
        &paths.controller_stderr,
    )?;
    drop(child);

    wait_until_active(app, &paths, run_id)
}

pub(crate) fn status(app: &App) -> CliResult<Value> {
    let paths = RuntimePaths::for_app(app)?;
    match send_request(&paths.socket, &ControllerRequest::Status) {
        Ok(response) => Ok(json!({
            "ok": true,
            "active": response.get("ok").and_then(Value::as_bool).unwrap_or(false),
            "package_name": app.package(),
            "device_serial": paths.device_serial.as_str(),
            "runtime": paths_json(&paths),
            "state": read_state_json(&paths.state),
            "session_lock": read_lock_json(&paths.session_lock).unwrap_or(Value::Null),
            "controller": response,
        })),
        Err(error) => Ok(json!({
            "ok": true,
            "active": false,
            "package_name": app.package(),
            "device_serial": paths.device_serial.as_str(),
            "runtime": paths_json(&paths),
            "state": read_state_json(&paths.state),
            "session_lock": read_lock_json(&paths.session_lock).unwrap_or(Value::Null),
            "stale_runtime": paths.socket.exists() || paths.state.exists(),
            "controller_error": error.to_string(),
        })),
    }
}

pub(crate) fn stop(app: &App) -> CliResult<Value> {
    let paths = RuntimePaths::for_app(app)?;
    let before = status(app)?;
    let virtual_event_path = virtual_event_path_from_status(&before);
    if !before
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        remove_stale_runtime(&paths)?;
        let cleanup = cleanup_report(app, &paths, virtual_event_path.as_deref());
        return Ok(json!({
            "ok": true,
            "active": false,
            "package_name": app.package(),
            "device_serial": paths.device_serial.as_str(),
            "already_stopped": true,
            "before": before,
            "cleanup": cleanup,
        }));
    }

    let response = send_request(&paths.socket, &ControllerRequest::Stop)?;
    remove_stale_runtime(&paths)?;
    let cleanup = cleanup_report(app, &paths, virtual_event_path.as_deref());
    Ok(json!({
        "ok": response.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "active": false,
        "package_name": app.package(),
        "device_serial": paths.device_serial.as_str(),
        "already_stopped": false,
        "before": before,
        "controller": response,
        "cleanup": cleanup,
    }))
}

pub(crate) fn tap(app: &App, spec: TapSpec) -> CliResult<Value> {
    let paths = RuntimePaths::for_app(app)?;
    if !paths.socket.exists() {
        return Err(CliError::new(
            "no active input session; run `input-dynamics session start --run-id <id>`",
        ));
    }
    let response = send_request(
        &paths.socket,
        &ControllerRequest::Tap {
            x: spec.x,
            y: spec.y,
            hold_ms: spec.hold_ms,
        },
    )?;
    Ok(json!({
        "ok": response.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "input_backend": "uinput",
        "device_serial": paths.device_serial.as_str(),
        "controller": response,
    }))
}

pub(crate) fn path(app: &App, spec: PathSpec) -> CliResult<Value> {
    let paths = RuntimePaths::for_app(app)?;
    if !paths.socket.exists() {
        return Err(CliError::new(
            "no active input session; run `input-dynamics session start --run-id <id>`",
        ));
    }
    let response = send_request(&paths.socket, &ControllerRequest::Path { spec })?;
    Ok(json!({
        "ok": response.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "input_backend": "uinput",
        "device_serial": paths.device_serial.as_str(),
        "controller": response,
    }))
}

pub(crate) fn run(app: &App, config: &RunConfig) -> CliResult<Value> {
    remove_file_if_exists(&config.socket)?;
    let listener = UnixListener::bind(&config.socket)?;
    let before_profiles = uinput::discover_touchscreen_profiles(app)?;
    let profile = uinput::select_primary_touchscreen_profile(&before_profiles)?;
    let mut uinput_process = start_uinput_process(app, config)?;
    write_uinput_line(&mut uinput_process, &uinput::register_line(&profile)?)?;
    write_uinput_line(
        &mut uinput_process,
        &uinput::delay_line(uinput::DEVICE_SETTLE_MS)?,
    )?;
    thread::sleep(Duration::from_millis(uinput::DEVICE_SETTLE_MS));
    ensure_uinput_alive(&mut uinput_process)?;

    let virtual_touchscreen = virtual_touchscreen_report(app, &profile, &before_profiles);
    let state = controller_state(app, config, &profile, &virtual_touchscreen)?;
    write_json_file(&config.state, &state)?;

    let mut stopped = false;
    for stream_result in listener.incoming() {
        let stream = stream_result?;
        if handle_stream(stream, &mut uinput_process, &profile)? {
            stopped = true;
            break;
        }
    }

    shutdown_uinput(uinput_process)?;
    remove_file_if_exists(&config.socket)?;
    remove_file_if_exists(&config.state)?;

    Ok(json!({
        "ok": true,
        "stopped": stopped,
        "package_name": app.package(),
        "device_serial": app.selected_device_serial()?,
    }))
}

fn controller_args(app: &App, paths: &RuntimePaths, run_id: &str) -> CliResult<Vec<String>> {
    Ok(vec![
        String::from("--adb"),
        String::from(app.adb_program()),
        String::from("--package"),
        String::from(app.package()),
        String::from("--serial"),
        paths.device_serial.clone(),
        String::from("controller"),
        String::from("run"),
        String::from("--socket"),
        path_string(&paths.socket)?,
        String::from("--state"),
        path_string(&paths.state)?,
        String::from("--uinput-stdout"),
        path_string(&paths.uinput_stdout)?,
        String::from("--uinput-stderr"),
        path_string(&paths.uinput_stderr)?,
        String::from("--run-id"),
        String::from(run_id),
    ])
}

fn wait_until_active(app: &App, paths: &RuntimePaths, run_id: &str) -> CliResult<Value> {
    let start_time = Instant::now();
    loop {
        let current = status(app)?;
        if current
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(json!({
                "ok": true,
                "active": true,
                "package_name": app.package(),
                "device_serial": paths.device_serial.as_str(),
                "run_id": run_id,
                "runtime": paths_json(paths),
                "controller": current,
            }));
        }
        if start_time.elapsed() >= START_TIMEOUT {
            return Ok(json!({
                "ok": false,
                "active": false,
                "package_name": app.package(),
                "device_serial": paths.device_serial.as_str(),
                "run_id": run_id,
                "runtime": paths_json(paths),
                "error": "timed out waiting for input controller to start",
                "controller": current,
            }));
        }
        thread::sleep(START_POLL_INTERVAL);
    }
}

fn handle_stream(
    mut stream: UnixStream,
    uinput_process: &mut StdinProcess,
    profile: &uinput::TouchscreenProfile,
) -> CliResult<bool> {
    let mut request_text = String::new();
    stream.read_to_string(&mut request_text)?;
    let request: ControllerRequest = serde_json::from_str(request_text.trim())?;
    let response = handle_request(&request, uinput_process, profile)?;
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(matches!(request, ControllerRequest::Stop))
}

fn handle_request(
    request: &ControllerRequest,
    uinput_process: &mut StdinProcess,
    profile: &uinput::TouchscreenProfile,
) -> CliResult<Value> {
    ensure_uinput_alive(uinput_process)?;
    match *request {
        ControllerRequest::Status => Ok(json!({
            "ok": true,
            "active": true,
            "input_backend": "uinput",
            "input_device_command": uinput::input_device_command(),
        })),
        ControllerRequest::Tap { x, y, hold_ms } => {
            let spec = TapSpec { x, y, hold_ms };
            for line in uinput::tap_lines(profile, spec)? {
                write_uinput_line(uinput_process, &line)?;
            }
            Ok(json!({
                "ok": true,
                "active": true,
                "input_backend": "uinput",
                "tap": {
                    "x": x,
                    "y": y,
                    "hold_ms": hold_ms,
                },
            }))
        }
        ControllerRequest::Path { ref spec } => {
            for line in uinput::path_lines(profile, spec)? {
                write_uinput_line(uinput_process, &line)?;
            }
            Ok(json!({
                "ok": true,
                "active": true,
                "input_backend": "uinput",
                "path": {
                    "points": spec.points,
                    "point_count": spec.points.len(),
                    "duration_ms": spec.duration_ms,
                },
            }))
        }
        ControllerRequest::Stop => {
            write_uinput_line(uinput_process, &uinput::delay_line(STOP_TAIL_MS)?)?;
            Ok(json!({
                "ok": true,
                "active": false,
                "input_backend": "uinput",
                "stopping": true,
            }))
        }
    }
}

fn send_request(socket: &Path, request: &ControllerRequest) -> CliResult<Value> {
    let mut stream = UnixStream::connect(socket)?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)?;
    let mut response_text = String::new();
    stream.read_to_string(&mut response_text)?;
    Ok(serde_json::from_str(response_text.trim())?)
}

fn start_uinput_process(app: &App, config: &RunConfig) -> CliResult<StdinProcess> {
    let args = vec![
        String::from("shell"),
        String::from("uinput"),
        String::from("-"),
    ];
    let scoped_args = app.scoped_adb_args(&args)?;
    spawn_process_with_stdin_to_files(
        app.adb_program(),
        &scoped_args,
        &config.uinput_stdout,
        &config.uinput_stderr,
    )
}

fn write_uinput_line(process: &mut StdinProcess, line: &str) -> CliResult<()> {
    process.stdin_mut().write_all(line.as_bytes())?;
    process.stdin_mut().write_all(b"\n")?;
    process.stdin_mut().flush()?;
    Ok(())
}

fn ensure_uinput_alive(process: &mut StdinProcess) -> CliResult<()> {
    if let Some(status) = process.try_wait()? {
        return Err(CliError::new(format!(
            "adb uinput process exited unexpectedly with status {status}"
        )));
    }
    Ok(())
}

fn shutdown_uinput(process: StdinProcess) -> CliResult<()> {
    let status = process.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::new(format!(
            "adb uinput process exited with status {status}"
        )))
    }
}

fn controller_state(
    app: &App,
    config: &RunConfig,
    profile: &uinput::TouchscreenProfile,
    virtual_touchscreen: &Value,
) -> CliResult<Value> {
    Ok(json!({
        "schema": "input_dynamics_controller_state.v1",
        "active": true,
        "pid": process::id(),
        "package_name": app.package(),
        "device_serial": app.selected_device_serial()?,
        "run_id": config.run_id,
        "socket_path": path_string_lossy(&config.socket),
        "state_path": path_string_lossy(&config.state),
        "started_wall_ms": wall_time_ms(),
        "input_backend": "uinput",
        "input_device_command": uinput::input_device_command(),
        "physical_touchscreen_profile_hash": uinput::profile_hash(profile)?,
        "physical_touchscreen": uinput::profile_summary(profile),
        "virtual_touchscreen": virtual_touchscreen,
    }))
}

fn virtual_touchscreen_report(
    app: &App,
    physical: &uinput::TouchscreenProfile,
    before_profiles: &[uinput::TouchscreenProfile],
) -> Value {
    let after_profiles = match uinput::discover_touchscreen_profiles(app) {
        Ok(profiles) => profiles,
        Err(error) => {
            return json!({
                "detected": false,
                "error": error.to_string(),
            });
        }
    };
    let Some(virtual_profile) =
        uinput::find_new_mirrored_touchscreen(before_profiles, &after_profiles, physical)
    else {
        return json!({
            "detected": false,
            "candidate_count": after_profiles.len().saturating_sub(before_profiles.len()),
            "after_touchscreen_count": after_profiles.len(),
        });
    };
    json!({
        "detected": true,
        "profile_hash": uinput::profile_hash(&virtual_profile).ok(),
        "profile": uinput::profile_summary(&virtual_profile),
        "framework": framework_input_device_report(app, virtual_profile.event_path()),
    })
}

fn framework_input_device_report(app: &App, event_path: &str) -> Value {
    let output = match app.adb_shell(
        vec![String::from("dumpsys"), String::from("input")],
        FailureMode::AllowFailure,
    ) {
        Ok(output) => output,
        Err(error) => {
            return json!({
                "ok": false,
                "event_path": event_path,
                "error": error.to_string(),
            });
        }
    };
    if output.status_code != Some(0_i32) {
        return json!({
            "ok": false,
            "event_path": event_path,
            "process": output.json(),
        });
    }
    let parsed = parse_dumpsys_input(output.stdout());
    let Some(event_hub) = parsed
        .event_hub_devices
        .iter()
        .find(|device| device.path.as_deref() == Some(event_path))
    else {
        return json!({
            "ok": false,
            "event_path": event_path,
            "error": "event path was not found in dumpsys input",
        });
    };
    let input_reader = parsed
        .reader_devices
        .iter()
        .find(|device| device.event_hub_ids.contains(&event_hub.id));
    json!({
        "ok": true,
        "event_path": event_path,
        "event_hub": event_hub.to_json(),
        "input_reader": input_reader.map(InputReaderDevice::to_json),
    })
}

fn cleanup_report(app: &App, paths: &RuntimePaths, virtual_event_path: Option<&str>) -> Value {
    json!({
        "runtime": wait_for_runtime_cleanup(paths),
        "virtual_touchscreen": wait_for_virtual_touchscreen_cleanup(app, virtual_event_path),
    })
}

fn virtual_event_path_from_status(status: &Value) -> Option<String> {
    [
        "/state/virtual_touchscreen/profile/event_path",
        "/session_lock/controller_state/virtual_touchscreen/profile/event_path",
    ]
    .into_iter()
    .find_map(|pointer| status.pointer(pointer).and_then(Value::as_str))
    .map(String::from)
}

fn wait_for_runtime_cleanup(paths: &RuntimePaths) -> Value {
    let start = Instant::now();
    loop {
        let socket_exists = paths.socket.exists();
        let state_exists = paths.state.exists();
        if !socket_exists && !state_exists {
            return json!({
                "ok": true,
                "socket_exists": false,
                "state_exists": false,
                "elapsed_ms": millis_u64(start.elapsed()),
            });
        }
        if start.elapsed() >= CLEANUP_TIMEOUT {
            return json!({
                "ok": false,
                "socket_exists": socket_exists,
                "state_exists": state_exists,
                "elapsed_ms": millis_u64(start.elapsed()),
                "timeout_ms": millis_u64(CLEANUP_TIMEOUT),
            });
        }
        thread::sleep(CLEANUP_POLL_INTERVAL);
    }
}

fn wait_for_virtual_touchscreen_cleanup(app: &App, maybe_event_path: Option<&str>) -> Value {
    let Some(event_path) = maybe_event_path else {
        return json!({
            "ok": true,
            "skipped": true,
            "reason": "virtual event path was not detected",
        });
    };
    let start = Instant::now();
    loop {
        match uinput::touchscreen_event_path_exists(app, event_path) {
            Ok(false) => {
                return json!({
                    "ok": true,
                    "event_path": event_path,
                    "present": false,
                    "elapsed_ms": millis_u64(start.elapsed()),
                });
            }
            Ok(true) => {}
            Err(error) => {
                return json!({
                    "ok": false,
                    "event_path": event_path,
                    "error": error.to_string(),
                });
            }
        }
        if start.elapsed() >= CLEANUP_TIMEOUT {
            return json!({
                "ok": false,
                "event_path": event_path,
                "present": true,
                "elapsed_ms": millis_u64(start.elapsed()),
                "timeout_ms": millis_u64(CLEANUP_TIMEOUT),
            });
        }
        thread::sleep(CLEANUP_POLL_INTERVAL);
    }
}

#[derive(Debug, Default)]
struct ParsedInputDumpsys {
    event_hub_devices: Vec<EventHubDevice>,
    reader_devices: Vec<InputReaderDevice>,
}

#[derive(Debug, Default)]
struct EventHubDevice {
    id: i64,
    name: String,
    classes: Option<String>,
    path: Option<String>,
    enabled: Option<bool>,
    descriptor: Option<String>,
    location: Option<String>,
    unique_id: Option<String>,
    identifier: Option<String>,
    video_device: Option<String>,
    sysfs_device_path: Option<String>,
}

#[derive(Debug, Default)]
struct InputReaderDevice {
    id: i64,
    name: String,
    event_hub_ids: Vec<i64>,
    is_virtual_device: Option<bool>,
    sources: Option<String>,
    sysfs_root_path: Option<String>,
}

impl EventHubDevice {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "classes": self.classes,
            "path": self.path,
            "enabled": self.enabled,
            "descriptor": self.descriptor,
            "location": self.location,
            "unique_id": self.unique_id,
            "identifier": self.identifier,
            "video_device": self.video_device,
            "sysfs_device_path": self.sysfs_device_path,
        })
    }
}

impl InputReaderDevice {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "event_hub_ids": self.event_hub_ids,
            "is_virtual_device": self.is_virtual_device,
            "sources": self.sources,
            "sysfs_root_path": self.sysfs_root_path,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DumpsysSection {
    None,
    EventHub,
    InputReader,
}

fn parse_dumpsys_input(text: &str) -> ParsedInputDumpsys {
    let mut parsed = ParsedInputDumpsys::default();
    let mut section = DumpsysSection::None;
    let mut current_event_hub = None;
    let mut current_reader = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "Event Hub State:" {
            push_current_event_hub(&mut parsed, &mut current_event_hub);
            push_current_reader(&mut parsed, &mut current_reader);
            section = DumpsysSection::EventHub;
            continue;
        }
        if trimmed.starts_with("Input Reader State") {
            push_current_event_hub(&mut parsed, &mut current_event_hub);
            push_current_reader(&mut parsed, &mut current_reader);
            section = DumpsysSection::InputReader;
            continue;
        }

        match section {
            DumpsysSection::EventHub => {
                if let Some((id, name)) = parse_event_hub_header(line) {
                    push_current_event_hub(&mut parsed, &mut current_event_hub);
                    current_event_hub = Some(EventHubDevice {
                        id,
                        name,
                        ..EventHubDevice::default()
                    });
                    continue;
                }
                if let Some(device) = current_event_hub.as_mut() {
                    parse_event_hub_property(device, trimmed);
                }
            }
            DumpsysSection::InputReader => {
                if let Some((id, name)) = parse_input_reader_header(trimmed) {
                    push_current_reader(&mut parsed, &mut current_reader);
                    current_reader = Some(InputReaderDevice {
                        id,
                        name,
                        ..InputReaderDevice::default()
                    });
                    continue;
                }
                if let Some(device) = current_reader.as_mut() {
                    parse_input_reader_property(device, trimmed);
                }
            }
            DumpsysSection::None => {}
        }
    }

    push_current_event_hub(&mut parsed, &mut current_event_hub);
    push_current_reader(&mut parsed, &mut current_reader);
    parsed
}

fn push_current_event_hub(parsed: &mut ParsedInputDumpsys, current: &mut Option<EventHubDevice>) {
    if let Some(device) = current.take() {
        parsed.event_hub_devices.push(device);
    }
}

fn push_current_reader(parsed: &mut ParsedInputDumpsys, current: &mut Option<InputReaderDevice>) {
    if let Some(device) = current.take() {
        parsed.reader_devices.push(device);
    }
}

fn parse_event_hub_header(line: &str) -> Option<(i64, String)> {
    if !line.starts_with("    ") || line.starts_with("      ") {
        return None;
    }
    parse_id_name_header(line.trim())
}

fn parse_input_reader_header(trimmed: &str) -> Option<(i64, String)> {
    let rest = trimmed.strip_prefix("Device ")?;
    parse_id_name_header(rest)
}

fn parse_id_name_header(text: &str) -> Option<(i64, String)> {
    let (id_text, name_text) = text.split_once(':')?;
    let id = id_text.trim().parse::<i64>().ok()?;
    let name = name_text.trim();
    if name.is_empty() {
        return None;
    }
    Some((id, String::from(name)))
}

fn parse_event_hub_property(device: &mut EventHubDevice, trimmed: &str) {
    if let Some(value) = labeled_value(trimmed, "Classes") {
        device.classes = Some(value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "Path") {
        device.path = Some(value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "Enabled") {
        device.enabled = parse_bool(&value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "Descriptor") {
        device.descriptor = Some(value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "Location") {
        device.location = Some(value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "UniqueId") {
        device.unique_id = Some(value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "Identifier") {
        device.identifier = Some(value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "VideoDevice") {
        device.video_device = Some(value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "SysfsDevicePath") {
        device.sysfs_device_path = Some(value);
    }
}

fn parse_input_reader_property(device: &mut InputReaderDevice, trimmed: &str) {
    if let Some(value) = labeled_value(trimmed, "EventHub Devices") {
        device.event_hub_ids = parse_i64_list(&value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "IsVirtualDevice") {
        device.is_virtual_device = parse_bool(&value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "Sources") {
        device.sources = Some(value);
        return;
    }
    if let Some(value) = labeled_value(trimmed, "SysfsRootPath") {
        device.sysfs_root_path = Some(value);
    }
}

fn labeled_value(trimmed: &str, label: &str) -> Option<String> {
    trimmed
        .strip_prefix(label)
        .and_then(|value| value.strip_prefix(':'))
        .map(str::trim)
        .map(String::from)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_i64_list(value: &str) -> Vec<i64> {
    value
        .trim_matches(|character| matches!(character, '[' | ']'))
        .split_whitespace()
        .filter_map(|part| part.parse::<i64>().ok())
        .collect()
}

fn write_json_file(path: &Path, value: &Value) -> CliResult<()> {
    let json_text = serde_json::to_string_pretty(value)?;
    fs::write(path, format!("{json_text}\n"))?;
    Ok(())
}

fn read_state_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(text.trim()).ok())
        .unwrap_or(Value::Null)
}

fn remove_stale_runtime(paths: &RuntimePaths) -> CliResult<()> {
    if paths.socket.exists() && send_request(&paths.socket, &ControllerRequest::Status).is_err() {
        remove_file_if_exists(&paths.socket)?;
    }
    if paths.state.exists() && !paths.socket.exists() {
        remove_file_if_exists(&paths.state)?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> CliResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn paths_json(paths: &RuntimePaths) -> Value {
    json!({
        "dir": path_string_lossy(&paths.dir),
        "device_serial": paths.device_serial.as_str(),
        "socket": path_string_lossy(&paths.socket),
        "state": path_string_lossy(&paths.state),
        "session_lock": path_string_lossy(&paths.session_lock),
        "controller_stdout": path_string_lossy(&paths.controller_stdout),
        "controller_stderr": path_string_lossy(&paths.controller_stderr),
        "uinput_stdout": path_string_lossy(&paths.uinput_stdout),
        "uinput_stderr": path_string_lossy(&paths.uinput_stderr),
    })
}

impl RuntimePaths {
    fn for_app(app: &App) -> CliResult<Self> {
        let device_serial = app.selected_device_serial()?;
        Ok(Self::from_base_dir(
            runtime_base_dir(),
            app.package(),
            &device_serial,
        ))
    }

    fn from_base_dir(base_dir: PathBuf, package: &str, device_serial: &str) -> Self {
        let prefix = format!(
            "{}.{}",
            sanitize_path_component(package),
            sanitize_path_component(device_serial)
        );
        Self {
            device_serial: String::from(device_serial),
            socket: runtime_file(&base_dir, &prefix, "sock"),
            state: runtime_file(&base_dir, &prefix, "state.json"),
            session_lock: runtime_file(&base_dir, &prefix, "session.lock.json"),
            controller_stdout: runtime_file(&base_dir, &prefix, "controller.stdout.log"),
            controller_stderr: runtime_file(&base_dir, &prefix, "controller.stderr.log"),
            uinput_stdout: runtime_file(&base_dir, &prefix, "uinput.stdout.log"),
            uinput_stderr: runtime_file(&base_dir, &prefix, "uinput.stderr.log"),
            dir: base_dir,
        }
    }
}

fn runtime_file(base_dir: &Path, prefix: &str, suffix: &str) -> PathBuf {
    base_dir.join(format!("{prefix}.{suffix}"))
}

fn runtime_base_dir() -> PathBuf {
    env::var_os(RUNTIME_DIR_ENV).map_or_else(
        || PathBuf::from(format!("/tmp/input-dynamics-{}", user_name())),
        PathBuf::from,
    )
}

fn user_name() -> String {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| String::from("unknown"))
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        String::from("default")
    } else {
        sanitized
    }
}

fn path_string(path: &Path) -> CliResult<String> {
    path.to_str()
        .map(String::from)
        .ok_or_else(|| CliError::new(format!("path is not valid UTF-8: {}", path.display())))
}

fn path_string_lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn wall_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_millis())
}

fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl SessionStartLock {
    pub(crate) fn activate(&mut self, input_status: &Value) -> CliResult<()> {
        let active_lock = json!({
            "schema": "input_dynamics_session_lock.v1",
            "state": "active",
            "package_name": self.package_name,
            "device_serial": self.device_serial.as_str(),
            "run_id": self.run_id,
            "owner_pid": process::id(),
            "created_wall_ms": read_lock_json(&self.path)
                .and_then(|lock| lock.get("created_wall_ms").cloned())
                .unwrap_or_else(|| json!(wall_time_ms())),
            "activated_wall_ms": wall_time_ms(),
            "lock_path": path_string_lossy(&self.path),
            "input_active": value_bool(input_status, "active"),
            "controller_state": input_status
                .pointer("/controller/state")
                .cloned()
                .unwrap_or(Value::Null),
        });
        write_json_file(&self.path, &active_lock)?;
        self.persist = true;
        Ok(())
    }
}

impl Drop for SessionStartLock {
    fn drop(&mut self) {
        if !self.persist {
            let _remove_result = remove_file_if_exists(&self.path);
        }
    }
}

fn initial_lock_json(app: &App, paths: &RuntimePaths, run_id: &str) -> Value {
    json!({
        "schema": "input_dynamics_session_lock.v1",
        "state": "starting",
        "package_name": app.package(),
        "device_serial": paths.device_serial.as_str(),
        "run_id": run_id,
        "owner_pid": process::id(),
        "created_wall_ms": wall_time_ms(),
        "lock_path": path_string_lossy(&paths.session_lock),
        "runtime": paths_json(paths),
    })
}

fn read_lock_json(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(text.trim()).ok())
}

fn lock_is_recent(lock: &Value) -> bool {
    let Some(created_wall_ms) = lock.get("created_wall_ms").and_then(Value::as_u64) else {
        return false;
    };
    wall_time_ms().saturating_sub(u128::from(created_wall_ms)) <= START_LOCK_STALE_MS
}

fn session_busy(app: &App, paths: &RuntimePaths, message: &str, current_status: &Value) -> Value {
    json!({
        "ok": false,
        "active": value_bool(current_status, "active"),
        "busy": true,
        "package_name": app.package(),
        "error": message,
        "runtime": paths_json(paths),
        "session_lock": read_lock_json(&paths.session_lock).unwrap_or(Value::Null),
        "controller": current_status,
    })
}

fn value_bool(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use proptest::strategy::Strategy;

    use crate::controller::{
        RuntimePaths, parse_dumpsys_input, parse_i64_list, sanitize_path_component,
        virtual_event_path_from_status,
    };

    const SAMPLE_DUMPSYS_INPUT: &str = r"
Event Hub State:
  Devices:
    1: sec_touchscreen
      Classes: KEYBOARD | TOUCH | TOUCH_MT
      Path: /dev/input/event3
      Enabled: true
      Descriptor: 48ee483b8b70f8343840706d561e5b8d5cb64b8c
      Location: sec_touchscreen/input1
      UniqueId: google_touchscreen
      Identifier: bus=0x001c, vendor=0x0000, product=0x0000, version=0x0000, bluetoothAddress=<not set>
      VideoDevice: Video device sec_touchscreen (/dev/v4l-touch0) : height=39, width=18, fd=254, hasValidFd=true
      SysfsDevicePath: /sys/devices/platform/mock-touch
    5: sec_touchscreen
      Classes: KEYBOARD | TOUCH | TOUCH_MT
      Path: /dev/input/event4
      Enabled: true
      Descriptor: virtual-descriptor
      Location: sec_touchscreen/input1
      UniqueId: <not set>
      Identifier: bus=0x001c, vendor=0x0000, product=0x0000, version=0x0000, bluetoothAddress=<not set>
      SysfsDevicePath: /sys/devices/virtual/input/input42
Input Reader State (Nums of device: 4):
  Device 4: sec_touchscreen
    EventHub Devices: [ 1 ]
    IsVirtualDevice: false
    Sources: KEYBOARD | TOUCHSCREEN
    SysfsRootPath:     /sys/devices/platform/mock-touch
  Device 7: sec_touchscreen
    EventHub Devices: [ 5 ]
    IsVirtualDevice: true
    Sources: KEYBOARD | TOUCHSCREEN
    SysfsRootPath:     /sys/devices/virtual/input/input42
";

    #[test]
    fn runtime_paths_include_sanitized_package_and_serial() {
        let base = PathBuf::from("/tmp/input-dynamics-test");
        let paths = RuntimePaths::from_base_dir(base, "org.inputdynamics.ime/debug", "device/123");

        assert!(
            paths
                .socket
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.contains("org.inputdynamics.ime_debug.device_123.sock")),
            "socket path should include sanitized package name and device serial"
        );
    }

    #[test]
    fn parses_dumpsys_input_event_hub_and_input_reader_links() {
        let parsed = parse_dumpsys_input(SAMPLE_DUMPSYS_INPUT);

        let event_hub = parsed
            .event_hub_devices
            .iter()
            .find(|device| device.path.as_deref() == Some("/dev/input/event4"));
        assert!(
            event_hub.is_some(),
            "virtual event path should be parsed from Event Hub state"
        );
        let event_hub_id = event_hub.map_or(0_i64, |device| device.id);
        assert_eq!(event_hub_id, 5, "Event Hub id should be parsed");

        let reader = parsed
            .reader_devices
            .iter()
            .find(|device| device.event_hub_ids.contains(&event_hub_id));
        assert!(
            reader.is_some(),
            "Input Reader device should link back to Event Hub id"
        );
        assert_eq!(reader.map(|device| device.id), Some(7));
        assert_eq!(
            reader.and_then(|device| device.is_virtual_device),
            Some(true)
        );
    }

    #[test]
    fn parses_framework_id_list() {
        assert_eq!(parse_i64_list("[ 1 5 ]"), vec![1, 5]);
        assert_eq!(parse_i64_list("[ ]"), Vec::<i64>::new());
    }

    #[test]
    fn virtual_event_path_prefers_live_state() {
        let status = serde_json::json!({
            "state": {
                "virtual_touchscreen": {
                    "profile": {
                        "event_path": "/dev/input/event4"
                    }
                }
            },
            "session_lock": {
                "controller_state": {
                    "virtual_touchscreen": {
                        "profile": {
                            "event_path": "/dev/input/event5"
                        }
                    }
                }
            }
        });

        assert_eq!(
            virtual_event_path_from_status(&status),
            Some(String::from("/dev/input/event4")),
            "live state should be preferred over lock fallback"
        );
    }

    #[test]
    fn virtual_event_path_falls_back_to_session_lock() {
        let status = serde_json::json!({
            "state": null,
            "session_lock": {
                "controller_state": {
                    "virtual_touchscreen": {
                        "profile": {
                            "event_path": "/dev/input/event5"
                        }
                    }
                }
            }
        });

        assert_eq!(
            virtual_event_path_from_status(&status),
            Some(String::from("/dev/input/event5")),
            "stale lock state should still support cleanup reporting"
        );
    }

    proptest::proptest! {
        #[test]
        fn sanitized_path_component_has_only_safe_ascii(input in path_component_input()) {
            let sanitized = sanitize_path_component(&input);

            proptest::prop_assert!(
                !sanitized.is_empty(),
                "sanitized path component should never be empty"
            );
            proptest::prop_assert!(
                sanitized
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || matches!(character, '.' | '-' | '_')),
                "sanitized path component should contain only safe ASCII filename characters"
            );
        }
    }

    fn path_component_input() -> impl Strategy<Value = String> {
        proptest::collection::vec(any_char_strategy(), 0..64)
            .prop_map(|characters| characters.into_iter().collect())
    }

    fn any_char_strategy() -> impl Strategy<Value = char> {
        proptest::char::range('\u{0}', '\u{7f}')
    }
}
