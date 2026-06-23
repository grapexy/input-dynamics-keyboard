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
use crate::profile::{
    self, InterKeyDelaySampling, KeyProfileContext, ProfileGenerator, RuntimeProfile,
};
use crate::uinput::{self, PathSpec, TapSpec};

const RUNTIME_DIR_ENV: &str = "INPUT_DYNAMICS_RUNTIME_DIR";
const START_TIMEOUT: Duration = Duration::from_secs(8);
const START_POLL_INTERVAL: Duration = Duration::from_millis(50);
const START_LOCK_STALE_MS: u128 = 120_000;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TAIL_MS: u64 = 100;
const DIAGNOSTIC_RETENTION_COUNT: usize = 8;
const RUN_ID_FRAGMENT_MAX_CHARS: usize = 64;
const EVENT_SCHEMA: &str = "input_dynamics_controller_event.v1";
const CURRENT_SCHEMA: &str = "input_dynamics_controller_current.v1";
const MANIFEST_SCHEMA: &str = "input_dynamics_controller_invocation.v1";

#[derive(Debug)]
pub(crate) struct RunConfig {
    pub(crate) socket: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) uinput_stdout: PathBuf,
    pub(crate) uinput_stderr: PathBuf,
    pub(crate) events: PathBuf,
    pub(crate) final_state: PathBuf,
    pub(crate) controller_invocation_id: String,
    pub(crate) run_id: String,
    pub(crate) input_profile: Option<RuntimeProfile>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ControllerTapSpec {
    pub(crate) fallback: TapSpec,
    pub(crate) key_context: Option<KeyProfileContext>,
    pub(crate) inter_key_delay_sampling: InterKeyDelaySampling,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimePaths {
    dir: PathBuf,
    package_name: String,
    device_serial: String,
    socket: PathBuf,
    state: PathBuf,
    session_lock: PathBuf,
    current: PathBuf,
    runs_dir: PathBuf,
    controller_stdout: PathBuf,
    controller_stderr: PathBuf,
    uinput_stdout: PathBuf,
    uinput_stderr: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ControllerInvocation {
    id: String,
    dir: PathBuf,
    manifest: PathBuf,
    events: PathBuf,
    controller_stdout: PathBuf,
    controller_stderr: PathBuf,
    uinput_stdout: PathBuf,
    uinput_stderr: PathBuf,
    final_state: PathBuf,
    final_session_lock: PathBuf,
}

#[derive(Clone, Debug)]
struct ControllerEventLog {
    path: PathBuf,
    package_name: String,
    device_serial: String,
    run_id: String,
    controller_invocation_id: String,
    source: &'static str,
    pid: u32,
    started: Instant,
}

#[derive(Debug, Eq, PartialEq)]
enum ResponseWriteOutcome {
    Delivered,
    Abandoned { error_kind: String, error: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvocationActive {
    Active,
    Inactive,
}

#[derive(Clone, Copy, Debug)]
struct InvocationMetadata<'a> {
    package_name: &'a str,
    device_serial: &'a str,
    run_id: &'a str,
    input_profile: Option<&'a RuntimeProfile>,
    controller_pid: Option<u32>,
    active: InvocationActive,
}

struct ControllerRequestContext<'a> {
    uinput_process: &'a mut StdinProcess,
    profile: &'a uinput::TouchscreenProfile,
    generator: Option<&'a mut ProfileGenerator>,
    runtime_state: &'a mut ControllerRuntimeState,
    event_log: &'a ControllerEventLog,
}

impl InvocationActive {
    const fn as_bool(self) -> bool {
        match self {
            Self::Active => true,
            Self::Inactive => false,
        }
    }
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
    Tap {
        fallback: TapSpec,
        key_context: Option<KeyProfileContext>,
        inter_key_delay_sampling: InterKeyDelaySampling,
    },
    Path {
        spec: PathSpec,
    },
    Stop,
}

impl ControllerRequest {
    const fn name(&self) -> &'static str {
        match *self {
            Self::Status => "status",
            Self::Tap { .. } => "tap",
            Self::Path { .. } => "path",
            Self::Stop => "stop",
        }
    }

    const fn tracked(&self) -> bool {
        !matches!(*self, Self::Status)
    }

    fn summary_json(&self) -> Value {
        match *self {
            Self::Status | Self::Stop => json!({
                "command": self.name(),
            }),
            Self::Tap { fallback, .. } => json!({
                "command": self.name(),
                "tap": {
                    "x": fallback.x,
                    "y": fallback.y,
                    "hold_ms": fallback.hold_ms,
                    "pressure": fallback.pressure,
                    "touch_major_px": fallback.touch_major_px,
                    "touch_minor_px": fallback.touch_minor_px,
                    "orientation": fallback.orientation,
                },
            }),
            Self::Path { ref spec } => json!({
                "command": self.name(),
                "path": {
                    "point_count": spec.points.len(),
                    "duration_ms": spec.duration_ms,
                    "first": spec.points.first().copied().map(touch_point_json),
                    "last": spec.points.last().copied().map(touch_point_json),
                },
            }),
        }
    }
}

#[derive(Clone, Debug)]
struct ControllerCommandMark {
    sequence: u64,
    command_name: &'static str,
    started_wall_ms: u128,
    started: Instant,
}

#[derive(Debug)]
struct ControllerRuntimeState {
    path: PathBuf,
    value: Value,
    sequence: u64,
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
    let paths = RuntimePaths::for_app(app)?;
    preserve_session_lock(&paths);
    mark_current_inactive(&paths);
    remove_file_if_exists(&paths.session_lock)
}

impl ControllerTapSpec {
    pub(crate) const fn profiled_key(
        fallback: TapSpec,
        key_context: KeyProfileContext,
        inter_key_delay_sampling: InterKeyDelaySampling,
    ) -> Self {
        Self {
            fallback,
            key_context: Some(key_context),
            inter_key_delay_sampling,
        }
    }
}

pub(crate) fn start(
    app: &App,
    run_id: &str,
    input_profile: Option<&RuntimeProfile>,
) -> CliResult<Value> {
    let paths = RuntimePaths::for_app(app)?;
    fs::create_dir_all(&paths.dir)?;
    fs::create_dir_all(&paths.runs_dir)?;
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

    prune_old_invocations(&paths)?;
    let invocation = ControllerInvocation::new(&paths, run_id);
    fs::create_dir_all(&invocation.dir)?;
    let metadata = InvocationMetadata {
        package_name: app.package(),
        device_serial: paths.device_serial.as_str(),
        run_id,
        input_profile,
        controller_pid: None,
        active: InvocationActive::Active,
    };
    write_invocation_manifest(&paths, &invocation, metadata)?;
    write_current_invocation(&paths, &invocation, metadata)?;
    let start_event_log =
        ControllerEventLog::client_from_invocation(app, &paths, &invocation, run_id);
    start_event_log.append(
        "controller_spawn_start",
        json!({
            "controller_stdout": path_string_lossy(&invocation.controller_stdout),
            "controller_stderr": path_string_lossy(&invocation.controller_stderr),
            "uinput_stdout": path_string_lossy(&invocation.uinput_stdout),
            "uinput_stderr": path_string_lossy(&invocation.uinput_stderr),
        }),
    );

    let executable = env::current_exe()?;
    let executable_text = path_string(&executable)?;
    let args = controller_args(app, &paths, &invocation, run_id, input_profile)?;
    let child = spawn_process_to_files(
        &executable_text,
        &args,
        &invocation.controller_stdout,
        &invocation.controller_stderr,
    )?;
    let child_pid = child.id();
    let spawned_metadata = InvocationMetadata {
        controller_pid: Some(child_pid),
        ..metadata
    };
    write_invocation_manifest(&paths, &invocation, spawned_metadata)?;
    write_current_invocation(&paths, &invocation, spawned_metadata)?;
    start_event_log.append(
        "controller_spawn_done",
        json!({
            "controller_pid": child_pid,
        }),
    );
    drop(child);

    wait_until_active(app, &paths, run_id)
}

pub(crate) fn status(app: &App) -> CliResult<Value> {
    let paths = RuntimePaths::for_app(app)?;
    match send_request(&paths, &ControllerRequest::Status) {
        Ok(response) => {
            let active = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let session_lock = read_lock_json(&paths.session_lock).unwrap_or(Value::Null);
            Ok(json!({
            "ok": true,
            "active": active,
            "ready_for_input": active && session_lock_ready(&session_lock),
            "package_name": app.package(),
            "device_serial": paths.device_serial.as_str(),
            "runtime": paths_json(&paths),
            "state": read_state_json(&paths.state),
            "session_lock": session_lock,
            "controller": response,
            }))
        }
        Err(error) => Ok(json!({
            "ok": true,
            "active": false,
            "ready_for_input": false,
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

    let response = send_request(&paths, &ControllerRequest::Stop)?;
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

pub(crate) fn tap(app: &App, spec: ControllerTapSpec) -> CliResult<Value> {
    let paths = RuntimePaths::for_app(app)?;
    ensure_ready_for_input(app, &paths)?;
    let response = send_request(
        &paths,
        &ControllerRequest::Tap {
            fallback: spec.fallback,
            key_context: spec.key_context,
            inter_key_delay_sampling: spec.inter_key_delay_sampling,
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
    ensure_ready_for_input(app, &paths)?;
    let response = send_request(&paths, &ControllerRequest::Path { spec })?;
    Ok(json!({
        "ok": response.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "input_backend": "uinput",
        "device_serial": paths.device_serial.as_str(),
        "controller": response,
    }))
}

pub(crate) fn run(app: &App, config: &RunConfig) -> CliResult<Value> {
    let event_log = ControllerEventLog::controller(app, config)?;
    event_log.append(
        "controller_start",
        json!({
            "socket": path_string_lossy(&config.socket),
            "state": path_string_lossy(&config.state),
            "events": path_string_lossy(&config.events),
            "final_state": path_string_lossy(&config.final_state),
            "uinput_stdout": path_string_lossy(&config.uinput_stdout),
            "uinput_stderr": path_string_lossy(&config.uinput_stderr),
            "input_profile": config
                .input_profile
                .as_ref()
                .map(RuntimeProfile::summary_json),
        }),
    );
    let outcome = run_inner(app, config, &event_log);
    match outcome.as_ref() {
        Ok(value) => event_log.append(
            "controller_exit",
            json!({
                "ok": value.get("ok").cloned().unwrap_or(Value::Null),
                "stopped": value.get("stopped").cloned().unwrap_or(Value::Null),
            }),
        ),
        Err(error) => event_log.append(
            "controller_exit",
            json!({
                "ok": false,
                "error": error.to_string(),
            }),
        ),
    }
    outcome
}

fn run_inner(app: &App, config: &RunConfig, event_log: &ControllerEventLog) -> CliResult<Value> {
    remove_file_if_exists(&config.socket)?;
    let listener = UnixListener::bind(&config.socket)?;
    let before_profiles = uinput::discover_touchscreen_profiles(app)?;
    let profile = uinput::select_primary_touchscreen_profile(&before_profiles)?;
    event_log.append(
        "uinput_start",
        json!({
            "command": uinput::input_device_command(),
            "stdout": path_string_lossy(&config.uinput_stdout),
            "stderr": path_string_lossy(&config.uinput_stderr),
            "physical_touchscreen": uinput::profile_summary(&profile),
            "physical_touchscreen_profile_hash": uinput::profile_hash(&profile).ok(),
        }),
    );
    let mut uinput_process = start_uinput_process(app, config)?;
    write_uinput_line(&mut uinput_process, &uinput::register_line(&profile)?)?;
    write_uinput_line(
        &mut uinput_process,
        &uinput::delay_line(uinput::DEVICE_SETTLE_MS)?,
    )?;
    thread::sleep(Duration::from_millis(uinput::DEVICE_SETTLE_MS));
    ensure_uinput_alive(&mut uinput_process)?;

    let virtual_touchscreen = virtual_touchscreen_report(app, &profile, &before_profiles);
    event_log.append(
        "uinput_registered",
        json!({
            "virtual_touchscreen": &virtual_touchscreen,
        }),
    );
    let state = controller_state(app, config, &profile, &virtual_touchscreen)?;
    let mut runtime_state = ControllerRuntimeState::new(config.state.clone(), state);
    runtime_state.write()?;
    event_log.append(
        "state_write_done",
        json!({
            "state": "controller_ready",
            "state_path": path_string_lossy(&config.state),
        }),
    );

    let mut stopped = false;
    let mut generator = config.input_profile.clone().map(ProfileGenerator::new);
    for stream_result in listener.incoming() {
        let stream = stream_result?;
        event_log.append("request_accept", json!({}));
        let mut request_context = ControllerRequestContext {
            uinput_process: &mut uinput_process,
            profile: &profile,
            generator: generator.as_mut(),
            runtime_state: &mut runtime_state,
            event_log,
        };
        if handle_stream(stream, &mut request_context)? {
            stopped = true;
            break;
        }
    }

    shutdown_uinput(uinput_process)?;
    preserve_final_state(config, &runtime_state.value, event_log);
    remove_file_if_exists(&config.socket)?;
    remove_file_if_exists(&config.state)?;

    Ok(json!({
        "ok": true,
        "stopped": stopped,
        "package_name": app.package(),
        "device_serial": app.selected_device_serial()?,
    }))
}

fn controller_args(
    app: &App,
    paths: &RuntimePaths,
    invocation: &ControllerInvocation,
    run_id: &str,
    input_profile: Option<&RuntimeProfile>,
) -> CliResult<Vec<String>> {
    let mut args = vec![
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
        path_string(&invocation.uinput_stdout)?,
        String::from("--uinput-stderr"),
        path_string(&invocation.uinput_stderr)?,
        String::from("--events"),
        path_string(&invocation.events)?,
        String::from("--final-state"),
        path_string(&invocation.final_state)?,
        String::from("--controller-invocation-id"),
        invocation.id.clone(),
        String::from("--run-id"),
        String::from(run_id),
    ];
    if let Some(runtime_profile) = input_profile {
        args.extend([
            String::from("--input-profile-runtime-json"),
            profile::runtime_json(runtime_profile)?,
        ]);
    }
    Ok(args)
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

fn ensure_ready_for_input(app: &App, paths: &RuntimePaths) -> CliResult<()> {
    if !paths.socket.exists() {
        return Err(CliError::new(
            "no active input session; run `input-dynamics session start --run-id <id>`",
        ));
    }
    let current = status(app)?;
    if !value_bool(&current, "active") {
        return Err(CliError::new(
            "no active input session; run `input-dynamics session start --run-id <id>`",
        ));
    }
    if !value_bool(&current, "ready_for_input") {
        let lock_state = current
            .pointer("/session_lock/state")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        return Err(CliError::new(format!(
            "input session is not ready for commands; session_lock.state={lock_state}"
        )));
    }
    Ok(())
}

fn handle_stream(
    mut stream: UnixStream,
    context: &mut ControllerRequestContext<'_>,
) -> CliResult<bool> {
    let request = read_controller_request(&mut stream, context.event_log)?;
    let command_mark = start_tracked_request(context, &request)?;
    let response = execute_tracked_request(context, &request, command_mark.as_ref())?;
    write_and_log_controller_response(&mut stream, &request, &response, context.event_log)?;
    context.event_log.append(
        "request_done",
        json!({
            "command": request.name(),
            "stop": matches!(request, ControllerRequest::Stop),
        }),
    );
    Ok(matches!(request, ControllerRequest::Stop))
}

fn read_controller_request(
    stream: &mut UnixStream,
    event_log: &ControllerEventLog,
) -> CliResult<ControllerRequest> {
    let mut request_text = String::new();
    event_log.append("request_read_start", json!({}));
    stream.read_to_string(&mut request_text)?;
    event_log.append(
        "request_read_done",
        json!({
            "request_bytes": request_text.len(),
        }),
    );
    let request: ControllerRequest = serde_json::from_str(request_text.trim())?;
    event_log.append(
        "request_parsed",
        json!({
            "command": request.name(),
            "request": request.summary_json(),
        }),
    );
    Ok(request)
}

fn start_tracked_request(
    context: &mut ControllerRequestContext<'_>,
    request: &ControllerRequest,
) -> CliResult<Option<ControllerCommandMark>> {
    let command_mark = context.runtime_state.start_request(request)?;
    if let Some(mark) = command_mark.as_ref() {
        context.event_log.append(
            "request_started",
            json!({
                "command_sequence": mark.sequence,
                "command": mark.command_name,
            }),
        );
        context.event_log.append(
            "state_write_done",
            json!({
                "state": "request_started",
                "command_sequence": mark.sequence,
                "command": mark.command_name,
            }),
        );
    }
    Ok(command_mark)
}

fn execute_tracked_request(
    context: &mut ControllerRequestContext<'_>,
    request: &ControllerRequest,
    command_mark: Option<&ControllerCommandMark>,
) -> CliResult<Value> {
    match handle_request(request, context) {
        Ok(response) => {
            finish_tracked_success(context, request, command_mark, &response)?;
            Ok(response)
        }
        Err(error) => {
            finish_tracked_failure(context, request, command_mark, &error)?;
            Err(error)
        }
    }
}

fn finish_tracked_success(
    context: &mut ControllerRequestContext<'_>,
    request: &ControllerRequest,
    command_mark: Option<&ControllerCommandMark>,
    response: &Value,
) -> CliResult<()> {
    if let Some(mark) = command_mark {
        context
            .runtime_state
            .finish_success(mark, request, response)?;
        context.event_log.append(
            "state_write_done",
            json!({
                "state": "request_finished",
                "command_sequence": mark.sequence,
                "command": mark.command_name,
                "ok": response.get("ok").cloned().unwrap_or(Value::Null),
            }),
        );
    }
    Ok(())
}

fn finish_tracked_failure(
    context: &mut ControllerRequestContext<'_>,
    request: &ControllerRequest,
    command_mark: Option<&ControllerCommandMark>,
    error: &CliError,
) -> CliResult<()> {
    if let Some(mark) = command_mark {
        context.runtime_state.finish_failure(mark, request, error)?;
        context.event_log.append(
            "state_write_done",
            json!({
                "state": "request_failed",
                "command_sequence": mark.sequence,
                "command": mark.command_name,
                "error": error.to_string(),
            }),
        );
    }
    Ok(())
}

fn write_and_log_controller_response(
    stream: &mut UnixStream,
    request: &ControllerRequest,
    response: &Value,
    event_log: &ControllerEventLog,
) -> CliResult<()> {
    event_log.append(
        "response_write_start",
        json!({
            "command": request.name(),
        }),
    );
    match write_controller_response(stream, response)? {
        ResponseWriteOutcome::Delivered => event_log.append(
            "response_write_done",
            json!({
                "command": request.name(),
            }),
        ),
        ResponseWriteOutcome::Abandoned { error_kind, error } => event_log.append(
            "response_write_error",
            json!({
                "command": request.name(),
                "abandoned": true,
                "error_kind": error_kind,
                "error": error,
            }),
        ),
    }
    Ok(())
}

fn write_controller_response(
    stream: &mut UnixStream,
    response: &Value,
) -> CliResult<ResponseWriteOutcome> {
    let mut response_text = serde_json::to_vec(response)?;
    response_text.push(b'\n');
    stream.set_nonblocking(true)?;
    match stream.write_all(&response_text) {
        Ok(()) => Ok(ResponseWriteOutcome::Delivered),
        Err(error) if io_response_write_abandoned(&error) => Ok(ResponseWriteOutcome::Abandoned {
            error_kind: io_error_kind_name(error.kind()),
            error: error.to_string(),
        }),
        Err(error) => Err(error.into()),
    }
}

fn handle_request(
    request: &ControllerRequest,
    context: &mut ControllerRequestContext<'_>,
) -> CliResult<Value> {
    ensure_uinput_alive(context.uinput_process)?;
    match *request {
        ControllerRequest::Status => Ok(json!({
            "ok": true,
            "active": true,
            "input_backend": "uinput",
            "input_device_command": uinput::input_device_command(),
            "input_profile": context
                .generator
                .as_ref()
                .map(|active| active.summary_json()),
        })),
        ControllerRequest::Tap {
            fallback,
            key_context,
            inter_key_delay_sampling,
        } => handle_tap_request(context, fallback, key_context, inter_key_delay_sampling),
        ControllerRequest::Path { ref spec } => handle_path_request(context, spec),
        ControllerRequest::Stop => handle_stop_request(context),
    }
}

fn handle_tap_request(
    context: &mut ControllerRequestContext<'_>,
    fallback: TapSpec,
    key_context: Option<KeyProfileContext>,
    inter_key_delay_sampling: InterKeyDelaySampling,
) -> CliResult<Value> {
    let sampled = if let Some(active_generator) = context.generator.as_mut() {
        active_generator.sample_tap(fallback, key_context, inter_key_delay_sampling)?
    } else {
        profile::SampledTap {
            spec: fallback,
            sample: None,
            inter_key_delay_ms: None,
        }
    };
    let line_count = write_uinput_lines(
        context.uinput_process,
        uinput::tap_lines(context.profile, sampled.spec)?,
    )?;
    context.event_log.append(
        "uinput_write_done",
        json!({
            "command": "tap",
            "line_count": line_count,
        }),
    );
    Ok(json!({
        "ok": true,
        "active": true,
        "input_backend": "uinput",
        "tap": {
            "x": sampled.spec.x,
            "y": sampled.spec.y,
            "hold_ms": sampled.spec.hold_ms,
            "pressure": sampled.spec.pressure,
            "touch_major_px": sampled.spec.touch_major_px,
            "touch_minor_px": sampled.spec.touch_minor_px,
            "orientation": sampled.spec.orientation,
        },
        "input_profile_sample": sampled.sample.map(profile::ProfileTapSample::json),
        "inter_key_delay_ms": sampled.inter_key_delay_ms,
    }))
}

fn handle_path_request(
    context: &mut ControllerRequestContext<'_>,
    spec: &PathSpec,
) -> CliResult<Value> {
    let line_count = write_uinput_lines(
        context.uinput_process,
        uinput::path_lines(context.profile, spec)?,
    )?;
    context.event_log.append(
        "uinput_write_done",
        json!({
            "command": "path",
            "line_count": line_count,
            "point_count": spec.points.len(),
        }),
    );
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

fn handle_stop_request(context: &mut ControllerRequestContext<'_>) -> CliResult<Value> {
    write_uinput_line(context.uinput_process, &uinput::delay_line(STOP_TAIL_MS)?)?;
    context.event_log.append(
        "uinput_write_done",
        json!({
            "command": "stop",
            "line_count": 1_u64,
        }),
    );
    context
        .event_log
        .append("controller_stop_requested", json!({}));
    Ok(json!({
        "ok": true,
        "active": false,
        "input_backend": "uinput",
        "stopping": true,
    }))
}

fn write_uinput_lines<I>(process: &mut StdinProcess, lines: I) -> CliResult<u64>
where
    I: IntoIterator<Item = String>,
{
    let mut line_count = 0_u64;
    for line in lines {
        write_uinput_line(process, &line)?;
        line_count = line_count.saturating_add(1);
    }
    Ok(line_count)
}

fn send_request(paths: &RuntimePaths, request: &ControllerRequest) -> CliResult<Value> {
    let event_log = ControllerEventLog::client(paths);
    let mut stream = connect_controller_request(paths, request, event_log.as_ref())?;
    write_controller_request(&mut stream, paths, request, event_log.as_ref())?;
    let response_text =
        read_controller_response_text(&mut stream, paths, request, event_log.as_ref())?;
    parse_controller_response(&response_text, request, event_log.as_ref())
}

fn connect_controller_request(
    paths: &RuntimePaths,
    request: &ControllerRequest,
    event_log: Option<&ControllerEventLog>,
) -> CliResult<UnixStream> {
    log_client_event(
        event_log,
        "request_connect",
        request,
        json!({
            "socket": path_string_lossy(&paths.socket),
        }),
    );
    let stream = match UnixStream::connect(&paths.socket) {
        Ok(stream) => stream,
        Err(error) => {
            log_client_event(
                event_log,
                "request_connect_error",
                request,
                json!({
                    "error_kind": io_error_kind_name(error.kind()),
                    "error": error.to_string(),
                }),
            );
            return Err(error.into());
        }
    };
    Ok(stream)
}

fn write_controller_request(
    stream: &mut UnixStream,
    paths: &RuntimePaths,
    request: &ControllerRequest,
    event_log: Option<&ControllerEventLog>,
) -> CliResult<()> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    serde_json::to_writer(&mut *stream, request).map_err(|error| {
        if json_io_timed_out(&error) {
            log_client_event(event_log, "request_write_timeout", request, json!({}));
            request_timeout_error(paths, request, "write", REQUEST_TIMEOUT)
        } else {
            CliError::from(error)
        }
    })?;
    stream.write_all(b"\n").map_err(|error| {
        if io_timed_out(&error) {
            log_client_event(
                event_log,
                "request_write_timeout",
                request,
                json!({
                    "error_kind": io_error_kind_name(error.kind()),
                    "error": error.to_string(),
                }),
            );
            request_timeout_error(paths, request, "write", REQUEST_TIMEOUT)
        } else {
            CliError::from(error)
        }
    })?;
    stream.flush().map_err(|error| {
        if io_timed_out(&error) {
            log_client_event(
                event_log,
                "request_write_timeout",
                request,
                json!({
                    "error_kind": io_error_kind_name(error.kind()),
                    "error": error.to_string(),
                }),
            );
            request_timeout_error(paths, request, "write", REQUEST_TIMEOUT)
        } else {
            CliError::from(error)
        }
    })?;
    log_client_event(event_log, "request_write_done", request, json!({}));
    stream.shutdown(Shutdown::Write)?;
    Ok(())
}

fn read_controller_response_text(
    stream: &mut UnixStream,
    paths: &RuntimePaths,
    request: &ControllerRequest,
    event_log: Option<&ControllerEventLog>,
) -> CliResult<String> {
    read_controller_response_text_with_timeout(stream, paths, request, event_log, REQUEST_TIMEOUT)
}

fn read_controller_response_text_with_timeout(
    stream: &mut UnixStream,
    paths: &RuntimePaths,
    request: &ControllerRequest,
    event_log: Option<&ControllerEventLog>,
    timeout: Duration,
) -> CliResult<String> {
    log_client_event(event_log, "response_read_start", request, json!({}));
    stream.set_read_timeout(Some(timeout))?;
    let mut response_text = String::new();
    stream.read_to_string(&mut response_text).map_err(|error| {
        if io_timed_out(&error) {
            log_client_event(
                event_log,
                "response_read_timeout",
                request,
                json!({
                    "error_kind": io_error_kind_name(error.kind()),
                    "error": error.to_string(),
                    "timeout_ms": millis_u64(timeout),
                    "state": read_state_json(&paths.state),
                }),
            );
            request_timeout_error(paths, request, "read", timeout)
        } else {
            log_client_event(
                event_log,
                "response_read_error",
                request,
                json!({
                    "error_kind": io_error_kind_name(error.kind()),
                    "error": error.to_string(),
                }),
            );
            CliError::from(error)
        }
    })?;
    Ok(response_text)
}

fn parse_controller_response(
    response_text: &str,
    request: &ControllerRequest,
    event_log: Option<&ControllerEventLog>,
) -> CliResult<Value> {
    log_client_event(
        event_log,
        "response_read_done",
        request,
        json!({
            "response_bytes": response_text.len(),
        }),
    );
    match serde_json::from_str(response_text.trim()) {
        Ok(response) => {
            log_client_event(event_log, "response_parse_done", request, json!({}));
            Ok(response)
        }
        Err(error) => {
            log_client_event(
                event_log,
                "response_parse_error",
                request,
                json!({
                    "error": error.to_string(),
                }),
            );
            Err(error.into())
        }
    }
}

fn request_timeout_error(
    paths: &RuntimePaths,
    request: &ControllerRequest,
    phase: &str,
    timeout: Duration,
) -> CliError {
    let state = read_state_json(&paths.state);
    CliError::new(format!(
        "timed out during {phase} for controller {} request after {} ms; socket={}; state={}; {}",
        request.name(),
        millis_u64(timeout),
        path_string_lossy(&paths.socket),
        path_string_lossy(&paths.state),
        timeout_state_summary(&state),
    ))
}

fn timeout_state_summary(state: &Value) -> String {
    format!(
        "current_command={}; last_command={}; last_error={}",
        command_brief(state.get("current_command")),
        command_brief(state.get("last_command")),
        error_brief(state.get("last_error")),
    )
}

fn command_brief(command: Option<&Value>) -> String {
    let Some(value) = command else {
        return String::from("null");
    };
    if value.is_null() {
        return String::from("null");
    }
    let name = value
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let sequence = value
        .get("sequence")
        .and_then(Value::as_u64)
        .map_or_else(|| String::from("?"), |number| number.to_string());
    let duration = value
        .get("duration_ms")
        .and_then(Value::as_u64)
        .map_or_else(String::new, |millis| format!(",duration_ms={millis}"));
    format!("{name}#{sequence}:{status}{duration}")
}

fn error_brief(error: Option<&Value>) -> String {
    let Some(value) = error else {
        return String::from("null");
    };
    if value.is_null() {
        return String::from("null");
    }
    value
        .as_str()
        .map_or_else(|| value.to_string(), String::from)
}

fn json_io_timed_out(error: &serde_json::Error) -> bool {
    error
        .io_error_kind()
        .is_some_and(|kind| matches!(kind, ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

fn io_timed_out(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn io_response_write_abandoned(error: &std::io::Error) -> bool {
    io_kind_response_write_abandoned(error.kind())
}

const fn io_kind_response_write_abandoned(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::BrokenPipe
            | ErrorKind::ConnectionReset
            | ErrorKind::NotConnected
            | ErrorKind::TimedOut
            | ErrorKind::WouldBlock
    )
}

fn io_error_kind_name(kind: ErrorKind) -> String {
    format!("{kind:?}")
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
        "run_id": config.run_id.as_str(),
        "controller_invocation_id": config.controller_invocation_id.as_str(),
        "socket_path": path_string_lossy(&config.socket),
        "state_path": path_string_lossy(&config.state),
        "events_path": path_string_lossy(&config.events),
        "final_state_path": path_string_lossy(&config.final_state),
        "started_wall_ms": wall_time_ms(),
        "input_backend": "uinput",
        "input_device_command": uinput::input_device_command(),
        "input_profile": config
            .input_profile
            .as_ref()
            .map(RuntimeProfile::summary_json),
        "command_sequence": 0_u64,
        "current_command": Value::Null,
        "last_command": Value::Null,
        "last_error": Value::Null,
        "physical_touchscreen_profile_hash": uinput::profile_hash(profile)?,
        "physical_touchscreen": uinput::profile_summary(profile),
        "virtual_touchscreen": virtual_touchscreen,
    }))
}

impl ControllerRuntimeState {
    const fn new(path: PathBuf, value: Value) -> Self {
        Self {
            path,
            value,
            sequence: 0,
        }
    }

    fn write(&self) -> CliResult<()> {
        write_json_file(&self.path, &self.value)
    }

    fn start_request(
        &mut self,
        request: &ControllerRequest,
    ) -> CliResult<Option<ControllerCommandMark>> {
        if !request.tracked() {
            return Ok(None);
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| CliError::new("controller command sequence overflow"))?;
        self.sequence = sequence;
        let mark = ControllerCommandMark {
            sequence,
            command_name: request.name(),
            started_wall_ms: wall_time_ms(),
            started: Instant::now(),
        };
        self.set_command_fields(
            Some(command_started_json(&mark, request)),
            None,
            None,
            Some(sequence),
        )?;
        self.write()?;
        Ok(Some(mark))
    }

    fn finish_success(
        &mut self,
        mark: &ControllerCommandMark,
        request: &ControllerRequest,
        response: &Value,
    ) -> CliResult<()> {
        let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let error = if ok {
            Value::Null
        } else {
            response
                .get("error")
                .cloned()
                .unwrap_or_else(|| json!("controller command returned ok:false"))
        };
        self.set_command_fields(
            Some(Value::Null),
            Some(command_finished_json(mark, request, &error, response)),
            Some(error),
            None,
        )?;
        self.write()
    }

    fn finish_failure(
        &mut self,
        mark: &ControllerCommandMark,
        request: &ControllerRequest,
        error: &CliError,
    ) -> CliResult<()> {
        let error_value = json!(error.to_string());
        self.set_command_fields(
            Some(Value::Null),
            Some(command_failed_json(mark, request, &error_value)),
            Some(error_value),
            None,
        )?;
        self.write()
    }

    fn set_command_fields(
        &mut self,
        current_command: Option<Value>,
        last_command: Option<Value>,
        last_error: Option<Value>,
        command_sequence: Option<u64>,
    ) -> CliResult<()> {
        let object = self
            .value
            .as_object_mut()
            .ok_or_else(|| CliError::new("controller state root was not an object"))?;
        if let Some(value) = current_command {
            object.insert(String::from("current_command"), value);
        }
        if let Some(value) = last_command {
            object.insert(String::from("last_command"), value);
        }
        if let Some(value) = last_error {
            object.insert(String::from("last_error"), value);
        }
        if let Some(value) = command_sequence {
            object.insert(String::from("command_sequence"), json!(value));
        }
        Ok(())
    }
}

fn command_started_json(mark: &ControllerCommandMark, request: &ControllerRequest) -> Value {
    json!({
        "schema": "input_dynamics_controller_command.v1",
        "sequence": mark.sequence,
        "command": mark.command_name,
        "status": "in_progress",
        "started_wall_ms": mark.started_wall_ms,
        "request": request.summary_json(),
    })
}

fn command_finished_json(
    mark: &ControllerCommandMark,
    request: &ControllerRequest,
    error: &Value,
    response: &Value,
) -> Value {
    let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
    json!({
        "schema": "input_dynamics_controller_command.v1",
        "sequence": mark.sequence,
        "command": mark.command_name,
        "status": "completed",
        "ok": ok,
        "error": error,
        "started_wall_ms": mark.started_wall_ms,
        "completed_wall_ms": wall_time_ms(),
        "duration_ms": millis_u64(mark.started.elapsed()),
        "request": request.summary_json(),
        "response": controller_response_summary(response),
    })
}

fn command_failed_json(
    mark: &ControllerCommandMark,
    request: &ControllerRequest,
    error: &Value,
) -> Value {
    json!({
        "schema": "input_dynamics_controller_command.v1",
        "sequence": mark.sequence,
        "command": mark.command_name,
        "status": "failed",
        "ok": false,
        "error": error,
        "started_wall_ms": mark.started_wall_ms,
        "completed_wall_ms": wall_time_ms(),
        "duration_ms": millis_u64(mark.started.elapsed()),
        "request": request.summary_json(),
        "response": Value::Null,
    })
}

fn controller_response_summary(response: &Value) -> Value {
    let path = response.get("path").map_or(Value::Null, |path| {
        json!({
            "point_count": path.get("point_count").cloned().unwrap_or(Value::Null),
            "duration_ms": path.get("duration_ms").cloned().unwrap_or(Value::Null),
        })
    });
    json!({
        "ok": response.get("ok").cloned().unwrap_or(Value::Null),
        "active": response.get("active").cloned().unwrap_or(Value::Null),
        "input_backend": response.get("input_backend").cloned().unwrap_or(Value::Null),
        "tap": response.get("tap").cloned().unwrap_or(Value::Null),
        "path": path,
        "inter_key_delay_ms": response
            .get("inter_key_delay_ms")
            .cloned()
            .unwrap_or(Value::Null),
        "stopping": response.get("stopping").cloned().unwrap_or(Value::Null),
    })
}

fn touch_point_json(point: uinput::TouchPoint) -> Value {
    json!({
        "x": point.x,
        "y": point.y,
    })
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
    if paths.socket.exists() && send_request(paths, &ControllerRequest::Status).is_err() {
        remove_file_if_exists(&paths.socket)?;
    }
    if paths.state.exists() && !paths.socket.exists() {
        preserve_stale_state(paths);
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
        "package_name": paths.package_name.as_str(),
        "device_serial": paths.device_serial.as_str(),
        "socket": path_string_lossy(&paths.socket),
        "state": path_string_lossy(&paths.state),
        "session_lock": path_string_lossy(&paths.session_lock),
        "current": path_string_lossy(&paths.current),
        "runs_dir": path_string_lossy(&paths.runs_dir),
        "controller_stdout": path_string_lossy(&paths.controller_stdout),
        "controller_stderr": path_string_lossy(&paths.controller_stderr),
        "uinput_stdout": path_string_lossy(&paths.uinput_stdout),
        "uinput_stderr": path_string_lossy(&paths.uinput_stderr),
        "current_invocation": read_json_file_or_null(&paths.current),
    })
}

fn log_client_event(
    event_log: Option<&ControllerEventLog>,
    event: &str,
    request: &ControllerRequest,
    fields: Value,
) {
    if let Some(log) = event_log {
        let mut event_fields = json!({
            "command": request.name(),
            "request": request.summary_json(),
        });
        merge_event_fields(&mut event_fields, fields);
        log.append(event, event_fields);
    }
}

impl ControllerInvocation {
    fn new(paths: &RuntimePaths, run_id: &str) -> Self {
        let id = controller_invocation_id(run_id);
        let dir = paths.runs_dir.join(&id);
        Self {
            id,
            manifest: dir.join("manifest.json"),
            events: dir.join("controller.events.jsonl"),
            controller_stdout: dir.join("controller.stdout.log"),
            controller_stderr: dir.join("controller.stderr.log"),
            uinput_stdout: dir.join("uinput.stdout.log"),
            uinput_stderr: dir.join("uinput.stderr.log"),
            final_state: dir.join("final.state.json"),
            final_session_lock: dir.join("final.session.lock.json"),
            dir,
        }
    }
}

impl ControllerEventLog {
    fn client_from_invocation(
        app: &App,
        paths: &RuntimePaths,
        invocation: &ControllerInvocation,
        run_id: &str,
    ) -> Self {
        Self {
            path: invocation.events.clone(),
            package_name: String::from(app.package()),
            device_serial: paths.device_serial.clone(),
            run_id: String::from(run_id),
            controller_invocation_id: invocation.id.clone(),
            source: "client",
            pid: process::id(),
            started: Instant::now(),
        }
    }

    fn client(paths: &RuntimePaths) -> Option<Self> {
        let current = read_json_file(&paths.current)?;
        let invocation = current.get("invocation")?;
        let event_path = invocation.get("events")?.as_str()?;
        let run_id = invocation.get("run_id")?.as_str()?;
        let controller_invocation_id = invocation.get("controller_invocation_id")?.as_str()?;
        Some(Self {
            path: PathBuf::from(event_path),
            package_name: paths.package_name.clone(),
            device_serial: paths.device_serial.clone(),
            run_id: String::from(run_id),
            controller_invocation_id: String::from(controller_invocation_id),
            source: "client",
            pid: process::id(),
            started: Instant::now(),
        })
    }

    fn controller(app: &App, config: &RunConfig) -> CliResult<Self> {
        Ok(Self {
            path: config.events.clone(),
            package_name: String::from(app.package()),
            device_serial: app.selected_device_serial()?,
            run_id: config.run_id.clone(),
            controller_invocation_id: config.controller_invocation_id.clone(),
            source: "controller",
            pid: process::id(),
            started: Instant::now(),
        })
    }

    fn append(&self, event: &str, fields: Value) {
        match self.append_result(event, fields) {
            Ok(()) | Err(_) => {}
        }
    }

    fn append_result(&self, event: &str, fields: Value) -> CliResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut record = json!({
            "schema": EVENT_SCHEMA,
            "source": self.source,
            "event": event,
            "pid": self.pid,
            "package_name": self.package_name.as_str(),
            "device_serial": self.device_serial.as_str(),
            "run_id": self.run_id.as_str(),
            "controller_invocation_id": self.controller_invocation_id.as_str(),
            "t_wall_ms": wall_time_ms(),
            "t_monotonic_ms": millis_u64(self.started.elapsed()),
        });
        merge_event_fields(&mut record, fields);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
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
            package_name: String::from(package),
            device_serial: String::from(device_serial),
            socket: runtime_file(&base_dir, &prefix, "sock"),
            state: runtime_file(&base_dir, &prefix, "state.json"),
            session_lock: runtime_file(&base_dir, &prefix, "session.lock.json"),
            current: runtime_file(&base_dir, &prefix, "current.json"),
            runs_dir: base_dir.join(format!("{prefix}.runs")),
            controller_stdout: runtime_file(&base_dir, &prefix, "controller.stdout.log"),
            controller_stderr: runtime_file(&base_dir, &prefix, "controller.stderr.log"),
            uinput_stdout: runtime_file(&base_dir, &prefix, "uinput.stdout.log"),
            uinput_stderr: runtime_file(&base_dir, &prefix, "uinput.stderr.log"),
            dir: base_dir,
        }
    }
}

fn write_invocation_manifest(
    paths: &RuntimePaths,
    invocation: &ControllerInvocation,
    metadata: InvocationMetadata<'_>,
) -> CliResult<()> {
    write_json_file(
        &invocation.manifest,
        &json!({
            "schema": MANIFEST_SCHEMA,
            "package_name": metadata.package_name,
            "device_serial": metadata.device_serial,
            "run_id": metadata.run_id,
            "controller_invocation_id": invocation.id.as_str(),
            "parent_pid": process::id(),
            "controller_pid": metadata.controller_pid,
            "created_wall_ms": wall_time_ms(),
            "input_profile": metadata.input_profile.map(RuntimeProfile::summary_json),
            "control": {
                "socket": path_string_lossy(&paths.socket),
                "state": path_string_lossy(&paths.state),
                "session_lock": path_string_lossy(&paths.session_lock),
                "current": path_string_lossy(&paths.current),
            },
            "diagnostics": invocation_json(invocation),
        }),
    )
}

fn write_current_invocation(
    paths: &RuntimePaths,
    invocation: &ControllerInvocation,
    metadata: InvocationMetadata<'_>,
) -> CliResult<()> {
    write_json_file(
        &paths.current,
        &json!({
            "schema": CURRENT_SCHEMA,
            "active": metadata.active.as_bool(),
            "package_name": metadata.package_name,
            "device_serial": metadata.device_serial,
            "run_id": metadata.run_id,
            "controller_invocation_id": invocation.id.as_str(),
            "controller_pid": metadata.controller_pid,
            "updated_wall_ms": wall_time_ms(),
            "control": {
                "socket": path_string_lossy(&paths.socket),
                "state": path_string_lossy(&paths.state),
                "session_lock": path_string_lossy(&paths.session_lock),
            },
            "invocation": invocation_json(invocation),
        }),
    )
}

fn invocation_json(invocation: &ControllerInvocation) -> Value {
    json!({
        "controller_invocation_id": invocation.id.as_str(),
        "dir": path_string_lossy(&invocation.dir),
        "manifest": path_string_lossy(&invocation.manifest),
        "events": path_string_lossy(&invocation.events),
        "controller_stdout": path_string_lossy(&invocation.controller_stdout),
        "controller_stderr": path_string_lossy(&invocation.controller_stderr),
        "uinput_stdout": path_string_lossy(&invocation.uinput_stdout),
        "uinput_stderr": path_string_lossy(&invocation.uinput_stderr),
        "final_state": path_string_lossy(&invocation.final_state),
        "final_session_lock": path_string_lossy(&invocation.final_session_lock),
    })
}

fn merge_event_fields(record: &mut Value, fields: Value) {
    let Some(record_object) = record.as_object_mut() else {
        return;
    };
    match fields {
        Value::Object(fields_object) => {
            for (key, value) in fields_object {
                record_object.insert(key, value);
            }
        }
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)) => {
            record_object.insert(String::from("fields"), other);
        }
    }
}

fn controller_invocation_id(run_id: &str) -> String {
    format!(
        "{}-pid{}-{}",
        wall_time_ms(),
        process::id(),
        run_id_fragment(run_id)
    )
}

fn run_id_fragment(run_id: &str) -> String {
    sanitize_path_component(run_id)
        .chars()
        .take(RUN_ID_FRAGMENT_MAX_CHARS)
        .collect()
}

fn prune_old_invocations(paths: &RuntimePaths) -> CliResult<()> {
    let entries = match fs::read_dir(&paths.runs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut directories = Vec::new();
    for entry_result in entries {
        let entry = entry_result?;
        if entry.file_type()?.is_dir() {
            directories.push(entry.path());
        }
    }
    directories.sort();
    directories.reverse();
    for directory in directories.into_iter().skip(DIAGNOSTIC_RETENTION_COUNT) {
        fs::remove_dir_all(directory)?;
    }
    Ok(())
}

fn preserve_final_state(config: &RunConfig, state: &Value, event_log: &ControllerEventLog) {
    match write_json_file(&config.final_state, state) {
        Ok(()) => event_log.append(
            "final_state_write_done",
            json!({
                "final_state": path_string_lossy(&config.final_state),
            }),
        ),
        Err(error) => event_log.append(
            "final_state_write_error",
            json!({
                "final_state": path_string_lossy(&config.final_state),
                "error": error.to_string(),
            }),
        ),
    }
}

fn preserve_stale_state(paths: &RuntimePaths) {
    let state = read_state_json(&paths.state);
    if state.is_null() {
        return;
    }
    let current = read_json_file(&paths.current);
    let final_state = current
        .as_ref()
        .and_then(|value| value.pointer("/invocation/final_state"))
        .and_then(Value::as_str);
    if let Some(path) = final_state {
        match write_json_file(&PathBuf::from(path), &state) {
            Ok(()) | Err(_) => {}
        }
    }
}

fn preserve_session_lock(paths: &RuntimePaths) {
    let lock = read_json_file(&paths.session_lock);
    let Some(lock_value) = lock else {
        return;
    };
    if lock_value.is_null() {
        return;
    }
    let current = read_json_file(&paths.current);
    let final_lock = current
        .as_ref()
        .and_then(|value| value.pointer("/invocation/final_session_lock"))
        .and_then(Value::as_str);
    if let Some(path) = final_lock {
        match write_json_file(&PathBuf::from(path), &lock_value) {
            Ok(()) | Err(_) => {}
        }
    }
}

fn mark_current_inactive(paths: &RuntimePaths) {
    let Some(mut current) = read_json_file(&paths.current) else {
        return;
    };
    let Some(object) = current.as_object_mut() else {
        return;
    };
    object.insert(
        String::from("active"),
        json!(InvocationActive::Inactive.as_bool()),
    );
    object.insert(String::from("updated_wall_ms"), json!(wall_time_ms()));
    match write_json_file(&paths.current, &current) {
        Ok(()) | Err(_) => {}
    }
}

fn read_json_file(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(text.trim()).ok())
}

fn read_json_file_or_null(path: &Path) -> Value {
    read_json_file(path).unwrap_or(Value::Null)
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

fn session_lock_ready(session_lock: &Value) -> bool {
    session_lock.get("state").and_then(Value::as_str) == Some("active")
        && session_lock
            .get("input_active")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use proptest::strategy::Strategy;

    use crate::controller::{
        ControllerEventLog, ControllerRequest, RuntimePaths, controller_response_summary,
        io_kind_response_write_abandoned, parse_dumpsys_input, parse_i64_list,
        read_controller_response_text_with_timeout, run_id_fragment, sanitize_path_component,
        timeout_state_summary, virtual_event_path_from_status, write_and_log_controller_response,
    };
    use crate::profile::InterKeyDelaySampling;
    use crate::uinput::{PathSpec, TapSpec, TouchPoint};

    const SAMPLE_DUMPSYS_INPUT: &str = r"
Event Hub State:
  Devices:
    1: sec_touchscreen
      Classes: KEYBOARD | TOUCH | TOUCH_MT
      Path: /dev/input/event3
      Enabled: true
      Descriptor: 48ee483b8b70f8343840706d561e5b8d5cb64b8c
      Location: sec_touchscreen/input1
      UniqueId: primary_touchscreen_id
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
        assert!(
            paths
                .current
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(
                    |name| name.contains("org.inputdynamics.ime_debug.device_123.current.json")
                ),
            "current path should include sanitized package name and device serial"
        );
        assert!(
            paths
                .runs_dir
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.contains("org.inputdynamics.ime_debug.device_123.runs")),
            "runs dir should include sanitized package name and device serial"
        );
    }

    #[test]
    fn response_write_abandoned_errors_are_nonfatal_transport_outcomes() {
        assert!(
            io_kind_response_write_abandoned(std::io::ErrorKind::BrokenPipe),
            "broken pipe means the client abandoned response delivery"
        );
        assert!(
            io_kind_response_write_abandoned(std::io::ErrorKind::ConnectionReset),
            "connection reset means the client abandoned response delivery"
        );
        assert!(
            !io_kind_response_write_abandoned(std::io::ErrorKind::PermissionDenied),
            "unrelated IO errors should remain controller failures"
        );
    }

    #[test]
    fn response_timeout_then_abandoned_write_is_logged_without_error() {
        let result = response_timeout_then_abandoned_write_regression();
        assert!(
            result.is_ok(),
            "response timeout and abandoned write regression should pass: {result:?}"
        );
    }

    fn response_timeout_then_abandoned_write_regression() -> crate::error::CliResult<()> {
        let base = test_runtime_dir("controller-timeout-abandoned");
        fs::create_dir_all(&base)?;
        let paths =
            RuntimePaths::from_base_dir(base.clone(), "org.inputdynamics.ime.debug", "test-device");
        fs::write(
            &paths.state,
            serde_json::json!({
                "current_command": {
                    "sequence": 1_u64,
                    "command": "tap",
                    "status": "in_progress"
                },
                "last_command": serde_json::Value::Null,
                "last_error": serde_json::Value::Null
            })
            .to_string(),
        )?;
        let events = base.join("controller.events.jsonl");
        let client_log = test_event_log(events.clone(), "client");
        let controller_log = test_event_log(events.clone(), "controller");
        let request = ControllerRequest::Tap {
            fallback: TapSpec::new(12, 34),
            key_context: None,
            inter_key_delay_sampling: InterKeyDelaySampling::Skip,
        };
        let (mut client_stream, mut controller_stream) = UnixStream::pair()?;

        let timeout_result = read_controller_response_text_with_timeout(
            &mut client_stream,
            &paths,
            &request,
            Some(&client_log),
            Duration::from_millis(1),
        );
        if timeout_result.is_ok() {
            return Err(crate::error::CliError::new(
                "client response read should time out while controller has not responded",
            ));
        }
        drop(client_stream);

        write_and_log_controller_response(
            &mut controller_stream,
            &request,
            &serde_json::json!({
                "ok": true,
                "active": true,
                "input_backend": "uinput"
            }),
            &controller_log,
        )?;

        let records = read_test_event_records(&events)?;
        if !records.iter().any(|record| {
            record.get("event").and_then(serde_json::Value::as_str) == Some("response_read_timeout")
                && record.get("source").and_then(serde_json::Value::as_str) == Some("client")
                && record.get("timeout_ms").and_then(serde_json::Value::as_u64) == Some(1)
        }) {
            return Err(crate::error::CliError::new(
                "event journal should include the client response timeout",
            ));
        }
        if !records.iter().any(|record| {
            record.get("event").and_then(serde_json::Value::as_str) == Some("response_write_error")
                && record.get("source").and_then(serde_json::Value::as_str) == Some("controller")
                && record.get("abandoned").and_then(serde_json::Value::as_bool) == Some(true)
        }) {
            return Err(crate::error::CliError::new(
                "event journal should include nonfatal abandoned response delivery",
            ));
        }

        fs::remove_dir_all(&base)?;
        Ok(())
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

    #[test]
    fn controller_path_request_summary_keeps_only_endpoints() {
        let request = ControllerRequest::Path {
            spec: PathSpec::new(
                vec![
                    TouchPoint::new(1, 2),
                    TouchPoint::new(3, 4),
                    TouchPoint::new(5, 6),
                ],
                120,
            ),
        };

        let summary = request.summary_json();

        assert_eq!(
            summary
                .pointer("/path/point_count")
                .and_then(serde_json::Value::as_u64),
            Some(3),
            "path summary should include point count"
        );
        assert_eq!(
            summary
                .pointer("/path/first/x")
                .and_then(serde_json::Value::as_i64),
            Some(1),
            "path summary should include first endpoint"
        );
        assert_eq!(
            summary
                .pointer("/path/last/y")
                .and_then(serde_json::Value::as_i64),
            Some(6),
            "path summary should include last endpoint"
        );
        assert!(
            summary.pointer("/path/points").is_none(),
            "path summary should not copy the full point list"
        );
    }

    #[test]
    fn controller_response_summary_keeps_path_compact() {
        let response = serde_json::json!({
            "ok": true,
            "active": true,
            "input_backend": "uinput",
            "path": {
                "points": [
                    {"x": 1_i32, "y": 2_i32},
                    {"x": 3_i32, "y": 4_i32}
                ],
                "point_count": 2_u64,
                "duration_ms": 80_u64
            }
        });

        let summary = controller_response_summary(&response);

        assert_eq!(
            summary
                .pointer("/path/point_count")
                .and_then(serde_json::Value::as_u64),
            Some(2),
            "response summary should include path point count"
        );
        assert!(
            summary.pointer("/path/points").is_none(),
            "response summary should not copy full path points"
        );
    }

    #[test]
    fn timeout_state_summary_reports_current_last_and_error() {
        let state = serde_json::json!({
            "current_command": {
                "sequence": 2_u64,
                "command": "path",
                "status": "in_progress"
            },
            "last_command": {
                "sequence": 1_u64,
                "command": "tap",
                "status": "completed",
                "duration_ms": 37_u64
            },
            "last_error": "previous failure"
        });

        let summary = timeout_state_summary(&state);

        assert!(
            summary.contains("current_command=path#2:in_progress"),
            "current command should be present: {summary}"
        );
        assert!(
            summary.contains("last_command=tap#1:completed,duration_ms=37"),
            "last command should be present: {summary}"
        );
        assert!(
            summary.contains("last_error=previous failure"),
            "last error should be present: {summary}"
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

        #[test]
        fn run_id_fragment_is_safe_and_bounded(input in path_component_input()) {
            let fragment = run_id_fragment(&input);
            let char_count = fragment.chars().count();

            proptest::prop_assert!(
                !fragment.is_empty(),
                "run id fragment should never be empty"
            );
            proptest::prop_assert!(
                char_count <= super::RUN_ID_FRAGMENT_MAX_CHARS,
                "run id fragment should be bounded"
            );
            proptest::prop_assert!(
                fragment
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || matches!(character, '.' | '-' | '_')),
                "run id fragment should contain only safe ASCII filename characters"
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

    fn test_runtime_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "input-dynamics-{name}-{}-{}",
            std::process::id(),
            super::wall_time_ms()
        ))
    }

    fn test_event_log(path: PathBuf, source: &'static str) -> ControllerEventLog {
        ControllerEventLog {
            path,
            package_name: String::from("org.inputdynamics.ime.debug"),
            device_serial: String::from("test-device"),
            run_id: String::from("run-controller-timeout-abandoned-test"),
            controller_invocation_id: String::from("invocation-controller-timeout-abandoned-test"),
            source,
            pid: std::process::id(),
            started: Instant::now(),
        }
    }

    fn read_test_event_records(path: &Path) -> crate::error::CliResult<Vec<serde_json::Value>> {
        let text = fs::read_to_string(path)?;
        let mut records = Vec::new();
        for line in text.lines() {
            records.push(serde_json::from_str(line)?);
        }
        Ok(records)
    }
}
