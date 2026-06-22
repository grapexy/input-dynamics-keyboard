//! Scientific run capture orchestration.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use input_dynamics_analysis::getevent::{GETEVENT_SCHEMA, normalize_file};
use serde_json::{Value, json};

use crate::app::{App, LOG_DIR};
use crate::commands::{normalize_stats_json, path_string, pull_logs};
use crate::controller::{self, SessionStartPermit};
use crate::coordinate_frame::manifest_coordinate_frame;
use crate::error::{CliError, CliResult};
use crate::observe::{self, AccessibilityDetail};
use crate::process::{FailureMode, spawn_process_to_files};
use crate::uinput;
use crate::validate::validate_logs;

const DEFAULT_RECORD_INPUT_CONTROLLER: &str = "input-dynamics-cli";

pub(crate) struct RecordConfig {
    pub(crate) run_id: String,
    pub(crate) out: PathBuf,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) with_input_controller: bool,
    pub(crate) with_evidence: bool,
    pub(crate) full_accessibility_evidence: bool,
    pub(crate) input_actor: String,
    pub(crate) input_controller: Option<String>,
    pub(crate) input_cadence_policy: String,
}

struct RecordPaths {
    root: PathBuf,
    ime: PathBuf,
    adb: PathBuf,
    derived: PathBuf,
    evidence: PathBuf,
    manifest: PathBuf,
    validation: PathBuf,
    getevent_raw: PathBuf,
    getevent_jsonl: PathBuf,
    getevent_stderr: PathBuf,
    ime_pull_tmp: PathBuf,
}

struct GeteventCapture {
    child: Option<Child>,
}

struct InputControllerCapture<'a> {
    app: &'a App,
    enabled: bool,
    start: Value,
    status_after_start: Value,
    ready: Value,
    stop: Value,
    session_lock: Option<controller::SessionStartLock>,
    stopped: bool,
}

struct ActiveRecordWindow<'a> {
    pre_stop: Value,
    clear: Value,
    start: Value,
    input_controller: InputControllerCapture<'a>,
}

impl Drop for GeteventCapture {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _kill_result = child.kill();
            let _wait_result = child.wait();
        }
    }
}

impl Drop for InputControllerCapture<'_> {
    fn drop(&mut self) {
        if self.enabled && !self.stopped {
            let _stop = controller::stop(self.app);
            let _clear_lock = controller::clear_session_lock(self.app);
        }
    }
}

impl GeteventCapture {
    fn start(app: &App, paths: &RecordPaths) -> CliResult<Self> {
        let args = vec![
            String::from("shell"),
            String::from("getevent"),
            String::from("-lt"),
        ];
        let scoped_args = app.scoped_adb_args(&args)?;
        let child = spawn_process_to_files(
            app.adb_program(),
            &scoped_args,
            &paths.getevent_raw,
            &paths.getevent_stderr,
        )?;
        Ok(Self { child: Some(child) })
    }

    fn stop(&mut self) -> CliResult<Value> {
        let Some(mut child) = self.child.take() else {
            return Ok(json!({"ok": true, "already_stopped": true}));
        };
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        let status = child.wait()?;
        Ok(json!({
            "ok": true,
            "already_stopped": false,
            "status_code": status.code(),
            "success": status.success(),
        }))
    }
}

impl<'a> InputControllerCapture<'a> {
    fn start(app: &'a App, config: &RecordConfig) -> CliResult<Self> {
        if !config.with_input_controller {
            return Ok(Self {
                app,
                enabled: false,
                start: Value::Null,
                status_after_start: Value::Null,
                ready: Value::Null,
                stop: Value::Null,
                session_lock: None,
                stopped: true,
            });
        }

        let session_lock = match controller::acquire_session_start(app, &config.run_id)? {
            SessionStartPermit::Acquired(session_lock) => session_lock,
            SessionStartPermit::Busy(status) => {
                return Err(CliError::new(format!(
                    "input controller is busy during record start: {status}"
                )));
            }
        };
        let start = controller::start(app, &config.run_id, None)?;
        ensure_command_ok(&start, "start record input controller")?;
        let status_after_start = controller::status(app)?;
        Ok(Self {
            app,
            enabled: true,
            start,
            status_after_start,
            ready: Value::Null,
            stop: Value::Null,
            session_lock: Some(session_lock),
            stopped: false,
        })
    }

    fn mark_ready(&mut self) -> CliResult<Value> {
        if !self.enabled {
            self.ready = json!({
                "ok": true,
                "enabled": false,
                "requested": false,
            });
            return Ok(self.ready.clone());
        }
        let Some(mut session_lock) = self.session_lock.take() else {
            self.ready = json!({
                "ok": true,
                "enabled": true,
                "requested": true,
                "already_ready": true,
                "status": controller::status(self.app)?,
            });
            return Ok(self.ready.clone());
        };
        session_lock.activate(&self.start)?;
        let status = controller::status(self.app)?;
        self.ready = json!({
            "ok": status
                .get("ready_for_input")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "enabled": true,
            "requested": true,
            "status": status,
        });
        ensure_command_ok(&self.ready, "mark record input controller ready")?;
        Ok(self.ready.clone())
    }

    fn stop(&mut self) -> Value {
        if !self.enabled {
            self.stopped = true;
            return json!({
                "ok": true,
                "enabled": false,
                "requested": false,
                "reason": "record was run without --with-input-controller",
            });
        }

        let status_before_stop = match controller::status(self.app) {
            Ok(status) => status,
            Err(error) => json!({
                "ok": false,
                "error": error.to_string(),
            }),
        };
        let stop = match controller::stop(self.app) {
            Ok(stop) => stop,
            Err(error) => json!({
                "ok": false,
                "error": error.to_string(),
            }),
        };
        let clear_lock = match controller::clear_session_lock(self.app) {
            Ok(()) => json!({"ok": true}),
            Err(error) => json!({
                "ok": false,
                "error": error.to_string(),
            }),
        };
        let ok = stop.get("ok").and_then(Value::as_bool).unwrap_or(false)
            && clear_lock
                .get("ok")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        self.stop = json!({
            "ok": ok,
            "enabled": true,
            "requested": true,
            "status_before_stop": status_before_stop,
            "stop": stop,
            "clear_session_lock": clear_lock,
        });
        self.stopped = true;
        self.stop.clone()
    }

    fn to_json(&self) -> Value {
        if !self.enabled {
            return json!({
                "enabled": false,
                "requested": false,
                "summary": Value::Null,
            });
        }
        json!({
            "enabled": true,
            "requested": true,
            "start": self.start,
            "status_after_start": self.status_after_start,
            "ready": self.ready,
            "stop": self.stop,
            "summary": input_controller_summary(&self.status_after_start, &self.stop),
        })
    }
}

pub(crate) fn record_run(app: &App, config: &RecordConfig) -> CliResult<Value> {
    let paths = prepare_paths(&config.out)?;
    let host_start_wall_ms = epoch_millis()?;
    let active = start_record_window(app, config)?;
    let ActiveRecordWindow {
        pre_stop,
        clear,
        start,
        mut input_controller,
    } = active;
    let touchscreen_profile = touchscreen_profile_snapshot(app);
    let layout_before_capture = layout_snapshot(app);
    let evidence_start =
        capture_evidence_or_cleanup(app, config, &paths, "start", &mut input_controller)?;
    let mut capture = start_getevent_capture_or_cleanup(app, &paths, &mut input_controller)?;
    mark_input_controller_ready_or_cleanup(app, &mut capture, &mut input_controller)?;
    let wait = wait_for_stop_or_cleanup(app, config, &mut capture, &mut input_controller)?;
    let capture_stop = stop_capture_or_cleanup(app, capture, &mut input_controller)?;
    let layout_after_capture = layout_snapshot(app);
    let evidence_end =
        capture_evidence_or_cleanup(app, config, &paths, "end", &mut input_controller)?;
    let input_controller_stop = input_controller.stop();
    let stop = app.broadcast("STOP", Vec::new())?;
    let pull = pull_logs(app, &paths.ime_pull_tmp)?;
    let ime_files = stage_ime_logs(&paths.ime_pull_tmp, &paths.ime)?;
    let validation = validate_logs(&paths.ime, Some(config.run_id.as_str()))?;
    write_json_file(&paths.validation, &validation)?;
    let getevent_stats = normalize_file(&paths.getevent_raw, &paths.getevent_jsonl)?;
    let getevent_normalization = json!({
        "ok": true,
        "schema": GETEVENT_SCHEMA,
        "input": path_string(&paths.getevent_raw)?,
        "output": path_string(&paths.getevent_jsonl)?,
        "stats": normalize_stats_json(&getevent_stats),
    });
    let host_stop_wall_ms = epoch_millis()?;
    let manifest_parts = ManifestParts {
        host_start_wall_ms,
        host_stop_wall_ms,
        start,
        wait,
        capture_stop,
        pre_stop,
        clear,
        touchscreen_profile,
        layout_before_capture,
        layout_after_capture,
        evidence_start,
        evidence_end,
        input_controller: input_controller.to_json(),
        input_controller_stop,
        stop,
        pull,
        validation: validation.clone(),
        getevent_normalization,
        ime_files,
    };
    let manifest = manifest_json(app, config, &paths, &manifest_parts)?;
    write_json_file(&paths.manifest, &manifest)?;

    Ok(json!({
        "ok": validation.get("ok").and_then(Value::as_bool).unwrap_or(false)
            && input_controller_result_ok(&manifest_parts.input_controller_stop),
        "package_name": app.package(),
        "external_run_id": config.run_id.as_str(),
        "output_dir": path_string(&paths.root)?,
        "manifest": path_string(&paths.manifest)?,
        "validation": validation,
        "getevent_normalization": manifest_parts.getevent_normalization,
        "input_controller": manifest_parts.input_controller,
        "evidence": {
            "enabled": config.with_evidence,
            "policy": if config.with_evidence { "start_end" } else { "none" },
            "start": manifest_parts.evidence_start,
            "end": manifest_parts.evidence_end,
        },
    }))
}

fn start_record_window<'a>(
    app: &'a App,
    config: &RecordConfig,
) -> CliResult<ActiveRecordWindow<'a>> {
    let pre_stop = app.broadcast("STOP", Vec::new())?;
    let clear = app.broadcast("CLEAR_LOGS", Vec::new())?;
    ensure_command_ok(&clear, "clear logs before record")?;
    let start = start_record_session(app, config)?;
    ensure_command_ok(&start, "start record session")?;
    let input_controller = match InputControllerCapture::start(app, config) {
        Ok(input_controller) => input_controller,
        Err(error) => {
            let stop_after_input_failure =
                app.broadcast("STOP", Vec::new())
                    .unwrap_or_else(|stop_error| {
                        json!({
                            "ok": false,
                            "error": stop_error.to_string(),
                        })
                    });
            return Err(CliError::new(format!(
                "failed to start record input controller: {error}; IME stop attempted: {stop_after_input_failure}"
            )));
        }
    };
    Ok(ActiveRecordWindow {
        pre_stop,
        clear,
        start,
        input_controller,
    })
}

fn start_getevent_capture_or_cleanup(
    app: &App,
    paths: &RecordPaths,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<GeteventCapture> {
    GeteventCapture::start(app, paths).map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, None, input_controller);
        CliError::new(format!(
            "failed to start getevent capture: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn mark_input_controller_ready_or_cleanup(
    app: &App,
    capture: &mut GeteventCapture,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<Value> {
    input_controller.mark_ready().map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, Some(capture), input_controller);
        CliError::new(format!(
            "failed to mark record input controller ready: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn wait_for_stop_or_cleanup(
    app: &App,
    config: &RecordConfig,
    capture: &mut GeteventCapture,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<Value> {
    wait_for_stop(config.duration_ms).map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, Some(capture), input_controller);
        CliError::new(format!(
            "failed while waiting for record stop: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn stop_capture_or_cleanup(
    app: &App,
    mut capture: GeteventCapture,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<Value> {
    capture.stop().map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, None, input_controller);
        CliError::new(format!(
            "failed to stop getevent capture: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn capture_evidence_or_cleanup(
    app: &App,
    config: &RecordConfig,
    paths: &RecordPaths,
    phase: &str,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<Value> {
    record_evidence(app, config, paths, phase).map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, None, input_controller);
        CliError::new(format!(
            "failed to capture {phase} evidence: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn record_evidence(
    app: &App,
    config: &RecordConfig,
    paths: &RecordPaths,
    phase: &str,
) -> CliResult<Value> {
    if !config.with_evidence {
        return Ok(json!({
            "enabled": false,
            "requested": false,
            "phase": phase,
        }));
    }
    let phase_dir = paths.evidence.join(phase);
    let bundle = observe::all(app, &phase_dir, record_accessibility_detail(config))?;
    Ok(json!({
        "enabled": true,
        "requested": true,
        "phase": phase,
        "policy": "start_end",
        "bundle": bundle,
    }))
}

const fn record_accessibility_detail(config: &RecordConfig) -> AccessibilityDetail {
    if config.full_accessibility_evidence {
        AccessibilityDetail::Full
    } else {
        AccessibilityDetail::Compressed
    }
}

fn start_record_session(app: &App, config: &RecordConfig) -> CliResult<Value> {
    let mut extras = vec![
        String::from("--es"),
        String::from("run_id"),
        config.run_id.clone(),
        String::from("--es"),
        String::from("input_actor"),
        config.input_actor.clone(),
        String::from("--es"),
        String::from("input_cadence_policy"),
        config.input_cadence_policy.clone(),
    ];
    if let Some(controller) = record_input_controller(config).as_ref() {
        extras.extend([
            String::from("--es"),
            String::from("input_controller"),
            controller.clone(),
        ]);
    }
    let enable = app.broadcast("ENABLE", Vec::new())?;
    if enable.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(CliError::new("failed to enable logging before record"));
    }
    app.broadcast("START", extras)
}

fn touchscreen_profile_snapshot(app: &App) -> Value {
    match uinput::discover_touchscreen_profile(app) {
        Ok(profile) => {
            let hash = uinput::profile_hash(&profile).map_or_else(
                |error| json!({"ok": false, "error": error.to_string()}),
                |hash| json!({"ok": true, "value": hash}),
            );
            json!({
                "ok": true,
                "physical_touchscreen": uinput::profile_summary(&profile),
                "physical_touchscreen_profile_hash": hash
                    .get("value")
                    .cloned()
                    .unwrap_or(Value::Null),
                "profile_hash_result": hash,
            })
        }
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
        }),
    }
}

fn layout_snapshot(app: &App) -> Value {
    app.broadcast("KEYBOARD_LAYOUT", Vec::new())
        .unwrap_or_else(|error| {
            json!({
                "ok": false,
                "error": error.to_string(),
            })
        })
}

fn ensure_command_ok(value: &Value, action: &str) -> CliResult<()> {
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(CliError::new(format!("{action} failed: {value}")))
}

fn cleanup_after_record_failure(
    app: &App,
    capture: Option<&mut GeteventCapture>,
    input_controller: &mut InputControllerCapture<'_>,
) -> Value {
    let capture_stop = capture.map_or(Value::Null, |active_capture| {
        active_capture.stop().unwrap_or_else(|error| {
            json!({
                "ok": false,
                "error": error.to_string(),
            })
        })
    });
    let input_controller_stop = input_controller.stop();
    let ime_stop = app.broadcast("STOP", Vec::new()).unwrap_or_else(|error| {
        json!({
            "ok": false,
            "error": error.to_string(),
        })
    });
    json!({
        "capture_stop": capture_stop,
        "input_controller_stop": input_controller_stop,
        "ime_stop": ime_stop,
    })
}

fn wait_for_stop(maybe_duration_ms: Option<u64>) -> CliResult<Value> {
    if let Some(capture_duration_ms) = maybe_duration_ms {
        thread::sleep(Duration::from_millis(capture_duration_ms));
        return Ok(json!({
            "stop_mode": "duration_ms",
            "duration_ms": capture_duration_ms,
        }));
    }
    let mut line = String::new();
    let bytes = io::stdin().read_line(&mut line)?;
    Ok(json!({
        "stop_mode": "stdin_enter",
        "stdin_bytes": bytes,
    }))
}

fn prepare_paths(out: &Path) -> CliResult<RecordPaths> {
    let root = out.to_path_buf();
    let ime = root.join("ime");
    let adb = root.join("adb");
    let derived = root.join("derived");
    let evidence = root.join("evidence");
    let ime_pull_tmp = root.join("ime-pull-tmp");
    fs::create_dir_all(&ime)?;
    fs::create_dir_all(&adb)?;
    fs::create_dir_all(&derived)?;
    if ime_pull_tmp.exists() {
        fs::remove_dir_all(&ime_pull_tmp)?;
    }
    Ok(RecordPaths {
        manifest: root.join("manifest.json"),
        validation: root.join("validation.json"),
        getevent_raw: adb.join("getevent.raw.log"),
        getevent_jsonl: adb.join("getevent.jsonl"),
        getevent_stderr: adb.join("getevent.stderr.log"),
        root,
        ime,
        adb,
        derived,
        evidence,
        ime_pull_tmp,
    })
}

fn stage_ime_logs(pull_dir: &Path, ime_dir: &Path) -> CliResult<Vec<String>> {
    let pulled_log_dir = pull_dir.join(LOG_DIR);
    let mut staged = Vec::new();
    for entry_result in fs::read_dir(&pulled_log_dir)? {
        let entry = entry_result?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let destination = ime_dir.join(entry.file_name());
        fs::copy(entry.path(), &destination)?;
        staged.push(path_string(&destination)?);
    }
    staged.sort();
    fs::remove_dir_all(pull_dir)?;
    Ok(staged)
}

struct ManifestParts {
    host_start_wall_ms: u64,
    host_stop_wall_ms: u64,
    start: Value,
    wait: Value,
    capture_stop: Value,
    pre_stop: Value,
    clear: Value,
    touchscreen_profile: Value,
    layout_before_capture: Value,
    layout_after_capture: Value,
    evidence_start: Value,
    evidence_end: Value,
    input_controller: Value,
    input_controller_stop: Value,
    stop: Value,
    pull: Value,
    validation: Value,
    getevent_normalization: Value,
    ime_files: Vec<String>,
}

fn manifest_json(
    app: &App,
    config: &RecordConfig,
    paths: &RecordPaths,
    parts: &ManifestParts,
) -> CliResult<Value> {
    Ok(json!({
        "schema": "input_dynamics_record_manifest.v1",
        "external_run_id": config.run_id.as_str(),
        "package_name": app.package(),
        "input_actor": config.input_actor,
        "input_controller": record_input_controller(config),
        "input_cadence_policy": config.input_cadence_policy,
        "host_start_wall_ms": parts.host_start_wall_ms,
        "host_stop_wall_ms": parts.host_stop_wall_ms,
        "output_dir": path_string(&paths.root)?,
        "ime_dir": path_string(&paths.ime)?,
        "adb_dir": path_string(&paths.adb)?,
        "derived_dir": path_string(&paths.derived)?,
        "evidence_dir": path_string(&paths.evidence)?,
        "getevent_raw_log": path_string(&paths.getevent_raw)?,
        "getevent_jsonl": path_string(&paths.getevent_jsonl)?,
        "getevent_stderr_log": path_string(&paths.getevent_stderr)?,
        "ime_files": &parts.ime_files,
        "device": device_json(app),
        "input_controller_runtime": parts.input_controller,
        "input_backend": parts.input_controller
            .pointer("/summary/input_backend")
            .cloned()
            .unwrap_or(Value::Null),
        "input_device_command": parts.input_controller
            .pointer("/summary/input_device_command")
            .cloned()
            .unwrap_or(Value::Null),
        "coordinate_frame": manifest_coordinate_frame(
            &parts.touchscreen_profile,
            &[
                ("layout_before_capture", &parts.layout_before_capture),
                ("layout_after_capture", &parts.layout_after_capture),
            ],
        ),
        "evidence": {
            "enabled": config.with_evidence,
            "policy": if config.with_evidence { "start_end" } else { "none" },
            "accessibility_detail": if config.full_accessibility_evidence {
                "full"
            } else {
                "compressed"
            },
            "start": parts.evidence_start,
            "end": parts.evidence_end,
        },
        "commands": {
            "start": parts.start,
            "wait": parts.wait,
            "capture_stop": parts.capture_stop,
            "pre_stop": parts.pre_stop,
            "clear": parts.clear,
            "touchscreen_profile": parts.touchscreen_profile,
            "layout_before_capture": parts.layout_before_capture,
            "layout_after_capture": parts.layout_after_capture,
            "evidence_start": parts.evidence_start,
            "evidence_end": parts.evidence_end,
            "input_controller_stop": parts.input_controller_stop,
            "stop": parts.stop,
            "pull": parts.pull,
            "validation": parts.validation,
            "getevent_normalization": parts.getevent_normalization,
        },
    }))
}

fn record_input_controller(config: &RecordConfig) -> Option<String> {
    config.input_controller.as_ref().map_or_else(
        || {
            config
                .with_input_controller
                .then(|| String::from(DEFAULT_RECORD_INPUT_CONTROLLER))
        },
        |controller| Some(controller.clone()),
    )
}

fn input_controller_result_ok(value: &Value) -> bool {
    if value.get("enabled").and_then(Value::as_bool) == Some(false) {
        return true;
    }
    value.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn input_controller_summary(status_after_start: &Value, stop: &Value) -> Value {
    let state = status_after_start
        .get("state")
        .cloned()
        .or_else(|| status_after_start.pointer("/controller/state").cloned())
        .unwrap_or(Value::Null);
    let command_state = stop
        .pointer("/status_before_stop/state")
        .cloned()
        .or_else(|| {
            stop.pointer("/status_before_stop/controller/state")
                .cloned()
        })
        .unwrap_or_else(|| state.clone());
    json!({
        "input_backend": state
            .get("input_backend")
            .cloned()
            .unwrap_or(Value::Null),
        "input_device_command": state
            .get("input_device_command")
            .cloned()
            .unwrap_or(Value::Null),
        "input_profile": state
            .get("input_profile")
            .cloned()
            .unwrap_or(Value::Null),
        "physical_touchscreen_profile_hash": state
            .get("physical_touchscreen_profile_hash")
            .cloned()
            .unwrap_or(Value::Null),
        "physical_touchscreen": state
            .get("physical_touchscreen")
            .cloned()
            .unwrap_or(Value::Null),
        "virtual_touchscreen": state
            .get("virtual_touchscreen")
            .cloned()
            .unwrap_or(Value::Null),
        "virtual_touchscreen_event_path": state
            .pointer("/virtual_touchscreen/profile/event_path")
            .cloned()
            .unwrap_or(Value::Null),
        "command_sequence": command_state
            .get("command_sequence")
            .cloned()
            .unwrap_or(Value::Null),
        "current_command": command_state
            .get("current_command")
            .cloned()
            .unwrap_or(Value::Null),
        "last_command": command_state
            .get("last_command")
            .cloned()
            .unwrap_or(Value::Null),
        "last_error": command_state
            .get("last_error")
            .cloned()
            .unwrap_or(Value::Null),
        "cleanup": stop
            .pointer("/stop/cleanup")
            .cloned()
            .unwrap_or(Value::Null),
    })
}

fn device_json(app: &App) -> Value {
    json!({
        "serial": adb_value(app, &[String::from("get-serialno")]),
        "model": adb_value(app, &getprop_args("ro.product.model")),
        "manufacturer": adb_value(app, &getprop_args("ro.product.manufacturer")),
        "api_level": adb_value(app, &getprop_args("ro.build.version.sdk")),
        "release": adb_value(app, &getprop_args("ro.build.version.release")),
    })
}

fn adb_value(app: &App, args: &[String]) -> Value {
    match app.adb(args, FailureMode::AllowFailure) {
        Ok(output) => json!({
            "ok": output.status_code == Some(0_i32),
            "value": output.stdout().trim(),
            "process": output.json(),
        }),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
        }),
    }
}

fn getprop_args(name: &str) -> Vec<String> {
    vec![
        String::from("shell"),
        String::from("getprop"),
        String::from(name),
    ]
}

fn write_json_file(path: &Path, value: &Value) -> CliResult<()> {
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, format!("{text}\n"))?;
    Ok(())
}

fn epoch_millis() -> CliResult<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::new(format!("system clock is before Unix epoch: {error}")))?
        .as_millis();
    u64::try_from(millis).map_err(|error| CliError::new(format!("millis overflow: {error}")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use crate::record::{
        RecordConfig, input_controller_result_ok, input_controller_summary, record_input_controller,
    };

    #[test]
    fn record_input_controller_defaults_when_runtime_is_requested() {
        let config = RecordConfig {
            run_id: String::from("run-test"),
            out: PathBuf::from("runs/run-test"),
            duration_ms: Some(1_u64),
            with_input_controller: true,
            with_evidence: false,
            full_accessibility_evidence: false,
            input_actor: String::from("agent_adb"),
            input_controller: None,
            input_cadence_policy: String::from("manual"),
        };

        assert_eq!(
            record_input_controller(&config),
            Some(String::from("input-dynamics-cli")),
            "record should default controller provenance when runtime controller is requested"
        );
    }

    #[test]
    fn input_controller_summary_extracts_runtime_identity_and_cleanup() {
        let status = json!({
            "state": {
                "input_backend": "uinput",
                "input_device_command": "/system/bin/uinput",
                "physical_touchscreen_profile_hash": "hash",
                "physical_touchscreen": {"event_path": "/dev/input/event3"},
                "virtual_touchscreen": {
                    "profile": {"event_path": "/dev/input/event4"}
                }
            }
        });
        let stop = json!({
            "status_before_stop": {
                "state": {
                    "command_sequence": 2_u64,
                    "current_command": null,
                    "last_command": {
                        "command": "path",
                        "status": "completed"
                    },
                    "last_error": null
                }
            },
            "stop": {
                "cleanup": {
                    "virtual_touchscreen": {
                        "ok": true,
                        "present": false
                    }
                }
            }
        });

        let summary = input_controller_summary(&status, &stop);

        assert_eq!(
            summary.get("input_backend").and_then(Value::as_str),
            Some("uinput"),
            "summary should expose the backend"
        );
        assert_eq!(
            summary.get("input_device_command").and_then(Value::as_str),
            Some("/system/bin/uinput"),
            "summary should expose the uinput command"
        );
        assert_eq!(
            summary
                .get("virtual_touchscreen_event_path")
                .and_then(Value::as_str),
            Some("/dev/input/event4"),
            "summary should expose the virtual event path"
        );
        assert!(
            summary
                .pointer("/cleanup/virtual_touchscreen/present")
                .and_then(Value::as_bool)
                == Some(false),
            "summary should expose virtual-device cleanup state"
        );
        assert_eq!(
            summary.get("command_sequence").and_then(Value::as_u64),
            Some(2),
            "summary should expose the latest command sequence"
        );
        assert_eq!(
            summary
                .pointer("/last_command/command")
                .and_then(Value::as_str),
            Some("path"),
            "summary should expose the latest controller command"
        );
    }

    #[test]
    fn disabled_input_controller_result_is_ok() {
        let disabled = json!({
            "enabled": false,
            "requested": false
        });

        assert!(
            input_controller_result_ok(&disabled),
            "disabled runtime controller should not make record fail"
        );
    }
}
