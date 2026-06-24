//! Scientific run capture orchestration.

use std::fmt::Write;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::thread;
use std::time::Duration;

use input_dynamics_analysis::getevent::{GETEVENT_SCHEMA, normalize_file};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::app::{App, LOG_DIR};
use crate::clock_probe::{capture_device_clock_probe, host_wall_millis, validate_probe_order};
use crate::commands::{normalize_stats_json, path_string, pull_logs};
use crate::controller::{self, SessionStartPermit};
use crate::coordinate_frame::manifest_coordinate_frame;
use crate::error::{CliError, CliResult};
use crate::observe::{self, AccessibilityDetail};
use crate::process::{FailureMode, run_process, spawn_process_to_files};
use crate::uinput;
use crate::validate::validate_logs;

const DEFAULT_RECORD_INPUT_CONTROLLER: &str = "input-dynamics-cli";
const EVIDENCE_CAPTURE_SCHEMA: &str = "input_dynamics_record_evidence_capture.v1";
const VIDEO_SCHEMA: &str = "input_dynamics_video_capture.v1";
const VIDEO_STARTUP_CHECK_DELAY: Duration = Duration::from_millis(250);

pub(crate) struct RecordConfig {
    pub(crate) run_id: String,
    pub(crate) out: PathBuf,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) with_input_controller: bool,
    pub(crate) with_evidence: bool,
    pub(crate) full_accessibility_evidence: bool,
    pub(crate) video_mode: VideoMode,
    pub(crate) input_actor: String,
    pub(crate) input_controller: Option<String>,
    pub(crate) input_cadence_policy: String,
}

#[derive(Clone, Copy)]
pub(crate) enum VideoMode {
    Enabled,
    Disabled,
}

impl VideoMode {
    const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

struct RecordPaths {
    root: PathBuf,
    ime: PathBuf,
    adb: PathBuf,
    derived: PathBuf,
    evidence: PathBuf,
    video: PathBuf,
    manifest: PathBuf,
    validation: PathBuf,
    getevent_raw: PathBuf,
    getevent_jsonl: PathBuf,
    getevent_stderr: PathBuf,
    video_screen: PathBuf,
    video_timing: PathBuf,
    video_stdout: PathBuf,
    video_stderr: PathBuf,
    video_pull_log: PathBuf,
    ime_pull_tmp: PathBuf,
}

struct GeteventCapture {
    child: Option<Child>,
}

struct VideoCapture {
    enabled: bool,
    required: bool,
    remote_path: String,
    local_path: PathBuf,
    timing_path: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    pull_log_path: PathBuf,
    start: Value,
    stop: Value,
    child: Option<Child>,
    stopped: bool,
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

struct CleanupGuards<'app, 'guard> {
    video: &'guard mut VideoCapture,
    input_controller: &'guard mut InputControllerCapture<'app>,
}

impl Drop for GeteventCapture {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _kill_result = child.kill();
            let _wait_result = child.wait();
        }
    }
}

impl Drop for VideoCapture {
    fn drop(&mut self) {
        if self.enabled && !self.stopped {
            if let Some(child) = self.child.as_mut() {
                let _signal = interrupt_child(child);
                thread::sleep(Duration::from_millis(250));
                if child.try_wait().ok().flatten().is_none() {
                    let _kill = child.kill();
                }
                let _wait = child.wait();
            }
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

impl VideoCapture {
    fn start(app: &App, config: &RecordConfig, paths: &RecordPaths) -> CliResult<Self> {
        if !config.video_mode.is_enabled() {
            return Ok(Self::disabled(paths));
        }

        fs::create_dir_all(&paths.video)?;
        let remote_path = remote_video_path(&config.run_id);
        let cleanup_before = remove_remote_file(app, &remote_path);
        let timing_before = capture_device_clock_probe(app, "before_screenrecord_start")?;
        let args = vec![
            String::from("shell"),
            String::from("screenrecord"),
            remote_path.clone(),
        ];
        let scoped_args = app.scoped_adb_args(&args)?;
        let mut child = spawn_process_to_files(
            app.adb_program(),
            &scoped_args,
            &paths.video_stdout,
            &paths.video_stderr,
        )?;
        thread::sleep(VIDEO_STARTUP_CHECK_DELAY);
        let startup_status = child.try_wait()?;
        if let Some(status) = startup_status {
            return Err(CliError::new(format!(
                "screenrecord exited during startup with status_code={:?}; stderr_log={}",
                status.code(),
                paths.video_stderr.display()
            )));
        }
        let timing_after = match capture_device_clock_probe(app, "after_screenrecord_start") {
            Ok(value) => value,
            Err(error) => {
                let _signal = interrupt_child(&child);
                let _wait = child.wait();
                return Err(error);
            }
        };
        validate_probe_order(&timing_before, &timing_after)?;
        Ok(Self {
            enabled: true,
            required: true,
            remote_path: remote_path.clone(),
            local_path: paths.video_screen.clone(),
            timing_path: paths.video_timing.clone(),
            stdout_path: paths.video_stdout.clone(),
            stderr_path: paths.video_stderr.clone(),
            pull_log_path: paths.video_pull_log.clone(),
            start: json!({
                "ok": true,
                "enabled": true,
                "required": true,
                "requested": true,
                "schema": VIDEO_SCHEMA,
                "adb_program": app.adb_program(),
                "adb_args": scoped_args,
                "screenrecord_command": ["screenrecord", remote_path],
                "remote_path": remote_path,
                "local_path": path_string(&paths.video_screen)?,
                "timing_path": path_string(&paths.video_timing)?,
                "stdout_log": path_string(&paths.video_stdout)?,
                "stderr_log": path_string(&paths.video_stderr)?,
                "pull_log": path_string(&paths.video_pull_log)?,
                "cleanup_before": cleanup_before,
                "before": timing_before,
                "after": timing_after,
            }),
            stop: Value::Null,
            child: Some(child),
            stopped: false,
        })
    }

    fn disabled(paths: &RecordPaths) -> Self {
        Self {
            enabled: false,
            required: false,
            remote_path: String::new(),
            local_path: paths.video_screen.clone(),
            timing_path: paths.video_timing.clone(),
            stdout_path: paths.video_stdout.clone(),
            stderr_path: paths.video_stderr.clone(),
            pull_log_path: paths.video_pull_log.clone(),
            start: json!({
                "ok": true,
                "enabled": false,
                "required": false,
                "requested": false,
                "disabled_reason": "--no-video",
            }),
            stop: Value::Null,
            child: None,
            stopped: true,
        }
    }

    fn stop(&mut self, app: &App) -> CliResult<Value> {
        if !self.enabled {
            self.stopped = true;
            self.stop = json!({
                "ok": true,
                "enabled": false,
                "required": false,
                "requested": false,
                "disabled_reason": "--no-video",
            });
            write_json_file(&self.timing_path, &self.to_json())?;
            return Ok(self.stop.clone());
        }

        let before = capture_device_clock_probe(app, "before_screenrecord_stop")?;
        if let Some(previous) = self.start.get("after") {
            validate_probe_order(previous, &before)?;
        }
        let Some(mut child) = self.child.take() else {
            return Err(CliError::new("screenrecord child was already taken"));
        };
        let already_exited_status = child.try_wait()?;
        let signal = if already_exited_status.is_none() {
            interrupt_child(&child)
        } else {
            json!({
                "ok": true,
                "already_exited": true,
            })
        };
        if already_exited_status.is_none() {
            thread::sleep(Duration::from_millis(500));
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
        }
        let status = child.wait()?;
        let after = capture_device_clock_probe(app, "after_screenrecord_stop")?;
        validate_probe_order(&before, &after)?;
        let pull = pull_video(
            app,
            &self.remote_path,
            &self.local_path,
            &self.pull_log_path,
        )?;
        let file = file_fingerprint(&self.local_path)?;
        let byte_count = file
            .get("byte_count")
            .and_then(Value::as_u64)
            .unwrap_or(0_u64);
        if byte_count == 0_u64 {
            return Err(CliError::new(format!(
                "screenrecord video is empty: {}",
                self.local_path.display()
            )));
        }
        let remote_cleanup = remove_remote_file(app, &self.remote_path);
        self.stop = json!({
            "ok": true,
            "enabled": true,
            "required": true,
            "requested": true,
            "already_exited_status_code": already_exited_status.and_then(|wait_status| wait_status.code()),
            "signal": signal,
            "status_code": status.code(),
            "success": status.success(),
            "before": before,
            "after": after,
            "pull": pull,
            "remote_cleanup": remote_cleanup,
            "file": file,
        });
        self.stopped = true;
        write_json_file(&self.timing_path, &self.to_json())?;
        Ok(self.stop.clone())
    }

    fn to_json(&self) -> Value {
        if !self.enabled {
            return json!({
                "enabled": false,
                "required": false,
                "schema": VIDEO_SCHEMA,
                "disabled_reason": "--no-video",
                "local_path": path_string_lossy(&self.local_path),
                "timing_path": path_string_lossy(&self.timing_path),
                "start": self.start,
                "stop": self.stop,
            });
        }
        json!({
            "enabled": true,
            "required": self.required,
            "schema": VIDEO_SCHEMA,
            "remote_path": self.remote_path,
            "local_path": path_string_lossy(&self.local_path),
            "timing_path": path_string_lossy(&self.timing_path),
            "stdout_log": path_string_lossy(&self.stdout_path),
            "stderr_log": path_string_lossy(&self.stderr_path),
            "pull_log": path_string_lossy(&self.pull_log_path),
            "start": self.start,
            "stop": self.stop,
            "file": self.stop.get("file").cloned().unwrap_or(Value::Null),
        })
    }
}

impl<'a> InputControllerCapture<'a> {
    fn acquire_start_lock(
        app: &App,
        config: &RecordConfig,
    ) -> CliResult<Option<controller::SessionStartLock>> {
        if !config.with_input_controller {
            return Ok(None);
        }

        match controller::acquire_session_start(app, &config.run_id)? {
            SessionStartPermit::Acquired(session_lock) => Ok(Some(session_lock)),
            SessionStartPermit::Busy(status) => Err(record_input_controller_busy_error(&status)),
        }
    }

    fn start(
        app: &'a App,
        config: &RecordConfig,
        session_lock: Option<controller::SessionStartLock>,
    ) -> CliResult<Self> {
        if !config.with_input_controller {
            return Ok(Self::disabled(app));
        }
        let Some(acquired_session_lock) = session_lock else {
            return Err(CliError::new(
                "record input controller start missing acquired start lock",
            ));
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
            session_lock: Some(acquired_session_lock),
            stopped: false,
        })
    }

    const fn disabled(app: &'a App) -> Self {
        Self {
            app,
            enabled: false,
            start: Value::Null,
            status_after_start: Value::Null,
            ready: Value::Null,
            stop: Value::Null,
            session_lock: None,
            stopped: true,
        }
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
    let paths = prepare_record_paths(config)?;
    let host_start_wall_ms = host_wall_millis()?;
    let ActiveRecordWindow {
        pre_stop,
        clear,
        start,
        mut input_controller,
    } = start_record_window(app, config)?;
    let touchscreen_profile = touchscreen_profile_snapshot(app);
    let layout_before_capture = layout_snapshot(app);
    let mut video = start_video_capture_or_cleanup(app, config, &paths, &mut input_controller)?;
    let evidence_start = capture_evidence_or_cleanup(
        app,
        config,
        &paths,
        "start",
        &mut CleanupGuards {
            video: &mut video,
            input_controller: &mut input_controller,
        },
    )?;
    let mut capture =
        start_getevent_capture_or_cleanup(app, &paths, &mut video, &mut input_controller)?;
    mark_input_controller_ready_or_cleanup(app, &mut capture, &mut video, &mut input_controller)?;
    let wait =
        wait_for_stop_or_cleanup(app, config, &mut capture, &mut video, &mut input_controller)?;
    let capture_stop = stop_capture_or_cleanup(app, capture, &mut video, &mut input_controller)?;
    let layout_after_capture = layout_snapshot(app);
    let evidence_end = capture_evidence_or_cleanup(
        app,
        config,
        &paths,
        "end",
        &mut CleanupGuards {
            video: &mut video,
            input_controller: &mut input_controller,
        },
    )?;
    let video_stop = stop_video_capture_or_cleanup(app, &mut video, &mut input_controller)?;
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
    let host_stop_wall_ms = host_wall_millis()?;
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
        video: video.to_json(),
        video_stop,
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

    record_result_json(app, config, &paths, &validation, &manifest_parts)
}

fn record_result_json(
    app: &App,
    config: &RecordConfig,
    paths: &RecordPaths,
    validation: &Value,
    manifest_parts: &ManifestParts,
) -> CliResult<Value> {
    Ok(json!({
        "ok": validation.get("ok").and_then(Value::as_bool).unwrap_or(false)
            && video_result_ok(&manifest_parts.video_stop)
            && input_controller_result_ok(&manifest_parts.input_controller_stop),
        "package_name": app.package(),
        "external_run_id": config.run_id.as_str(),
        "output_dir": path_string(&paths.root)?,
        "manifest": path_string(&paths.manifest)?,
        "validation": validation,
        "getevent_normalization": manifest_parts.getevent_normalization.clone(),
        "input_controller": manifest_parts.input_controller.clone(),
        "video": manifest_parts.video.clone(),
        "evidence": {
            "enabled": config.with_evidence,
            "policy": if config.with_evidence { "start_end" } else { "none" },
            "start": manifest_parts.evidence_start.clone(),
            "end": manifest_parts.evidence_end.clone(),
        },
    }))
}

fn start_record_window<'a>(
    app: &'a App,
    config: &RecordConfig,
) -> CliResult<ActiveRecordWindow<'a>> {
    let session_lock = InputControllerCapture::acquire_start_lock(app, config)?;
    let pre_stop = app.broadcast("STOP", Vec::new())?;
    let clear = app.broadcast("CLEAR_LOGS", Vec::new())?;
    ensure_command_ok(&clear, "clear logs before record")?;
    let start = start_record_session(app, config)?;
    ensure_command_ok(&start, "start record session")?;
    let input_controller = match InputControllerCapture::start(app, config, session_lock) {
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

fn record_input_controller_busy_error(status: &Value) -> CliError {
    CliError::with_details(
        "input controller is busy during record start; no record side effects were attempted",
        json!({
            "error_code": "input_controller_busy",
            "busy": true,
            "mutated": false,
            "controller_status": status,
            "suggested_next_command": {
                "argv": ["input-dynamics", "controller", "status"],
                "reason": "inspect the active diagnostic input controller before starting a controller-backed record",
            },
            "diagnostic_only": true,
        }),
    )
}

fn start_getevent_capture_or_cleanup(
    app: &App,
    paths: &RecordPaths,
    video: &mut VideoCapture,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<GeteventCapture> {
    GeteventCapture::start(app, paths).map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, None, Some(video), input_controller);
        CliError::new(format!(
            "failed to start getevent capture: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn start_video_capture_or_cleanup(
    app: &App,
    config: &RecordConfig,
    paths: &RecordPaths,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<VideoCapture> {
    VideoCapture::start(app, config, paths).map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, None, None, input_controller);
        CliError::new(format!(
            "failed to start screenrecord capture: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn mark_input_controller_ready_or_cleanup(
    app: &App,
    capture: &mut GeteventCapture,
    video: &mut VideoCapture,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<Value> {
    input_controller.mark_ready().map_err(|error| {
        let cleanup =
            cleanup_after_record_failure(app, Some(capture), Some(video), input_controller);
        CliError::new(format!(
            "failed to mark record input controller ready: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn wait_for_stop_or_cleanup(
    app: &App,
    config: &RecordConfig,
    capture: &mut GeteventCapture,
    video: &mut VideoCapture,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<Value> {
    wait_for_stop(config.duration_ms).map_err(|error| {
        let cleanup =
            cleanup_after_record_failure(app, Some(capture), Some(video), input_controller);
        CliError::new(format!(
            "failed while waiting for record stop: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn stop_capture_or_cleanup(
    app: &App,
    mut capture: GeteventCapture,
    video: &mut VideoCapture,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<Value> {
    capture.stop().map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, None, Some(video), input_controller);
        CliError::new(format!(
            "failed to stop getevent capture: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn stop_video_capture_or_cleanup(
    app: &App,
    video: &mut VideoCapture,
    input_controller: &mut InputControllerCapture<'_>,
) -> CliResult<Value> {
    video.stop(app).map_err(|error| {
        let cleanup = cleanup_after_record_failure(app, None, None, input_controller);
        CliError::new(format!(
            "failed to stop screenrecord capture: {error}; cleanup attempted: {cleanup}"
        ))
    })
}

fn capture_evidence_or_cleanup(
    app: &App,
    config: &RecordConfig,
    paths: &RecordPaths,
    phase: &str,
    guards: &mut CleanupGuards<'_, '_>,
) -> CliResult<Value> {
    record_evidence(app, config, paths, phase).map_err(|error| {
        let cleanup = cleanup_after_record_failure(
            app,
            None,
            Some(&mut *guards.video),
            &mut *guards.input_controller,
        );
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
            "schema": EVIDENCE_CAPTURE_SCHEMA,
            "enabled": false,
            "requested": false,
            "phase": phase,
        }));
    }
    let phase_dir = paths.evidence.join(phase);
    let before = capture_device_clock_probe(app, &format!("before_evidence_{phase}"))?;
    let bundle = observe::all(app, &phase_dir, record_accessibility_detail(config))?;
    let after = capture_device_clock_probe(app, &format!("after_evidence_{phase}"))?;
    validate_probe_order(&before, &after)?;
    Ok(json!({
        "schema": EVIDENCE_CAPTURE_SCHEMA,
        "enabled": true,
        "requested": true,
        "phase": phase,
        "policy": "start_end",
        "clock_domain": "device_elapsed_realtime_ns",
        "clock_alignment_status": "bracketed",
        "before": before,
        "after": after,
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
    video: Option<&mut VideoCapture>,
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
    let video_stop = video.map_or(Value::Null, |active_video| {
        active_video.stop(app).unwrap_or_else(|error| {
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
        "video_stop": video_stop,
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
    let stdin_handle = io::stdin();
    let mut locked_stdin = stdin_handle.lock();
    wait_for_stdin_enter(&mut locked_stdin)
}

fn wait_for_stdin_enter(reader: &mut impl BufRead) -> CliResult<Value> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;
    if bytes == 0_usize {
        return Err(record_stdin_unavailable_error("stdin_eof"));
    }
    Ok(json!({
        "stop_mode": "stdin_enter",
        "stdin_bytes": bytes,
    }))
}

fn validate_record_stop_mode(config: &RecordConfig) -> CliResult<()> {
    ensure_record_stop_mode(config.duration_ms)
}

fn prepare_record_paths(config: &RecordConfig) -> CliResult<RecordPaths> {
    validate_record_stop_mode(config)?;
    prepare_paths(&config.out)
}

fn ensure_record_stop_mode(maybe_duration_ms: Option<u64>) -> CliResult<()> {
    match maybe_duration_ms {
        Some(duration_ms) if duration_ms > 0_u64 => Ok(()),
        Some(duration_ms) => Err(record_invalid_duration_error(duration_ms)),
        None => Err(record_stdin_unavailable_error("duration_required")),
    }
}

fn record_stdin_unavailable_error(reason: &str) -> CliError {
    CliError::with_details(
        "record requires --duration-ms during the session workflow migration",
        json!({
            "error_code": "record_stdin_unavailable",
            "reason": reason,
            "safe_current_command": "input-dynamics record --run-id <run-id> --out <run-dir> --duration-ms <positive-ms>",
            "current_capture_workflow": "input-dynamics record --run-id <run-id> --out <run-dir> --duration-ms <positive-ms>",
            "future_workflow": "umbrella_session",
            "future_only": true,
            "migration_note": "record is a transitional foreground capture path and will be removed before release",
        }),
    )
}

fn record_invalid_duration_error(duration_ms: u64) -> CliError {
    CliError::with_details(
        "record requires a positive --duration-ms value during the session workflow migration",
        json!({
            "error_code": "record_invalid_duration",
            "reason": "duration_must_be_positive",
            "duration_ms": duration_ms,
            "safe_current_command": "input-dynamics record --run-id <run-id> --out <run-dir> --duration-ms <positive-ms>",
            "current_capture_workflow": "input-dynamics record --run-id <run-id> --out <run-dir> --duration-ms <positive-ms>",
            "future_workflow": "umbrella_session",
            "future_only": true,
            "migration_note": "record is a transitional foreground capture path and will be removed before release",
        }),
    )
}

fn prepare_paths(out: &Path) -> CliResult<RecordPaths> {
    let root = out.to_path_buf();
    let ime = root.join("ime");
    let adb = root.join("adb");
    let derived = root.join("derived");
    let evidence = root.join("evidence");
    let video = root.join("video");
    let ime_pull_tmp = root.join("ime-pull-tmp");
    fs::create_dir_all(&ime)?;
    fs::create_dir_all(&adb)?;
    fs::create_dir_all(&derived)?;
    fs::create_dir_all(&video)?;
    if ime_pull_tmp.exists() {
        fs::remove_dir_all(&ime_pull_tmp)?;
    }
    Ok(RecordPaths {
        manifest: root.join("manifest.json"),
        validation: root.join("validation.json"),
        getevent_raw: adb.join("getevent.raw.log"),
        getevent_jsonl: adb.join("getevent.jsonl"),
        getevent_stderr: adb.join("getevent.stderr.log"),
        video_screen: video.join("screen.mp4"),
        video_timing: video.join("timing.json"),
        video_stdout: video.join("screenrecord.stdout.log"),
        video_stderr: video.join("screenrecord.stderr.log"),
        video_pull_log: video.join("adb-pull-video.log"),
        root,
        ime,
        adb,
        derived,
        evidence,
        video,
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
        if !should_stage_ime_file(&entry.path()) {
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

fn should_stage_ime_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let is_jsonl = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
    file_name == "input_dynamics_control_status.json"
        || (file_name.starts_with("session-") && is_jsonl)
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
    video: Value,
    video_stop: Value,
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
    let coordinate_frame = manifest_coordinate_frame(
        &parts.touchscreen_profile,
        &[
            ("layout_before_capture", &parts.layout_before_capture),
            ("layout_after_capture", &parts.layout_after_capture),
        ],
    );
    let evidence = json!({
        "enabled": config.with_evidence,
        "policy": if config.with_evidence { "start_end" } else { "none" },
        "accessibility_detail": if config.full_accessibility_evidence {
            "full"
        } else {
            "compressed"
        },
        "start": parts.evidence_start,
        "end": parts.evidence_end,
    });
    let commands = json!({
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
        "video_stop": parts.video_stop,
        "input_controller_stop": parts.input_controller_stop,
        "stop": parts.stop,
        "pull": parts.pull,
        "validation": parts.validation,
        "getevent_normalization": parts.getevent_normalization,
    });
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
        "video_dir": path_string(&paths.video)?,
        "getevent_raw_log": path_string(&paths.getevent_raw)?,
        "getevent_jsonl": path_string(&paths.getevent_jsonl)?,
        "getevent_stderr_log": path_string(&paths.getevent_stderr)?,
        "video": parts.video,
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
        "coordinate_frame": coordinate_frame,
        "evidence": evidence,
        "commands": commands,
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

fn video_result_ok(value: &Value) -> bool {
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

fn remote_video_path(run_id: &str) -> String {
    format!(
        "/data/local/tmp/input-dynamics-{}-screen.mp4",
        sanitize_remote_component(run_id)
    )
}

fn sanitize_remote_component(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn interrupt_child(child: &Child) -> Value {
    interrupt_process_id(child.id())
}

fn interrupt_process_id(pid: u32) -> Value {
    #[cfg(unix)]
    {
        let process_group = format!("-{pid}");
        let args = vec![String::from("-INT"), process_group];
        run_process("kill", &args, FailureMode::AllowFailure).map_or_else(
            |error| {
                json!({
                    "ok": false,
                    "method": "kill_process_group_int",
                    "pid": pid,
                    "error": error.to_string(),
                })
            },
            |output| {
                json!({
                    "ok": output.status_code == Some(0_i32),
                    "method": "kill_process_group_int",
                    "pid": pid,
                    "process": output.json(),
                })
            },
        )
    }
    #[cfg(not(unix))]
    {
        json!({
            "ok": false,
            "method": "unsupported_platform",
            "pid": pid,
        })
    }
}

fn pull_video(
    app: &App,
    remote_path: &str,
    local_path: &Path,
    pull_log_path: &Path,
) -> CliResult<Value> {
    let args = vec![
        String::from("pull"),
        remote_path.to_owned(),
        path_string(local_path)?,
    ];
    let output = app.adb(&args, FailureMode::AllowFailure)?;
    let value = json!({
        "ok": output.status_code == Some(0_i32),
        "remote_path": remote_path,
        "local_path": path_string(local_path)?,
        "process": output.json(),
    });
    write_json_file(pull_log_path, &value)?;
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(value)
    } else {
        Err(CliError::new(format!(
            "failed to pull screenrecord video: {value}"
        )))
    }
}

fn remove_remote_file(app: &App, remote_path: &str) -> Value {
    app.adb_shell(
        vec![
            String::from("rm"),
            String::from("-f"),
            remote_path.to_owned(),
        ],
        FailureMode::AllowFailure,
    )
    .map_or_else(
        |error| {
            json!({
                "ok": false,
                "remote_path": remote_path,
                "error": error.to_string(),
            })
        },
        |output| {
            json!({
                "ok": output.status_code == Some(0_i32),
                "remote_path": remote_path,
                "process": output.json(),
            })
        },
    )
}

fn file_fingerprint(path: &Path) -> CliResult<Value> {
    let metadata = fs::metadata(path)?;
    Ok(json!({
        "byte_count": metadata.len(),
        "sha256": format!("sha256:{}", sha256_file(path)?),
    }))
}

fn sha256_file(path: &Path) -> CliResult<String> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> CliResult<String> {
    let capacity = bytes
        .len()
        .checked_mul(2)
        .ok_or_else(|| CliError::new("hex capacity overflow"))?;
    let mut output = String::with_capacity(capacity);
    for byte in bytes {
        write!(&mut output, "{byte:02x}")
            .map_err(|error| CliError::new(format!("failed to format digest: {error}")))?;
    }
    Ok(output)
}

fn path_string_lossy(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::process;

    use serde_json::{Value, json};

    use crate::record::{
        RecordConfig, VideoMode, ensure_record_stop_mode, input_controller_result_ok,
        input_controller_summary, prepare_record_paths, record_input_controller,
        record_input_controller_busy_error, should_stage_ime_file, wait_for_stdin_enter,
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
            video_mode: VideoMode::Disabled,
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
    fn record_stop_mode_allows_duration_without_terminal_stdin() {
        assert!(
            ensure_record_stop_mode(Some(1_u64)).is_ok(),
            "bounded records should not require stdin"
        );
    }

    #[test]
    fn record_rejects_open_ended_config_before_creating_output_dir() {
        let out = std::env::temp_dir().join(format!(
            "input-dynamics-record-no-duration-{}",
            process::id()
        ));
        let _cleanup_before = fs::remove_dir_all(&out);
        let config = RecordConfig {
            run_id: String::from("run-test"),
            out: out.clone(),
            duration_ms: None,
            with_input_controller: false,
            with_evidence: false,
            full_accessibility_evidence: false,
            video_mode: VideoMode::Disabled,
            input_actor: String::from("human"),
            input_controller: None,
            input_cadence_policy: String::from("manual"),
        };

        let outcome = prepare_record_paths(&config);

        assert!(
            outcome.is_err(),
            "open-ended record config should be rejected"
        );
        assert!(
            !out.exists(),
            "open-ended record rejection must not create artifacts"
        );
        let _cleanup_after = fs::remove_dir_all(&out);
    }

    #[test]
    fn record_stop_mode_rejects_open_ended_record() {
        let outcome = ensure_record_stop_mode(None);

        assert!(
            outcome.is_err(),
            "open-ended record must fail during migration"
        );
        let error_json = outcome
            .err()
            .map_or_else(|| json!({}), |error| error.to_json());

        assert_eq!(
            error_json
                .pointer("/details/error_code")
                .and_then(Value::as_str),
            Some("record_stdin_unavailable"),
            "error should be machine-readable"
        );
        assert_eq!(
            error_json.get("error_code").and_then(Value::as_str),
            Some("record_stdin_unavailable"),
            "error_code should be promoted for agent branching"
        );
        assert_eq!(
            error_json
                .pointer("/details/reason")
                .and_then(Value::as_str),
            Some("duration_required"),
            "error should explain that duration is required"
        );
        assert_record_error_uses_current_safe_workflow(&error_json);
    }

    #[test]
    fn record_stop_mode_rejects_zero_duration() {
        let outcome = ensure_record_stop_mode(Some(0_u64));

        assert!(outcome.is_err(), "zero-duration record must fail");
        let error_json = outcome
            .err()
            .map_or_else(|| json!({}), |error| error.to_json());

        assert_eq!(
            error_json.get("error_code").and_then(Value::as_str),
            Some("record_invalid_duration"),
            "zero duration should have a distinct machine-readable error"
        );
        assert_eq!(
            error_json
                .pointer("/details/reason")
                .and_then(Value::as_str),
            Some("duration_must_be_positive"),
            "zero duration should explain the positive-duration requirement"
        );
        assert_record_error_uses_current_safe_workflow(&error_json);
    }

    fn assert_record_error_uses_current_safe_workflow(error_json: &Value) {
        assert_eq!(
            error_json
                .pointer("/details/current_capture_workflow")
                .and_then(Value::as_str),
            Some(
                "input-dynamics record --run-id <run-id> --out <run-dir> --duration-ms <positive-ms>"
            ),
            "record errors should keep bounded record as current-safe workflow"
        );
        assert!(
            error_json.pointer("/details/canonical_workflow").is_none(),
            "record errors should not name current session workflow during Checkpoint 3"
        );
        assert_eq!(
            error_json
                .pointer("/details/future_workflow")
                .and_then(Value::as_str),
            Some("umbrella_session"),
            "future umbrella workflow should be named without command-shaped guidance"
        );
        assert_eq!(
            error_json
                .pointer("/details/future_only")
                .and_then(Value::as_bool),
            Some(true),
            "future workflow metadata should be explicitly non-current"
        );
        assert!(
            error_json
                .pointer("/details/future_canonical_workflow")
                .is_none(),
            "record errors should not expose future session commands as machine guidance"
        );
    }

    #[test]
    fn record_input_controller_busy_error_is_non_mutating_and_actionable() {
        let status = json!({
            "ok": true,
            "active": true,
            "ready_for_input": true,
        });

        let error_json = record_input_controller_busy_error(&status).to_json();

        assert_eq!(
            error_json.get("error_code").and_then(Value::as_str),
            Some("input_controller_busy"),
            "busy record preflight should have a stable error code"
        );
        assert_eq!(
            error_json
                .pointer("/details/mutated")
                .and_then(Value::as_bool),
            Some(false),
            "busy record preflight must report that no record side effects were attempted"
        );
        assert_eq!(
            error_json
                .pointer("/details/suggested_next_command/argv/1")
                .and_then(Value::as_str),
            Some("controller"),
            "busy record preflight should send agents to controller diagnostics"
        );
    }

    #[test]
    fn record_wait_accepts_enter_from_interactive_stdin() {
        let mut input = Cursor::new(b"\n".as_slice());
        let outcome = wait_for_stdin_enter(&mut input);

        assert!(outcome.is_ok(), "Enter should stop the record: {outcome:?}");
        let result = outcome.unwrap_or_else(|error| json!({"error": error.to_string()}));

        assert_eq!(
            result.get("stop_mode").and_then(Value::as_str),
            Some("stdin_enter")
        );
        assert_eq!(result.get("stdin_bytes").and_then(Value::as_u64), Some(1));
    }

    #[test]
    fn record_wait_rejects_stdin_eof() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let outcome = wait_for_stdin_enter(&mut input);

        assert!(outcome.is_err(), "EOF is not an Enter stop");
        let error_json = outcome
            .err()
            .map_or_else(|| json!({}), |error| error.to_json());

        assert_eq!(
            error_json
                .pointer("/details/error_code")
                .and_then(Value::as_str),
            Some("record_stdin_unavailable"),
            "EOF error should be machine-readable"
        );
        assert_eq!(
            error_json
                .pointer("/details/reason")
                .and_then(Value::as_str),
            Some("stdin_eof"),
            "EOF should be distinguishable from nonterminal preflight"
        );
    }

    #[test]
    fn public_docs_do_not_teach_open_ended_record() {
        let documents = [
            ("README.md", include_str!("../../../README.md")),
            ("docs/cli.md", include_str!("../../../docs/cli.md")),
            (
                "skills/input-dynamics-keyboard/SKILL.md",
                include_str!("../../../skills/input-dynamics-keyboard/SKILL.md"),
            ),
        ];
        let forbidden_phrases = [
            "press Enter",
            "duration-ms is omitted",
            "stdin to stop",
            "requires a real interactive terminal",
        ];

        for (name, contents) in documents {
            for phrase in forbidden_phrases {
                assert!(
                    !contents.contains(phrase),
                    "{name} should not contain stale open-ended record guidance: {phrase}"
                );
            }
        }
    }

    #[test]
    fn public_docs_do_not_teach_controller_only_session_commands() {
        let documents = [
            ("README.md", include_str!("../../../README.md")),
            ("docs/cli.md", include_str!("../../../docs/cli.md")),
            (
                "docs/input-profiles.md",
                include_str!("../../../docs/input-profiles.md"),
            ),
            (
                "docs/releases.md",
                include_str!("../../../docs/releases.md"),
            ),
            (
                "skills/input-dynamics-keyboard/SKILL.md",
                include_str!("../../../skills/input-dynamics-keyboard/SKILL.md"),
            ),
        ];
        let forbidden_phrases = [
            "idk session start",
            "idk session status",
            "idk session stop",
            "input-dynamics session start --run-id",
            "cargo run --quiet -p input-dynamics -- session start",
            "cargo run --quiet -p input-dynamics -- session status",
            "cargo run --quiet -p input-dynamics -- session stop",
            "requires `session start`",
            "poll `session status`",
            "canonical live-input path",
            "stateful session lifecycle",
            "`session start` uses",
        ];

        for (name, contents) in documents {
            for phrase in forbidden_phrases {
                assert!(
                    !contents.contains(phrase),
                    "{name} should not contain stale controller-only session guidance: {phrase}"
                );
            }
        }
    }

    #[test]
    fn stage_ime_filter_keeps_only_session_and_status_files() {
        assert!(
            should_stage_ime_file(&PathBuf::from("session-20260623-test.jsonl")),
            "session JSONL should be staged"
        );
        assert!(
            should_stage_ime_file(&PathBuf::from("input_dynamics_control_status.json")),
            "latest control status should be staged"
        );
        assert!(
            !should_stage_ime_file(&PathBuf::from("input_dynamics_control_result_old.json")),
            "per-command control results are already represented in the manifest"
        );
        assert!(
            !should_stage_ime_file(&PathBuf::from("session-20260623-test.json")),
            "session files must be JSONL"
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
