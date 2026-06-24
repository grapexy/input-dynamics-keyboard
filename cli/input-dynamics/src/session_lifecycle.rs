//! Stateful umbrella session lifecycle orchestration.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use input_dynamics_analysis::getevent::{GETEVENT_SCHEMA, normalize_file};
use serde_json::{Value, json};

use crate::app::{App, LOG_DIR};
use crate::clock_probe::{capture_device_clock_probe, host_wall_millis, validate_probe_order};
use crate::commands::{normalize_stats_json, path_string, pull_logs};
use crate::error::{CliError, CliResult};
use crate::observe::{self, AccessibilityDetail};
use crate::process::FailureMode;
use crate::session_process::{
    HostProcessKind, HostProcessProbe, HostProcessSignaler, ProcessLiveness, SessionProcessSpec,
    StopMethod, StopOutcome, StopPolicy, pre_spawn_descriptor, probe_descriptor,
    start_session_process, stop_process_group,
};
use crate::session_state::io::{
    acquire_lock_exclusive, checked_update_json, read_json_classified, write_json_atomic,
};
use crate::session_state::paths::{RunSessionPaths, RuntimeSessionPaths};
use crate::session_state::schema::{
    ArtifactStatus, CURRENT_SCHEMA, CaptureSessionCommand, CaptureSessionCommandName,
    CaptureSessionCurrent, CaptureSessionLock, CaptureSessionState, FINALIZATION_SCHEMA,
    FinalizationLedger, FinalizationOwner, FinalizationStep, InputProvenance, LOCK_SCHEMA,
    LifecycleSnapshot, LifecycleState, LockState, ProcessDescriptor, ProcessState, ReadStatus,
    Requirement, STATE_SCHEMA, StepStatus, finalization_complete,
};
use crate::validate::validate_logs;

const SESSION_LIFECYCLE_RESULT_SCHEMA: &str = "input_dynamics_session_lifecycle_result.v1";
const RECORD_MANIFEST_SCHEMA: &str = "input_dynamics_record_manifest.v1";
const VIDEO_CAPTURE_SCHEMA: &str = "input_dynamics_video_capture.v1";
const EVIDENCE_CAPTURE_SCHEMA: &str = "input_dynamics_record_evidence_capture.v1";
const RUNTIME_DIR_NAME: &str = "input-dynamics-runtime";
const SCREENRECORD_PROCESS: &str = "screenrecord";
const GETEVENT_PROCESS: &str = "getevent";
const STOP_GRACE_TIMEOUT: Duration = Duration::from_secs(2);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HumanSessionStart {
    pub(crate) run_id: String,
    pub(crate) out: PathBuf,
    pub(crate) with_evidence: bool,
    pub(crate) full_accessibility_evidence: bool,
    pub(crate) video_enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionStatusRequest {
    pub(crate) run_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionStopRequest {
    pub(crate) run_id: Option<String>,
}

trait LifecycleEffects {
    fn package_name(&self) -> &str;
    fn adb_program(&self) -> &str;
    fn selected_device_serial(&self) -> CliResult<String>;
    fn scoped_adb_args(&self, args: &[String]) -> CliResult<Vec<String>>;
    fn broadcast(&self, action_suffix: &str, extras: Vec<String>) -> CliResult<Value>;
    fn adb_shell(&self, args: Vec<String>, failure_mode: FailureMode) -> CliResult<Value>;
    fn pull_file(&self, remote: &str, local: &Path) -> CliResult<Value>;
    fn pull_logs(&self, out: &Path) -> CliResult<Value>;
    fn capture_clock_probe(&self, phase: &str) -> CliResult<Value>;
    fn capture_evidence(
        &self,
        out: &Path,
        detail: AccessibilityDetail,
        phase: &str,
    ) -> CliResult<Value>;
    fn start_process(
        &self,
        spec: &SessionProcessSpec,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> CliResult<ProcessDescriptor>;
    fn probe_process(&self, descriptor: &ProcessDescriptor) -> ProcessLiveness;
    fn stop_process(&mut self, descriptor: &ProcessDescriptor) -> StopOutcome;
}

struct RealLifecycleEffects<'a> {
    app: &'a App,
}

pub(crate) fn start_human_session(app: &App, request: &HumanSessionStart) -> CliResult<Value> {
    let mut effects = RealLifecycleEffects { app };
    start_human_session_with_effects(&mut effects, request)
}

pub(crate) fn session_status(app: &App, request: &SessionStatusRequest) -> CliResult<Value> {
    let effects = RealLifecycleEffects { app };
    session_status_with_effects(&effects, request)
}

pub(crate) fn stop_session(app: &App, request: &SessionStopRequest) -> CliResult<Value> {
    let mut effects = RealLifecycleEffects { app };
    stop_session_with_effects(&mut effects, request)
}

pub(crate) fn diagnostic_ime_mutation_guard(
    app: &App,
    diagnostic_command: &str,
) -> CliResult<Option<Value>> {
    let device_serial = app.selected_device_serial()?;
    let runtime_paths = runtime_paths(app.package(), &device_serial);
    let current_read = read_json_classified(&runtime_paths.current, CURRENT_SCHEMA);
    let lock_read = read_json_classified(&runtime_paths.lock, LOCK_SCHEMA);
    if current_read.status == ReadStatus::Missing && lock_read.status == ReadStatus::Missing {
        return Ok(None);
    }

    let run_id = current_read
        .value
        .as_ref()
        .and_then(|value| value.get("run_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            lock_read
                .value
                .as_ref()
                .and_then(|value| value.get("run_id"))
                .and_then(Value::as_str)
        });
    let suggested = run_id.map_or_else(
        || {
            json!({
                "argv": ["input-dynamics", "session", "status"],
                "reason": "inspect the active or stale umbrella session runtime before using diagnostic IME mutation",
            })
        },
        |observed_run_id| {
            json!({
                "argv": ["input-dynamics", "session", "status", "--run-id", observed_run_id],
                "reason": "inspect the active umbrella session before using diagnostic IME mutation",
            })
        },
    );
    Ok(Some(json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": false,
        "command": diagnostic_command,
        "error_code": "umbrella_session_active",
        "message": "diagnostic IME mutation is blocked while an umbrella session runtime is active",
        "diagnostic_only": true,
        "mutated": false,
        "package_name": app.package(),
        "device_serial": device_serial,
        "run_id": run_id,
        "current_path": runtime_paths.current,
        "lock_path": runtime_paths.lock,
        "current_read": classified_json(&current_read),
        "lock_read": classified_json(&lock_read),
        "suggested_next_command": suggested,
    })))
}

fn start_human_session_with_effects(
    effects: &mut dyn LifecycleEffects,
    request: &HumanSessionStart,
) -> CliResult<Value> {
    let device_serial = effects.selected_device_serial()?;
    let runtime_paths = runtime_paths(effects.package_name(), &device_serial);
    let run_paths = RunSessionPaths::from_run_dir(&request.out);
    let identity = SessionIdentity::new(
        effects.package_name(),
        &device_serial,
        &request.run_id,
        &request.out,
        SessionIdentityPaths {
            runtime: &runtime_paths,
            run: &run_paths,
        },
    );
    let created_wall_ms = host_wall_millis()?;
    let mut lock = new_lock(&identity, created_wall_ms, LockState::Starting);
    acquire_lock_exclusive(&runtime_paths.lock, &serde_json::to_value(&lock)?)?;
    let mut state = new_state(&identity, request, created_wall_ms);

    if let Err(error) = initialize_locked_start(request, &identity, &run_paths, &state) {
        let cleanup = cleanup_failed_start(effects, &identity, &lock, &mut state, &error);
        return Err(CliError::with_details(
            format!("session start failed after lock acquisition: {error}"),
            cleanup,
        ));
    }

    let start_result =
        start_human_session_after_lock(effects, request, &identity, &mut lock, &mut state);
    match start_result {
        Ok(result) => Ok(result),
        Err(error) => {
            let cleanup = cleanup_failed_start(effects, &identity, &lock, &mut state, &error);
            Err(CliError::with_details(
                format!("session start failed after lock acquisition: {error}"),
                cleanup,
            ))
        }
    }
}

fn initialize_locked_start(
    request: &HumanSessionStart,
    identity: &SessionIdentity,
    run_paths: &RunSessionPaths,
    state: &CaptureSessionState,
) -> CliResult<()> {
    fs::create_dir_all(&run_paths.session_dir)?;
    ensure_recording_dirs(&request.out)?;
    write_state(&run_paths.state, state)?;
    write_current(
        &identity.current_path,
        identity,
        LifecycleState::Starting,
        Some(LockState::Starting),
    )
}

fn start_human_session_after_lock(
    effects: &dyn LifecycleEffects,
    request: &HumanSessionStart,
    identity: &SessionIdentity,
    lock: &mut CaptureSessionLock,
    state: &mut CaptureSessionState,
) -> CliResult<Value> {
    start_ime_phase(effects, request, identity, lock, state)?;
    start_video_phase(effects, request, identity, lock, state)?;
    start_getevent_phase(effects, identity, lock, state)?;
    start_evidence_phase(effects, request, identity, lock, state)?;
    transition_state(
        identity,
        state,
        lock,
        Transition {
            next: LifecycleState::Active,
            stage: "active",
            lock_state: Some(LockState::Active),
        },
    )?;

    Ok(json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": true,
        "command": "session start",
        "mutated": true,
        "package_name": identity.package_name,
        "device_serial": identity.device_serial,
        "run_id": identity.run_id,
        "output_dir": identity.output_dir,
        "state_path": identity.state_path,
        "lock_path": identity.lock_path,
        "current_path": identity.current_path,
        "lifecycle_state": "active",
        "processes": state.processes,
        "ime": state.ime,
        "evidence": {
            "enabled": request.with_evidence,
            "policy": if request.with_evidence { "start_end" } else { "none" },
            "start": state.start_config.get("evidence_start").cloned().unwrap_or(Value::Null),
        },
    }))
}

fn start_ime_phase(
    effects: &dyn LifecycleEffects,
    request: &HumanSessionStart,
    identity: &SessionIdentity,
    lock: &mut CaptureSessionLock,
    state: &mut CaptureSessionState,
) -> CliResult<()> {
    let pre_stop = effects.broadcast("STOP", Vec::new())?;
    let clear_logs = effects.broadcast("CLEAR_LOGS", Vec::new())?;
    ensure_result_ok(&clear_logs, "clear logs before session start")?;
    let ime_start = start_ime_logging(effects, request)?;
    ensure_result_ok(&ime_start, "start IME logging")?;
    state.ime = json!({
        "pre_stop": pre_stop,
        "clear_logs": clear_logs,
        "start": ime_start,
    });
    transition_state(
        identity,
        state,
        lock,
        Transition {
            next: LifecycleState::ImeStarted,
            stage: "ime_started",
            lock_state: Some(LockState::Starting),
        },
    )
}

fn start_video_phase(
    effects: &dyn LifecycleEffects,
    request: &HumanSessionStart,
    identity: &SessionIdentity,
    lock: &mut CaptureSessionLock,
    state: &mut CaptureSessionState,
) -> CliResult<()> {
    if !request.video_enabled {
        return Ok(());
    }
    let before = effects.capture_clock_probe("before_screenrecord_start")?;
    let spec = screenrecord_spec(effects, identity)?;
    let descriptor = start_owned_process(effects, identity, state, SCREENRECORD_PROCESS, &spec)?;
    let after = effects.capture_clock_probe("after_screenrecord_start")?;
    validate_probe_order(&before, &after)?;
    state
        .processes
        .insert(String::from(SCREENRECORD_PROCESS), descriptor);
    set_start_config_value(
        state,
        "video_start_timing",
        json!({
            "ok": true,
            "before": before,
            "after": after,
        }),
    )?;
    transition_state(
        identity,
        state,
        lock,
        Transition {
            next: LifecycleState::VideoStarted,
            stage: "video_started",
            lock_state: Some(LockState::Starting),
        },
    )
}

fn start_getevent_phase(
    effects: &dyn LifecycleEffects,
    identity: &SessionIdentity,
    lock: &mut CaptureSessionLock,
    state: &mut CaptureSessionState,
) -> CliResult<()> {
    let spec = getevent_spec(effects, identity)?;
    let descriptor = start_owned_process(effects, identity, state, GETEVENT_PROCESS, &spec)?;
    state
        .processes
        .insert(String::from(GETEVENT_PROCESS), descriptor);
    transition_state(
        identity,
        state,
        lock,
        Transition {
            next: LifecycleState::GeteventStarted,
            stage: "getevent_started",
            lock_state: Some(LockState::Starting),
        },
    )
}

fn start_evidence_phase(
    effects: &dyn LifecycleEffects,
    request: &HumanSessionStart,
    identity: &SessionIdentity,
    lock: &mut CaptureSessionLock,
    state: &mut CaptureSessionState,
) -> CliResult<()> {
    let evidence_start = capture_evidence(effects, request, identity, "start")?;
    set_start_config_value(state, "evidence_start", evidence_start)?;
    if !request.with_evidence {
        return Ok(());
    }
    mark_directory_artifact(
        state,
        "evidence_start",
        &identity.output_dir.join("evidence").join("start"),
        EVIDENCE_CAPTURE_SCHEMA,
    );
    transition_state(
        identity,
        state,
        lock,
        Transition {
            next: LifecycleState::StartEvidenceCaptured,
            stage: "start_evidence_captured",
            lock_state: Some(LockState::Starting),
        },
    )
}

fn session_status_with_effects(
    effects: &dyn LifecycleEffects,
    request: &SessionStatusRequest,
) -> CliResult<Value> {
    let device_serial = effects.selected_device_serial()?;
    let runtime_paths = runtime_paths(effects.package_name(), &device_serial);
    let current_read = read_json_classified(&runtime_paths.current, CURRENT_SCHEMA);
    let runtime_lock_read = read_json_classified(&runtime_paths.lock, LOCK_SCHEMA);
    let Some(current) = current_read.value.as_ref() else {
        if current_read.status != ReadStatus::Missing
            || runtime_lock_read.status != ReadStatus::Missing
        {
            return Ok(runtime_repair_required_result(&RuntimeRepairRequired {
                command: "session status",
                reason_code: "runtime_incomplete",
                package_name: effects.package_name(),
                device_serial: &device_serial,
                runtime_paths: &runtime_paths,
                current_read: &current_read,
                lock_read: &runtime_lock_read,
            }));
        }
        return Ok(no_active_session_status(
            effects.package_name(),
            &device_serial,
            &runtime_paths,
            "session status",
        ));
    };
    if current_read.status != ReadStatus::Valid {
        return Ok(runtime_repair_required_result(&RuntimeRepairRequired {
            command: "session status",
            reason_code: "current_invalid",
            package_name: effects.package_name(),
            device_serial: &device_serial,
            runtime_paths: &runtime_paths,
            current_read: &current_read,
            lock_read: &runtime_lock_read,
        }));
    }
    if let Some(selector) = request.run_id.as_deref() {
        let observed = current.get("run_id").and_then(Value::as_str);
        if observed != Some(selector) {
            return Ok(selector_mismatch_result(
                "session status",
                selector,
                observed,
            ));
        }
    }
    let loaded = load_status_runtime(effects, &device_serial, &runtime_paths, current)?;

    Ok(json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": loaded.state_read.status == ReadStatus::Valid
            && loaded.lock_read.status == ReadStatus::Valid
            && loaded.identity_mismatches.is_empty(),
        "command": "session status",
        "mutated": false,
        "package_name": effects.package_name(),
        "device_serial": device_serial,
        "run_id": current.get("run_id").cloned().unwrap_or(Value::Null),
        "output_dir": current.get("output_dir").cloned().unwrap_or(Value::Null),
        "state_path": loaded.state_path.to_string_lossy(),
        "lock_path": loaded.lock_path.to_string_lossy(),
        "current_path": runtime_paths.current.to_string_lossy(),
        "lifecycle_state": current.get("observed_lifecycle_state").cloned().unwrap_or(Value::Null),
        "lock_state": current.get("observed_lock_state").cloned().unwrap_or(Value::Null),
        "current_read": classified_json(&current_read),
        "state_read": classified_json(&loaded.state_read),
        "lock_read": classified_json(&loaded.lock_read),
        "identity_mismatches": loaded.identity_mismatches,
        "process_liveness": loaded.process_liveness,
    }))
}

struct LoadedStatusRuntime {
    state_path: PathBuf,
    lock_path: PathBuf,
    state_read: crate::session_state::io::ClassifiedJson,
    lock_read: crate::session_state::io::ClassifiedJson,
    identity_mismatches: Vec<String>,
    process_liveness: BTreeMap<String, Value>,
}

fn load_status_runtime(
    effects: &dyn LifecycleEffects,
    device_serial: &str,
    runtime_paths: &RuntimeSessionPaths,
    current: &Value,
) -> CliResult<LoadedStatusRuntime> {
    let state_path = required_path(current, "state_path")?;
    let lock_path = required_path(current, "lock_path")?;
    let state_read = read_json_classified(&state_path, STATE_SCHEMA);
    let lock_read = read_json_classified(&lock_path, LOCK_SCHEMA);
    let state_option = classified_state(&state_read)?;
    let lock_option = classified_lock(&lock_read)?;
    let identity_mismatches = match (state_option.as_ref(), lock_option.as_ref()) {
        (Some(loaded_state), Some(loaded_lock)) => {
            runtime_identity_mismatches(&RuntimeIdentityView {
                package_name: effects.package_name(),
                device_serial,
                current,
                state: loaded_state,
                lock: loaded_lock,
                state_path: &state_path,
                lock_path: &lock_path,
                current_path: &runtime_paths.current,
            })
        }
        (None, _) | (_, None) => Vec::new(),
    };
    let process_liveness = status_process_liveness(effects, state_option.as_ref());
    Ok(LoadedStatusRuntime {
        state_path,
        lock_path,
        state_read,
        lock_read,
        identity_mismatches,
        process_liveness,
    })
}

fn classified_state(
    classified: &crate::session_state::io::ClassifiedJson,
) -> CliResult<Option<CaptureSessionState>> {
    if classified.status != ReadStatus::Valid {
        return Ok(None);
    }
    classified
        .value
        .as_ref()
        .map(|value| {
            serde_json::from_value::<CaptureSessionState>(value.clone()).map_err(Into::into)
        })
        .transpose()
}

fn classified_lock(
    classified: &crate::session_state::io::ClassifiedJson,
) -> CliResult<Option<CaptureSessionLock>> {
    if classified.status != ReadStatus::Valid {
        return Ok(None);
    }
    classified
        .value
        .as_ref()
        .map(|value| {
            serde_json::from_value::<CaptureSessionLock>(value.clone()).map_err(Into::into)
        })
        .transpose()
}

fn status_process_liveness(
    effects: &dyn LifecycleEffects,
    state_option: Option<&CaptureSessionState>,
) -> BTreeMap<String, Value> {
    let mut process_liveness = BTreeMap::new();
    let Some(loaded_state) = state_option else {
        return process_liveness;
    };
    for (name, descriptor) in &loaded_state.processes {
        let liveness = effects.probe_process(descriptor);
        process_liveness.insert(name.clone(), process_liveness_json(&liveness));
    }
    process_liveness
}

fn stop_session_with_effects(
    effects: &mut dyn LifecycleEffects,
    request: &SessionStopRequest,
) -> CliResult<Value> {
    if request.run_id.is_none() {
        return safe_stop_without_run_id(effects);
    }
    let Some(mut active) = resolve_active_stop_session(effects, request)? else {
        return no_active_stop_result(effects);
    };
    claim_stop_ownership(&active.identity, &mut active.lock, &mut active.state)?;
    let mut ledger = new_finalization_ledger(&active.identity)?;
    let mut outcomes: BTreeMap<String, Value> = BTreeMap::new();
    stop_capture_side_effects(effects, &mut active, &mut ledger, &mut outcomes)?;
    let finalization =
        finalize_artifacts(effects, &active.identity, &mut active.state, &mut ledger);
    outcomes.insert(String::from("artifacts"), finalization);
    finish_stop_session(active, ledger, outcomes)
}

fn safe_stop_without_run_id(effects: &dyn LifecycleEffects) -> CliResult<Value> {
    let device_serial = effects.selected_device_serial()?;
    let runtime_paths = runtime_paths(effects.package_name(), &device_serial);
    let current_read = read_json_classified(&runtime_paths.current, CURRENT_SCHEMA);
    let Some(current) = current_read.value.as_ref() else {
        return Ok(no_active_session_status(
            effects.package_name(),
            &device_serial,
            &runtime_paths,
            "session stop",
        ));
    };
    if current_read.status != ReadStatus::Valid {
        return Ok(invalid_runtime_json_result(
            "session stop",
            "current_invalid",
            &current_read,
        ));
    }
    Ok(stop_requires_run_id_result(
        current.get("run_id").and_then(Value::as_str),
        current,
    ))
}

struct ActiveStopSession {
    identity: SessionIdentity,
    state: CaptureSessionState,
    lock: CaptureSessionLock,
}

fn resolve_active_stop_session(
    effects: &dyn LifecycleEffects,
    request: &SessionStopRequest,
) -> CliResult<Option<ActiveStopSession>> {
    let device_serial = effects.selected_device_serial()?;
    let runtime_paths = runtime_paths(effects.package_name(), &device_serial);
    let current_read = read_json_classified(&runtime_paths.current, CURRENT_SCHEMA);
    let Some(current) = current_read.value.as_ref() else {
        return Ok(None);
    };
    if current_read.status != ReadStatus::Valid {
        return Err(CliError::with_details(
            "active session current is invalid",
            invalid_runtime_json_result("session stop", "current_invalid", &current_read),
        ));
    }
    let observed_run_id = current.get("run_id").and_then(Value::as_str);
    let Some(run_id) = request.run_id.as_deref() else {
        return Err(CliError::with_details(
            "session stop requires --run-id before it mutates",
            stop_requires_run_id_result(observed_run_id, current),
        ));
    };
    if observed_run_id != Some(run_id) {
        return Err(CliError::with_details(
            "requested run id does not match the active session",
            selector_mismatch_result("session stop", run_id, observed_run_id),
        ));
    }
    let state_path = required_path(current, "state_path")?;
    let lock_path = required_path(current, "lock_path")?;
    let state = read_state(&state_path)?;
    let lock = read_lock(&lock_path)?;
    let output_dir = required_path(current, "output_dir")?;
    let identity = SessionIdentity {
        package_name: effects.package_name().to_owned(),
        device_serial: device_serial.clone(),
        run_id: run_id.to_owned(),
        finalization_path: output_dir.join("session").join("finalization.json"),
        lock_snapshot_path: output_dir.join("session").join("lock.snapshot.json"),
        output_dir,
        state_path,
        lock_path,
        current_path: runtime_paths.current,
    };
    validate_runtime_identity(&RuntimeIdentityView {
        package_name: effects.package_name(),
        device_serial: &device_serial,
        current,
        state: &state,
        lock: &lock,
        state_path: &identity.state_path,
        lock_path: &identity.lock_path,
        current_path: &identity.current_path,
    })?;
    Ok(Some(ActiveStopSession {
        identity,
        state,
        lock,
    }))
}

fn no_active_stop_result(effects: &dyn LifecycleEffects) -> CliResult<Value> {
    let device_serial = effects.selected_device_serial()?;
    let runtime_paths = runtime_paths(effects.package_name(), &device_serial);
    Ok(no_active_session_status(
        effects.package_name(),
        &device_serial,
        &runtime_paths,
        "session stop",
    ))
}

fn claim_stop_ownership(
    identity: &SessionIdentity,
    lock: &mut CaptureSessionLock,
    state: &mut CaptureSessionState,
) -> CliResult<()> {
    if state.lifecycle.state != LifecycleState::Active {
        return Err(CliError::with_details(
            "session stop can only finalize an active session",
            json!({
                "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
                "ok": false,
                "command": "session stop",
                "error_code": "session_not_active",
                "mutated": false,
                "observed_lifecycle_state": state.lifecycle.state,
                "run_id": identity.run_id,
            }),
        ));
    }
    claim_finalization_owner(identity, lock)?;
    transition_state(
        identity,
        state,
        lock,
        Transition {
            next: LifecycleState::StopRequested,
            stage: "stop_requested",
            lock_state: Some(LockState::StopRequested),
        },
    )?;
    mark_processes_stop_requested(identity, state)?;
    transition_state(
        identity,
        state,
        lock,
        Transition {
            next: LifecycleState::Stopping,
            stage: "stopping",
            lock_state: Some(LockState::Stopping),
        },
    )
}

fn stop_capture_side_effects(
    effects: &mut dyn LifecycleEffects,
    active: &mut ActiveStopSession,
    ledger: &mut FinalizationLedger,
    outcomes: &mut BTreeMap<String, Value>,
) -> CliResult<()> {
    stop_process_step(effects, active, ledger, outcomes, GETEVENT_PROCESS);
    stop_process_step(effects, active, ledger, outcomes, SCREENRECORD_PROCESS);
    let ime_stop = effects.broadcast("STOP", Vec::new());
    record_step(ledger, "stop_ime", Requirement::Required, &ime_stop);
    outcomes.insert(String::from("stop_ime"), result_value(&ime_stop));
    let end_evidence = stop_end_evidence(effects, &active.identity, &active.state);
    if end_evidence
        .as_ref()
        .ok()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        mark_directory_artifact(
            &mut active.state,
            "evidence_end",
            &active.identity.output_dir.join("evidence").join("end"),
            EVIDENCE_CAPTURE_SCHEMA,
        );
    }
    record_step(
        ledger,
        "capture_end_evidence",
        evidence_requirement(&active.state),
        &end_evidence,
    );
    outcomes.insert(
        String::from("capture_end_evidence"),
        result_value(&end_evidence),
    );
    transition_state(
        &active.identity,
        &mut active.state,
        &mut active.lock,
        Transition {
            next: LifecycleState::EndEvidenceCapturing,
            stage: "end_evidence_capturing",
            lock_state: Some(LockState::Stopping),
        },
    )?;
    transition_state(
        &active.identity,
        &mut active.state,
        &mut active.lock,
        Transition {
            next: LifecycleState::Finalizing,
            stage: "finalizing",
            lock_state: Some(LockState::Finalizing),
        },
    )
}

fn stop_process_step(
    effects: &mut dyn LifecycleEffects,
    active: &mut ActiveStopSession,
    ledger: &mut FinalizationLedger,
    outcomes: &mut BTreeMap<String, Value>,
    process_name: &str,
) {
    let timing_before = (process_name == SCREENRECORD_PROCESS
        && active.state.processes.contains_key(SCREENRECORD_PROCESS))
    .then(|| effects.capture_clock_probe("before_screenrecord_stop"));
    let stop = stop_named_process(effects, &active.identity, &mut active.state, process_name);
    if process_name == SCREENRECORD_PROCESS
        && active.state.processes.contains_key(SCREENRECORD_PROCESS)
    {
        let timing = capture_video_stop_timing(effects, timing_before, &stop);
        if let Ok(value) = timing.as_ref() {
            let _set =
                set_start_config_value(&mut active.state, "video_stop_timing", value.clone());
            let _write = write_state(&active.identity.state_path, &active.state);
        }
        outcomes.insert(
            String::from("screenrecord_stop_timing"),
            result_value(&timing),
        );
    }
    record_step(
        ledger,
        stop_step_name(process_name),
        process_requirement(&active.state, process_name),
        &stop,
    );
    outcomes.insert(
        String::from(stop_step_name(process_name)),
        result_value(&stop),
    );
}

fn capture_video_stop_timing(
    effects: &dyn LifecycleEffects,
    before_result: Option<CliResult<Value>>,
    stop: &CliResult<Value>,
) -> CliResult<Value> {
    let before_marker = before_result
        .ok_or_else(|| CliError::new("screenrecord stop timing was not requested"))??;
    let after = effects.capture_clock_probe("after_screenrecord_stop")?;
    validate_probe_order(&before_marker, &after)?;
    Ok(json!({
        "ok": true,
        "before": before_marker,
        "after": after,
        "stop": result_value(stop),
    }))
}

fn finish_stop_session(
    mut active: ActiveStopSession,
    mut ledger: FinalizationLedger,
    mut outcomes: BTreeMap<String, Value>,
) -> CliResult<Value> {
    let runtime_cleanup = clear_runtime_files(&active.identity);
    ledger.cleanup_ok = runtime_cleanup.get("ok").and_then(Value::as_bool) == Some(true);
    record_step_value(
        &mut ledger,
        "clear_runtime",
        Requirement::Required,
        &runtime_cleanup,
    );
    outcomes.insert(String::from("clear_runtime"), runtime_cleanup);
    let complete = finalization_complete(&ledger.steps, &active.state.artifacts);
    let terminal = if complete {
        LifecycleState::Complete
    } else {
        LifecycleState::Incomplete
    };
    active.state.lifecycle.state = terminal;
    active.state.lifecycle.stage = lifecycle_stage(terminal);
    active.state.lifecycle.history.push(history_event(
        terminal,
        "terminal",
        host_wall_millis().unwrap_or(0_u64),
    ));
    active.state.transition_seq = active.state.transition_seq.saturating_add(1_u64);
    active.state.updated_wall_ms = host_wall_millis()?;
    active.state.finalization = Some(serde_json::to_value(&ledger)?);
    write_state(&active.identity.state_path, &active.state)?;
    ledger.run_state = terminal;
    ledger.finished_wall_ms = Some(host_wall_millis()?);
    write_json_atomic(
        &active.identity.finalization_path,
        &serde_json::to_value(&ledger)?,
    )?;
    write_json_atomic(
        &active.identity.lock_snapshot_path,
        &serde_json::to_value(&active.lock)?,
    )?;
    let completion = if complete {
        StopCompletion::Complete
    } else {
        StopCompletion::Incomplete
    };
    stop_result_json(&StopResultJson {
        identity: &active.identity,
        state: &active.state,
        ledger: &ledger,
        outcomes,
        completion,
    })
}

fn clear_runtime_files(identity: &SessionIdentity) -> Value {
    let current = remove_runtime_file(&identity.current_path);
    let lock = remove_runtime_file(&identity.lock_path);
    let ok = current.get("ok").and_then(Value::as_bool) == Some(true)
        && lock.get("ok").and_then(Value::as_bool) == Some(true);
    json!({
        "ok": ok,
        "current": current,
        "lock": lock,
    })
}

fn remove_runtime_file(path: &Path) -> Value {
    match fs::remove_file(path) {
        Ok(()) => json!({
            "ok": true,
            "path": path,
            "removed": true,
            "missing": false,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({
            "ok": true,
            "path": path,
            "removed": false,
            "missing": true,
        }),
        Err(error) => json!({
            "ok": false,
            "path": path,
            "removed": false,
            "missing": false,
            "error": error.to_string(),
        }),
    }
}

struct StopResultJson<'a> {
    identity: &'a SessionIdentity,
    state: &'a CaptureSessionState,
    ledger: &'a FinalizationLedger,
    outcomes: BTreeMap<String, Value>,
    completion: StopCompletion,
}

#[derive(Clone, Copy)]
enum StopCompletion {
    Complete,
    Incomplete,
}

impl StopCompletion {
    const fn ok(self) -> bool {
        matches!(self, Self::Complete)
    }

    const fn lifecycle(self) -> LifecycleState {
        match self {
            Self::Complete => LifecycleState::Complete,
            Self::Incomplete => LifecycleState::Incomplete,
        }
    }
}

fn stop_result_json(result: &StopResultJson<'_>) -> CliResult<Value> {
    Ok(json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": result.completion.ok(),
        "command": "session stop",
        "mutated": true,
        "package_name": result.identity.package_name,
        "device_serial": result.identity.device_serial,
        "run_id": result.identity.run_id,
        "output_dir": result.identity.output_dir,
        "state_path": result.identity.state_path,
        "finalization_path": result.identity.finalization_path,
        "lock_snapshot_path": result.identity.lock_snapshot_path,
        "lifecycle_state": serde_json::to_value(result.completion.lifecycle())?,
        "outcomes": result.outcomes,
        "artifacts": result.state.artifacts,
        "finalization": result.ledger,
    }))
}

fn stop_step_name(process_name: &str) -> &'static str {
    if process_name == GETEVENT_PROCESS {
        "stop_getevent"
    } else {
        "stop_screenrecord"
    }
}

fn process_requirement(state: &CaptureSessionState, process_name: &str) -> Requirement {
    if state.processes.contains_key(process_name) {
        Requirement::Required
    } else {
        Requirement::Optional
    }
}

fn evidence_requirement(state: &CaptureSessionState) -> Requirement {
    if state
        .start_config
        .get("with_evidence")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        Requirement::Required
    } else {
        Requirement::Optional
    }
}

impl LifecycleEffects for RealLifecycleEffects<'_> {
    fn package_name(&self) -> &str {
        self.app.package()
    }

    fn adb_program(&self) -> &str {
        self.app.adb_program()
    }

    fn selected_device_serial(&self) -> CliResult<String> {
        self.app.selected_device_serial()
    }

    fn scoped_adb_args(&self, args: &[String]) -> CliResult<Vec<String>> {
        self.app.scoped_adb_args(args)
    }

    fn broadcast(&self, action_suffix: &str, extras: Vec<String>) -> CliResult<Value> {
        self.app.broadcast(action_suffix, extras)
    }

    fn adb_shell(&self, args: Vec<String>, failure_mode: FailureMode) -> CliResult<Value> {
        let output = self.app.adb_shell(args, failure_mode)?;
        Ok(json!({
            "ok": output.status_code == Some(0_i32),
            "process": output.json(),
        }))
    }

    fn pull_file(&self, remote: &str, local: &Path) -> CliResult<Value> {
        let output = self.app.adb(
            &[
                String::from("pull"),
                String::from(remote),
                path_string(local)?,
            ],
            FailureMode::AllowFailure,
        )?;
        Ok(json!({
            "ok": output.status_code == Some(0_i32),
            "remote_path": remote,
            "local_path": path_string(local)?,
            "process": output.json(),
        }))
    }

    fn pull_logs(&self, out: &Path) -> CliResult<Value> {
        pull_logs(self.app, out)
    }

    fn capture_clock_probe(&self, phase: &str) -> CliResult<Value> {
        capture_device_clock_probe(self.app, phase)
    }

    fn capture_evidence(
        &self,
        out: &Path,
        detail: AccessibilityDetail,
        phase: &str,
    ) -> CliResult<Value> {
        let bundle = observe::all(self.app, out, detail)?;
        Ok(json!({
            "schema": EVIDENCE_CAPTURE_SCHEMA,
            "enabled": true,
            "requested": true,
            "phase": phase,
            "policy": "start_end",
            "bundle": bundle,
        }))
    }

    fn start_process(
        &self,
        spec: &SessionProcessSpec,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> CliResult<ProcessDescriptor> {
        let started = start_session_process(spec, stdout_path, stderr_path)?;
        Ok(started.descriptor().clone())
    }

    fn probe_process(&self, descriptor: &ProcessDescriptor) -> ProcessLiveness {
        probe_descriptor(descriptor, &HostProcessProbe)
    }

    fn stop_process(&mut self, descriptor: &ProcessDescriptor) -> StopOutcome {
        let mut signaler = HostProcessSignaler;
        stop_process_group(
            descriptor,
            &StopPolicy {
                grace_timeout: STOP_GRACE_TIMEOUT,
                poll_interval: STOP_POLL_INTERVAL,
            },
            &HostProcessProbe,
            &mut signaler,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionIdentity {
    package_name: String,
    device_serial: String,
    run_id: String,
    output_dir: PathBuf,
    state_path: PathBuf,
    finalization_path: PathBuf,
    lock_snapshot_path: PathBuf,
    lock_path: PathBuf,
    current_path: PathBuf,
}

#[derive(Clone, Copy)]
struct SessionIdentityPaths<'a> {
    runtime: &'a RuntimeSessionPaths,
    run: &'a RunSessionPaths,
}

impl SessionIdentity {
    fn new(
        package_name: &str,
        device_serial: &str,
        run_id: &str,
        output_dir: &Path,
        paths: SessionIdentityPaths<'_>,
    ) -> Self {
        Self {
            package_name: String::from(package_name),
            device_serial: String::from(device_serial),
            run_id: String::from(run_id),
            output_dir: output_dir.to_path_buf(),
            state_path: paths.run.state.clone(),
            finalization_path: paths.run.finalization.clone(),
            lock_snapshot_path: paths.run.lock_snapshot.clone(),
            lock_path: paths.runtime.lock.clone(),
            current_path: paths.runtime.current.clone(),
        }
    }
}

fn runtime_paths(package_name: &str, device_serial: &str) -> RuntimeSessionPaths {
    RuntimeSessionPaths::from_base_dir(
        &std::env::temp_dir().join(RUNTIME_DIR_NAME),
        package_name,
        device_serial,
    )
}

fn new_lock(identity: &SessionIdentity, wall_ms: u64, lock_state: LockState) -> CaptureSessionLock {
    CaptureSessionLock {
        schema: String::from(LOCK_SCHEMA),
        lock_state,
        observed_lifecycle_state: LifecycleState::Starting,
        mutation_seq: 0_u64,
        package_name: identity.package_name.clone(),
        device_serial: identity.device_serial.clone(),
        run_id: identity.run_id.clone(),
        command: CaptureSessionCommand {
            name: CaptureSessionCommandName::SessionStart,
            bounded: false,
        },
        output_dir: identity.output_dir.to_string_lossy().to_string(),
        state_path: identity.state_path.to_string_lossy().to_string(),
        owner_pid: process::id(),
        owner_host: host_name(),
        invocation_id: invocation_id(wall_ms),
        created_wall_ms: wall_ms,
        updated_wall_ms: wall_ms,
        cli_version: String::from(env!("CARGO_PKG_VERSION")),
        finalization_owner: None,
    }
}

fn new_state(
    identity: &SessionIdentity,
    request: &HumanSessionStart,
    wall_ms: u64,
) -> CaptureSessionState {
    CaptureSessionState {
        schema: String::from(STATE_SCHEMA),
        run_id: identity.run_id.clone(),
        run_root: identity.output_dir.to_string_lossy().to_string(),
        package_name: identity.package_name.clone(),
        device_serial: identity.device_serial.clone(),
        cli_version: String::from(env!("CARGO_PKG_VERSION")),
        transition_seq: 0_u64,
        created_wall_ms: wall_ms,
        updated_wall_ms: wall_ms,
        lifecycle: LifecycleSnapshot {
            state: LifecycleState::Starting,
            stage: String::from("starting"),
            history: vec![history_event(LifecycleState::Starting, "starting", wall_ms)],
        },
        start_config: json!({
            "run_id": request.run_id,
            "out": request.out,
            "input_actor": "human",
            "with_evidence": request.with_evidence,
            "full_accessibility_evidence": request.full_accessibility_evidence,
            "video_enabled": request.video_enabled,
        }),
        input: InputProvenance::human(),
        artifacts: initial_artifacts(request),
        processes: BTreeMap::new(),
        ime: Value::Null,
        controller: None,
        finalization: None,
    }
}

fn initial_artifacts(request: &HumanSessionStart) -> BTreeMap<String, ArtifactStatus> {
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        String::from("manifest"),
        ArtifactStatus::new("manifest.json", Requirement::Required, "write_manifest"),
    );
    artifacts.insert(
        String::from("validation"),
        ArtifactStatus::new(
            "validation.json",
            Requirement::Required,
            "validate_ime_logs",
        ),
    );
    artifacts.insert(
        String::from("adb_getevent_raw"),
        ArtifactStatus::new(
            "adb/getevent.raw.log",
            Requirement::Required,
            "stop_getevent",
        ),
    );
    artifacts.insert(
        String::from("adb_getevent_jsonl"),
        ArtifactStatus::new(
            "adb/getevent.jsonl",
            Requirement::Required,
            "normalize_getevent",
        ),
    );
    if request.video_enabled {
        artifacts.insert(
            String::from("video_screen"),
            ArtifactStatus::new("video/screen.mp4", Requirement::Required, "pull_video"),
        );
        artifacts.insert(
            String::from("video_timing"),
            ArtifactStatus::new(
                "video/timing.json",
                Requirement::Required,
                "write_video_timing",
            ),
        );
    }
    if request.with_evidence {
        artifacts.insert(
            String::from("evidence_start"),
            ArtifactStatus::new(
                "evidence/start",
                Requirement::Required,
                "capture_start_evidence",
            ),
        );
        artifacts.insert(
            String::from("evidence_end"),
            ArtifactStatus::new(
                "evidence/end",
                Requirement::Required,
                "capture_end_evidence",
            ),
        );
    }
    artifacts
}

fn ensure_recording_dirs(out: &Path) -> CliResult<()> {
    for child in ["ime", "adb", "derived", "video", "evidence"] {
        fs::create_dir_all(out.join(child))?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Transition {
    next: LifecycleState,
    stage: &'static str,
    lock_state: Option<LockState>,
}

fn transition_state(
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
    lock: &mut CaptureSessionLock,
    transition: Transition,
) -> CliResult<()> {
    let wall_ms = host_wall_millis()?;
    if let Some(next_lock_state) = transition.lock_state {
        let expected = lock.mutation_seq;
        lock.lock_state = next_lock_state;
        lock.observed_lifecycle_state = transition.next;
        lock.mutation_seq = lock.mutation_seq.saturating_add(1_u64);
        lock.updated_wall_ms = wall_ms;
        checked_update_json(
            &identity.lock_path,
            LOCK_SCHEMA,
            "mutation_seq",
            expected,
            &serde_json::to_value(&lock)?,
        )?;
    }

    state.lifecycle.state = transition.next;
    state.lifecycle.stage = String::from(transition.stage);
    state
        .lifecycle
        .history
        .push(history_event(transition.next, transition.stage, wall_ms));
    state.transition_seq = state.transition_seq.saturating_add(1_u64);
    state.updated_wall_ms = wall_ms;
    write_state(&identity.state_path, state)?;

    if let Some(next_lock_state) = transition.lock_state {
        write_current(
            &identity.current_path,
            identity,
            transition.next,
            Some(next_lock_state),
        )?;
    }
    Ok(())
}

fn write_state(path: &Path, state: &CaptureSessionState) -> CliResult<()> {
    write_json_atomic(path, &serde_json::to_value(state)?)
}

fn write_current(
    path: &Path,
    identity: &SessionIdentity,
    lifecycle: LifecycleState,
    lock_state: Option<LockState>,
) -> CliResult<()> {
    let current = CaptureSessionCurrent {
        schema: String::from(CURRENT_SCHEMA),
        package_name: identity.package_name.clone(),
        device_serial: identity.device_serial.clone(),
        run_id: identity.run_id.clone(),
        output_dir: identity.output_dir.to_string_lossy().to_string(),
        state_path: identity.state_path.to_string_lossy().to_string(),
        lock_path: identity.lock_path.to_string_lossy().to_string(),
        observed_lifecycle_state: lifecycle,
        observed_lock_state: lock_state,
        updated_wall_ms: host_wall_millis()?,
    };
    write_json_atomic(path, &serde_json::to_value(current)?)
}

fn set_start_config_value(
    state: &mut CaptureSessionState,
    key: &str,
    value: Value,
) -> CliResult<()> {
    let Some(object) = state.start_config.as_object_mut() else {
        return Err(CliError::new("session start_config is not a JSON object"));
    };
    object.insert(String::from(key), value);
    Ok(())
}

fn start_ime_logging(
    effects: &dyn LifecycleEffects,
    request: &HumanSessionStart,
) -> CliResult<Value> {
    let enable = effects.broadcast("ENABLE", Vec::new())?;
    ensure_result_ok(&enable, "enable IME logging")?;
    let extras = vec![
        String::from("--es"),
        String::from("run_id"),
        request.run_id.clone(),
        String::from("--es"),
        String::from("input_actor"),
        String::from("human"),
        String::from("--es"),
        String::from("input_cadence_policy"),
        String::from("manual"),
    ];
    let start = effects.broadcast("START", extras)?;
    ensure_result_ok(&start, "start IME logging session")?;
    Ok(json!({
        "ok": true,
        "enable": enable,
        "start": start,
    }))
}

fn start_owned_process(
    effects: &dyn LifecycleEffects,
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
    name: &str,
    spec: &SessionProcessSpec,
) -> CliResult<ProcessDescriptor> {
    let pre_spawn = pre_spawn_descriptor(spec);
    state.processes.insert(String::from(name), pre_spawn);
    state.transition_seq = state.transition_seq.saturating_add(1_u64);
    state.updated_wall_ms = host_wall_millis()?;
    write_state(&identity.state_path, state)?;
    let stdout = identity.output_dir.join(&spec.stdout);
    let stderr = identity.output_dir.join(&spec.stderr);
    effects.start_process(spec, &stdout, &stderr)
}

fn screenrecord_spec(
    effects: &dyn LifecycleEffects,
    identity: &SessionIdentity,
) -> CliResult<SessionProcessSpec> {
    let remote_path = remote_video_path(&identity.run_id);
    let _cleanup = effects.adb_shell(
        vec![String::from("rm"), String::from("-f"), remote_path.clone()],
        FailureMode::AllowFailure,
    );
    let args = vec![
        String::from("shell"),
        String::from("screenrecord"),
        remote_path.clone(),
    ];
    Ok(SessionProcessSpec {
        name: String::from(SCREENRECORD_PROCESS),
        kind: HostProcessKind::AdbShell,
        required: true,
        program: String::from(effects.adb_program()),
        args: effects.scoped_adb_args(&args)?,
        remote_command: vec![String::from("screenrecord"), remote_path],
        stdout: String::from("video/screenrecord.stdout.log"),
        stderr: String::from("video/screenrecord.stderr.log"),
        stop_method: StopMethod::ProcessGroupInterruptThenKill,
        expected_exit: false,
    })
}

fn getevent_spec(
    effects: &dyn LifecycleEffects,
    _identity: &SessionIdentity,
) -> CliResult<SessionProcessSpec> {
    let args = vec![
        String::from("shell"),
        String::from("getevent"),
        String::from("-lt"),
    ];
    Ok(SessionProcessSpec {
        name: String::from(GETEVENT_PROCESS),
        kind: HostProcessKind::AdbShell,
        required: true,
        program: String::from(effects.adb_program()),
        args: effects.scoped_adb_args(&args)?,
        remote_command: vec![String::from("getevent"), String::from("-lt")],
        stdout: String::from("adb/getevent.raw.log"),
        stderr: String::from("adb/getevent.stderr.log"),
        stop_method: StopMethod::ProcessGroupTerminateThenKill,
        expected_exit: false,
    })
}

fn capture_evidence(
    effects: &dyn LifecycleEffects,
    request: &HumanSessionStart,
    identity: &SessionIdentity,
    phase: &str,
) -> CliResult<Value> {
    if !request.with_evidence {
        return Ok(json!({
            "schema": EVIDENCE_CAPTURE_SCHEMA,
            "enabled": false,
            "requested": false,
            "phase": phase,
        }));
    }
    let detail = if request.full_accessibility_evidence {
        AccessibilityDetail::Full
    } else {
        AccessibilityDetail::Compressed
    };
    effects.capture_evidence(
        &identity.output_dir.join("evidence").join(phase),
        detail,
        phase,
    )
}

fn cleanup_failed_start(
    effects: &mut dyn LifecycleEffects,
    identity: &SessionIdentity,
    lock: &CaptureSessionLock,
    state: &mut CaptureSessionState,
    error: &CliError,
) -> Value {
    let mut cleanup = BTreeMap::new();
    for name in [GETEVENT_PROCESS, SCREENRECORD_PROCESS] {
        if let Some(descriptor) = state.processes.get(name).cloned() {
            let outcome = effects.stop_process(&descriptor);
            cleanup.insert(String::from(name), stop_outcome_json(&outcome));
        }
    }
    let ime_stop = effects
        .broadcast("STOP", Vec::new())
        .unwrap_or_else(|stop_error| json!({"ok": false, "error": stop_error.to_string()}));
    let _snapshot = write_json_atomic(
        &identity.lock_snapshot_path,
        &serde_json::to_value(lock).unwrap_or(Value::Null),
    );
    state.lifecycle.state = LifecycleState::Incomplete;
    state.lifecycle.stage = String::from("start_failed");
    state.lifecycle.history.push(history_event(
        LifecycleState::Incomplete,
        "start_failed",
        host_wall_millis().unwrap_or(0_u64),
    ));
    state.transition_seq = state.transition_seq.saturating_add(1_u64);
    state.updated_wall_ms = host_wall_millis().unwrap_or(state.updated_wall_ms);
    state.finalization = Some(json!({
        "failure_stage": "session_start",
        "error": error.to_string(),
        "cleanup": cleanup,
        "ime_stop": ime_stop,
    }));
    let _write_state = write_state(&identity.state_path, state);
    let runtime_cleanup = clear_runtime_files(identity);
    json!({
        "error_code": "session_start_failed_after_lock",
        "mutated": true,
        "state_path": identity.state_path,
        "lock_snapshot_path": identity.lock_snapshot_path,
        "cleanup": cleanup,
        "ime_stop": ime_stop,
        "runtime_cleanup": runtime_cleanup,
    })
}

fn claim_finalization_owner(
    identity: &SessionIdentity,
    lock: &mut CaptureSessionLock,
) -> CliResult<()> {
    if lock.lock_state != LockState::Active
        || lock.observed_lifecycle_state != LifecycleState::Active
    {
        return Err(CliError::with_details(
            "session stop can only claim an active session lock",
            json!({
                "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
                "ok": false,
                "command": "session stop",
                "error_code": "session_not_active",
                "mutated": false,
                "observed_lock_state": lock.lock_state,
                "observed_lifecycle_state": lock.observed_lifecycle_state,
                "run_id": identity.run_id,
            }),
        ));
    }
    if lock.finalization_owner.is_some() {
        return Err(CliError::with_details(
            "session finalization is already in progress",
            json!({
                "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
                "ok": false,
                "command": "session stop",
                "error_code": "finalization_in_progress",
                "mutated": false,
                "finalization_owner": lock.finalization_owner,
                "run_id": identity.run_id,
            }),
        ));
    }
    let expected = lock.mutation_seq;
    lock.finalization_owner = Some(FinalizationOwner {
        owner_pid: process::id(),
        owner_host: host_name(),
        invocation_id: invocation_id(host_wall_millis()?),
        claimed_wall_ms: host_wall_millis()?,
    });
    lock.lock_state = LockState::Finalizing;
    lock.mutation_seq = lock.mutation_seq.saturating_add(1_u64);
    lock.updated_wall_ms = host_wall_millis()?;
    checked_update_json(
        &identity.lock_path,
        LOCK_SCHEMA,
        "mutation_seq",
        expected,
        &serde_json::to_value(lock)?,
    )
}

fn mark_processes_stop_requested(
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
) -> CliResult<()> {
    for descriptor in state.processes.values_mut() {
        if descriptor.state == ProcessState::Running {
            descriptor.state = ProcessState::StopRequested;
        }
    }
    state.transition_seq = state.transition_seq.saturating_add(1_u64);
    state.updated_wall_ms = host_wall_millis()?;
    write_state(&identity.state_path, state)
}

fn stop_named_process(
    effects: &mut dyn LifecycleEffects,
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
    name: &str,
) -> CliResult<Value> {
    let Some(descriptor) = state.processes.get(name).cloned() else {
        return Ok(json!({
            "ok": true,
            "skipped": true,
            "reason": "process_not_started",
            "process": name,
        }));
    };
    let outcome = effects.stop_process(&descriptor);
    if let Some(stored) = state.processes.get_mut(name) {
        stored.state = outcome.recommended_state;
        stored.failure =
            (!outcome.ok).then(|| String::from("process stop did not complete cleanly"));
        stored.exit_observed_wall_ms = Some(host_wall_millis()?);
    }
    state.transition_seq = state.transition_seq.saturating_add(1_u64);
    state.updated_wall_ms = host_wall_millis()?;
    write_state(&identity.state_path, state)?;
    Ok(stop_outcome_json(&outcome))
}

fn stop_end_evidence(
    effects: &dyn LifecycleEffects,
    identity: &SessionIdentity,
    state: &CaptureSessionState,
) -> CliResult<Value> {
    let with_evidence = state
        .start_config
        .get("with_evidence")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !with_evidence {
        return Ok(json!({
            "schema": EVIDENCE_CAPTURE_SCHEMA,
            "enabled": false,
            "requested": false,
            "phase": "end",
        }));
    }
    let detail = if state
        .start_config
        .get("full_accessibility_evidence")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        AccessibilityDetail::Full
    } else {
        AccessibilityDetail::Compressed
    };
    effects.capture_evidence(
        &identity.output_dir.join("evidence").join("end"),
        detail,
        "end",
    )
}

fn finalize_artifacts(
    effects: &dyn LifecycleEffects,
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
    ledger: &mut FinalizationLedger,
) -> Value {
    let mut outcomes = BTreeMap::new();
    finalize_ime_logs(effects, identity, state, ledger, &mut outcomes);
    finalize_video(effects, identity, state, ledger, &mut outcomes);
    finalize_getevent(identity, state, ledger, &mut outcomes);
    finalize_manifest(identity, state, ledger, &mut outcomes);
    let _state_write = write_state(&identity.state_path, state);
    json!({
        "ok": outcomes.values().all(|value| value.get("ok").and_then(Value::as_bool) == Some(true)),
        "outcomes": outcomes,
    })
}

fn finalize_ime_logs(
    effects: &dyn LifecycleEffects,
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
    ledger: &mut FinalizationLedger,
    outcomes: &mut BTreeMap<String, Value>,
) {
    let pull_dir = identity.output_dir.join("ime-pull-tmp");
    if pull_dir.exists() {
        let _remove = fs::remove_dir_all(&pull_dir);
    }
    let pull = effects.pull_logs(&pull_dir);
    record_step(ledger, "pull_ime_logs", Requirement::Required, &pull);
    outcomes.insert(String::from("pull_ime_logs"), result_value(&pull));
    let stage = pull
        .as_ref()
        .map(|_| stage_ime_logs(&pull_dir, &identity.output_dir.join("ime")));
    let stage_value = stage
        .as_ref()
        .map_or_else(|_| json!({"ok": false, "skipped": true}), result_value);
    record_step_value(
        ledger,
        "stage_ime_logs",
        Requirement::Required,
        &stage_value,
    );
    outcomes.insert(String::from("stage_ime_logs"), stage_value.clone());
    let validation = stage.as_ref().map_or_else(
        |_| Err(CliError::new("IME staging did not run")),
        |_| validate_logs(&identity.output_dir.join("ime"), Some(&identity.run_id)),
    );
    let validation_value = result_value(&validation);
    record_step_value(
        ledger,
        "validate_ime_logs",
        Requirement::Required,
        &validation_value,
    );
    let validation_path = identity.output_dir.join("validation.json");
    if validation.is_ok() {
        let _write = write_json_atomic(&validation_path, &validation_value);
        if validation_value.get("ok").and_then(Value::as_bool) == Some(true) {
            mark_artifact(
                state,
                "validation",
                &validation_path,
                "input_dynamics_validation_result.v1",
            );
        } else {
            mark_failed_artifact(
                state,
                "validation",
                &validation_path,
                "input_dynamics_validation_result.v1",
                "validation returned ok:false",
            );
        }
    }
    outcomes.insert(String::from("validate_ime_logs"), validation_value.clone());
}

fn finalize_video(
    effects: &dyn LifecycleEffects,
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
    ledger: &mut FinalizationLedger,
    outcomes: &mut BTreeMap<String, Value>,
) {
    let video = pull_video_if_needed(effects, identity, state);
    let video_value = result_value(&video);
    let video_required = state.processes.contains_key(SCREENRECORD_PROCESS);
    record_step_value(
        ledger,
        "pull_video",
        if video_required {
            Requirement::Required
        } else {
            Requirement::Optional
        },
        &video_value,
    );
    outcomes.insert(String::from("pull_video"), video_value);
}

fn finalize_getevent(
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
    ledger: &mut FinalizationLedger,
    outcomes: &mut BTreeMap<String, Value>,
) {
    let normalize = normalize_getevent(identity);
    let normalize_value = result_value(&normalize);
    record_step_value(
        ledger,
        "normalize_getevent",
        Requirement::Required,
        &normalize_value,
    );
    if normalize_value.get("ok").and_then(Value::as_bool) == Some(true) {
        mark_artifact(
            state,
            "adb_getevent_jsonl",
            &identity.output_dir.join("adb").join("getevent.jsonl"),
            GETEVENT_SCHEMA,
        );
        mark_artifact(
            state,
            "adb_getevent_raw",
            &identity.output_dir.join("adb").join("getevent.raw.log"),
            "text/plain",
        );
    } else if normalize.is_ok() {
        mark_failed_artifact(
            state,
            "adb_getevent_jsonl",
            &identity.output_dir.join("adb").join("getevent.jsonl"),
            GETEVENT_SCHEMA,
            "getevent normalization returned ok:false",
        );
        mark_failed_artifact(
            state,
            "adb_getevent_raw",
            &identity.output_dir.join("adb").join("getevent.raw.log"),
            "text/plain",
            "getevent normalization returned ok:false",
        );
    } else {
        // The ledger carries the normalization error. The required artifacts
        // remain unsatisfied because there is no trustworthy normalized output.
    }
    outcomes.insert(String::from("normalize_getevent"), normalize_value.clone());
}

fn finalize_manifest(
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
    ledger: &mut FinalizationLedger,
    outcomes: &mut BTreeMap<String, Value>,
) {
    let manifest = write_manifest(identity, state, outcomes);
    let manifest_value = result_value(&manifest);
    record_step_value(
        ledger,
        "write_manifest",
        Requirement::Required,
        &manifest_value,
    );
    if manifest.is_ok() {
        mark_artifact(
            state,
            "manifest",
            &identity.output_dir.join("manifest.json"),
            RECORD_MANIFEST_SCHEMA,
        );
    }
    outcomes.insert(String::from("write_manifest"), manifest_value);
}

fn new_finalization_ledger(identity: &SessionIdentity) -> CliResult<FinalizationLedger> {
    Ok(FinalizationLedger {
        schema: String::from(FINALIZATION_SCHEMA),
        run_id: identity.run_id.clone(),
        run_state: LifecycleState::Finalizing,
        attempt_id: invocation_id(host_wall_millis()?),
        owner_pid: process::id(),
        owner_host: host_name(),
        started_wall_ms: host_wall_millis()?,
        finished_wall_ms: None,
        failure_stage: None,
        failure_reasons: Vec::new(),
        cleanup_attempted: true,
        cleanup_ok: false,
        last_completed_step: None,
        steps: Vec::new(),
    })
}

fn record_step(
    ledger: &mut FinalizationLedger,
    name: &str,
    requirement: Requirement,
    result: &CliResult<Value>,
) {
    record_step_value(ledger, name, requirement, &result_value(result));
}

fn record_step_value(
    ledger: &mut FinalizationLedger,
    name: &str,
    requirement: Requirement,
    value: &Value,
) {
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let mut step = FinalizationStep::new(
        name,
        requirement,
        if ok {
            StepStatus::Ok
        } else {
            StepStatus::Failed
        },
    );
    let wall_ms = host_wall_millis().unwrap_or(0_u64);
    step.attempt_count = 1_u64;
    step.started_wall_ms = Some(wall_ms);
    step.finished_wall_ms = Some(wall_ms);
    step.message = Some(value.to_string());
    if ok {
        ledger.last_completed_step = Some(String::from(name));
    } else {
        ledger.failure_reasons.push(format!("{name}: {value}"));
        if ledger.failure_stage.is_none() {
            ledger.failure_stage = Some(String::from(name));
        }
    }
    ledger.steps.push(step);
}

fn result_value(result: &CliResult<Value>) -> Value {
    match result.as_ref() {
        Ok(value) => {
            if value.get("ok").is_some() {
                value.clone()
            } else {
                json!({"ok": true, "value": value})
            }
        }
        Err(error) => json!({"ok": false, "error": error.to_string()}),
    }
}

fn pull_video_if_needed(
    effects: &dyn LifecycleEffects,
    identity: &SessionIdentity,
    state: &mut CaptureSessionState,
) -> CliResult<Value> {
    let Some(descriptor) = state.processes.get(SCREENRECORD_PROCESS) else {
        return Ok(json!({
            "ok": true,
            "skipped": true,
            "reason": "video_disabled",
        }));
    };
    let remote_path = descriptor
        .remote_command
        .get(1)
        .ok_or_else(|| CliError::new("screenrecord descriptor missing remote path"))?;
    let start_timing = required_start_config_ok(state, "video_start_timing")?;
    let stop_timing = required_start_config_ok(state, "video_stop_timing")?;
    validate_video_timing_order(&start_timing, &stop_timing)?;
    let local_path = identity.output_dir.join("video").join("screen.mp4");
    let pull = effects.pull_file(remote_path, &local_path)?;
    let pull_ok = pull.get("ok").and_then(Value::as_bool) == Some(true);
    let cleanup = effects
        .adb_shell(
            vec![String::from("rm"), String::from("-f"), remote_path.clone()],
            FailureMode::AllowFailure,
        )
        .unwrap_or_else(|error| json!({"ok": false, "error": error.to_string()}));
    let timing_path = identity.output_dir.join("video").join("timing.json");
    let file = if pull_ok {
        file_fingerprint(&local_path).unwrap_or_else(|error| {
            json!({
                "ok": false,
                "error": error.to_string(),
            })
        })
    } else {
        Value::Null
    };
    let byte_count = file
        .get("byte_count")
        .and_then(Value::as_u64)
        .unwrap_or(0_u64);
    let video_ok =
        pull_ok && byte_count > 0_u64 && cleanup.get("ok").and_then(Value::as_bool) == Some(true);
    let video_json = json!({
        "schema": VIDEO_CAPTURE_SCHEMA,
        "ok": video_ok,
        "enabled": true,
        "required": true,
        "remote_path": remote_path,
        "local_path": local_path.to_string_lossy(),
        "start": start_timing,
        "stop": stop_timing,
        "pull": pull,
        "remote_cleanup": cleanup,
        "file": file,
        "failure_reason": (!video_ok).then_some("video pull, fingerprint, byte count, or remote cleanup failed"),
    });
    write_json_atomic(&timing_path, &video_json)?;
    if video_ok {
        mark_artifact(state, "video_screen", &local_path, VIDEO_CAPTURE_SCHEMA);
        mark_artifact(state, "video_timing", &timing_path, VIDEO_CAPTURE_SCHEMA);
        Ok(video_json)
    } else {
        mark_failed_artifact(
            state,
            "video_screen",
            &local_path,
            VIDEO_CAPTURE_SCHEMA,
            "video finalization returned ok:false",
        );
        mark_failed_artifact(
            state,
            "video_timing",
            &timing_path,
            VIDEO_CAPTURE_SCHEMA,
            "video finalization returned ok:false",
        );
        Err(CliError::new(format!(
            "failed to pull screenrecord video: {video_json}"
        )))
    }
}

fn required_start_config_ok(state: &CaptureSessionState, key: &str) -> CliResult<Value> {
    let value = state
        .start_config
        .get(key)
        .cloned()
        .ok_or_else(|| CliError::new(format!("session start_config missing {key}")))?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(CliError::new(format!(
            "session start_config {key} is not ok:true"
        )));
    }
    Ok(value)
}

fn validate_video_timing_order(start: &Value, stop: &Value) -> CliResult<()> {
    let start_before = start
        .get("before")
        .ok_or_else(|| CliError::new("video start timing missing before marker"))?;
    let start_after = start
        .get("after")
        .ok_or_else(|| CliError::new("video start timing missing after marker"))?;
    let stop_before = stop
        .get("before")
        .ok_or_else(|| CliError::new("video stop timing missing before marker"))?;
    let stop_after = stop
        .get("after")
        .ok_or_else(|| CliError::new("video stop timing missing after marker"))?;
    validate_probe_order(start_before, start_after)?;
    validate_probe_order(start_after, stop_before)?;
    validate_probe_order(stop_before, stop_after)
}

fn normalize_getevent(identity: &SessionIdentity) -> CliResult<Value> {
    let raw = identity.output_dir.join("adb").join("getevent.raw.log");
    let jsonl = identity.output_dir.join("adb").join("getevent.jsonl");
    let stats = normalize_file(&raw, &jsonl)?;
    let raw_byte_count = fs::metadata(&raw)?.len();
    let ok = raw_byte_count > 0_u64 && stats.records > 0_u64;
    Ok(json!({
        "ok": ok,
        "schema": GETEVENT_SCHEMA,
        "input": path_string(&raw)?,
        "output": path_string(&jsonl)?,
        "raw_byte_count": raw_byte_count,
        "stats": normalize_stats_json(&stats),
        "failure_reason": (!ok).then_some("getevent raw output or normalized records are empty"),
    }))
}

fn write_manifest(
    identity: &SessionIdentity,
    state: &CaptureSessionState,
    outcomes: &BTreeMap<String, Value>,
) -> CliResult<Value> {
    let manifest_path = identity.output_dir.join("manifest.json");
    let video_timing_path = identity.output_dir.join("video").join("timing.json");
    let video_timing = read_optional_json_value(&video_timing_path);
    let manifest = json!({
        "schema": RECORD_MANIFEST_SCHEMA,
        "external_run_id": identity.run_id,
        "package_name": identity.package_name,
        "host_start_wall_ms": state.created_wall_ms,
        "host_stop_wall_ms": state.updated_wall_ms,
        "device": {
            "serial": identity.device_serial,
        },
        "input_actor": "human",
        "input_controller": Value::Null,
        "input_cadence_policy": "manual",
        "output_dir": identity.output_dir,
        "ime_dir": identity.output_dir.join("ime"),
        "adb_dir": identity.output_dir.join("adb"),
        "video_dir": identity.output_dir.join("video"),
        "evidence_dir": identity.output_dir.join("evidence"),
        "getevent_raw_log": identity.output_dir.join("adb").join("getevent.raw.log"),
        "getevent_jsonl": identity.output_dir.join("adb").join("getevent.jsonl"),
        "getevent_stderr_log": identity.output_dir.join("adb").join("getevent.stderr.log"),
        "video": {
            "enabled": state.start_config.get("video_enabled").cloned().unwrap_or(Value::Bool(false)),
            "required": state.processes.contains_key(SCREENRECORD_PROCESS),
            "timing_path": video_timing_path,
            "start": state.start_config.get("video_start_timing").cloned().unwrap_or(Value::Null),
            "stop": state.start_config.get("video_stop_timing").cloned().unwrap_or(Value::Null),
            "file": video_timing.get("file").cloned().unwrap_or(Value::Null),
            "capture": video_timing,
        },
        "session": {
            "state_path": identity.state_path,
            "finalization_path": identity.finalization_path,
            "lock_snapshot_path": identity.lock_snapshot_path,
            "lifecycle": state.lifecycle,
            "processes": state.processes,
        },
        "evidence": {
            "enabled": state.start_config.get("with_evidence").cloned().unwrap_or(Value::Bool(false)),
            "policy": if state.start_config.get("with_evidence").and_then(Value::as_bool).unwrap_or(false) {
                "start_end"
            } else {
                "none"
            },
            "start": state.start_config.get("evidence_start").cloned().unwrap_or(Value::Null),
            "end": outcomes.get("capture_end_evidence").cloned().unwrap_or(Value::Null),
        },
        "commands": outcomes,
        "artifacts": manifest_artifacts(&state.artifacts),
        "session_artifacts": state.artifacts,
    });
    write_json_atomic(&manifest_path, &manifest)?;
    Ok(json!({
        "ok": true,
        "path": manifest_path,
    }))
}

fn manifest_artifacts(artifacts: &BTreeMap<String, ArtifactStatus>) -> Value {
    let mut object = serde_json::Map::new();
    for (key, artifact) in artifacts {
        let mut value = serde_json::to_value(artifact).unwrap_or(Value::Null);
        if let Some(map) = value.as_object_mut() {
            map.insert(String::from("exists"), Value::Bool(artifact.present));
        }
        object.insert(key.clone(), value);
    }
    Value::Object(object)
}

fn read_optional_json_value(path: &Path) -> Value {
    let Ok(text) = fs::read_to_string(path) else {
        return Value::Null;
    };
    serde_json::from_str(text.trim()).unwrap_or(Value::Null)
}

fn stage_ime_logs(pull_dir: &Path, ime_dir: &Path) -> CliResult<Value> {
    fs::create_dir_all(ime_dir)?;
    let pulled_log_dir = pull_dir.join(LOG_DIR);
    let mut staged = Vec::new();
    for entry_result in fs::read_dir(&pulled_log_dir)? {
        let entry = entry_result?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() || !should_stage_ime_file(&entry.path()) {
            continue;
        }
        let destination = ime_dir.join(entry.file_name());
        fs::copy(entry.path(), &destination)?;
        staged.push(destination.to_string_lossy().to_string());
    }
    staged.sort();
    fs::remove_dir_all(pull_dir)?;
    Ok(json!({
        "ok": !staged.is_empty(),
        "staged": staged,
    }))
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

fn mark_artifact(state: &mut CaptureSessionState, key: &str, path: &Path, schema: &str) {
    let fingerprint = file_fingerprint(path).ok();
    let present = fingerprint.is_some();
    let entry = state.artifacts.entry(String::from(key)).or_insert_with(|| {
        ArtifactStatus::new(
            &path.to_string_lossy(),
            Requirement::Required,
            "session_stop",
        )
    });
    entry.present = present;
    entry.valid = present;
    entry.schema = Some(String::from(schema));
    entry.fingerprint = fingerprint
        .as_ref()
        .and_then(|value| value.get("sha256"))
        .and_then(Value::as_str)
        .map(String::from);
    entry.failure_reason = (!present).then(|| String::from("artifact missing"));
}

fn mark_failed_artifact(
    state: &mut CaptureSessionState,
    key: &str,
    path: &Path,
    schema: &str,
    reason: &str,
) {
    let fingerprint = file_fingerprint(path).ok();
    let present = fingerprint.is_some();
    let entry = state.artifacts.entry(String::from(key)).or_insert_with(|| {
        ArtifactStatus::new(
            &path.to_string_lossy(),
            Requirement::Required,
            "session_stop",
        )
    });
    entry.present = present;
    entry.valid = false;
    entry.schema = Some(String::from(schema));
    entry.fingerprint = fingerprint
        .as_ref()
        .and_then(|value| value.get("sha256"))
        .and_then(Value::as_str)
        .map(String::from);
    entry.failure_reason = Some(String::from(reason));
}

fn mark_directory_artifact(state: &mut CaptureSessionState, key: &str, path: &Path, schema: &str) {
    let present = path.is_dir();
    let entry = state.artifacts.entry(String::from(key)).or_insert_with(|| {
        ArtifactStatus::new(
            &path.to_string_lossy(),
            Requirement::Required,
            "session_stop",
        )
    });
    entry.present = present;
    entry.valid = present;
    entry.schema = Some(String::from(schema));
    entry.fingerprint = None;
    entry.failure_reason = (!present).then(|| String::from("artifact directory missing"));
}

fn file_fingerprint(path: &Path) -> CliResult<Value> {
    let metadata = fs::metadata(path)?;
    let bytes = fs::read(path)?;
    Ok(json!({
        "ok": true,
        "byte_count": metadata.len(),
        "sha256": format!("sha256:{}", sha256_hex(&bytes)),
    }))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len().saturating_mul(2_usize));
    for byte in digest {
        match write!(&mut output, "{byte:02x}") {
            Ok(()) | Err(_) => {}
        }
    }
    output
}

fn stop_outcome_json(outcome: &StopOutcome) -> Value {
    json!({
        "ok": outcome.ok,
        "method": outcome.method.map(StopMethod::as_str),
        "initial_liveness": process_liveness_json(&outcome.initial_liveness),
        "final_liveness": process_liveness_json(&outcome.final_liveness),
        "recommended_state": outcome.recommended_state,
        "attempts": outcome.attempts.iter().map(|attempt| {
            json!({
                "signal": attempt.signal.as_str(),
                "target_process_group_id": attempt.target_process_group_id,
                "ok": attempt.ok,
                "detail": attempt.detail,
            })
        }).collect::<Vec<_>>(),
    })
}

fn process_liveness_json(liveness: &ProcessLiveness) -> Value {
    json!({
        "status": liveness.status,
        "host_pid": liveness.host_pid,
        "host_process_group_id": liveness.host_process_group_id,
        "observed_process_group_id": liveness.observed_process_group_id,
        "message": liveness.message,
    })
}

fn no_active_session_status(
    package_name: &str,
    device_serial: &str,
    runtime_paths: &RuntimeSessionPaths,
    command: &str,
) -> Value {
    json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": false,
        "command": command,
        "error_code": "no_active_session",
        "message": "no active umbrella session was found",
        "mutated": false,
        "package_name": package_name,
        "device_serial": device_serial,
        "current_path": runtime_paths.current,
        "lock_path": runtime_paths.lock,
    })
}

fn stop_requires_run_id_result(observed_run_id: Option<&str>, current: &Value) -> Value {
    json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": false,
        "command": "session stop",
        "error_code": "session_stop_run_id_required",
        "message": "session stop requires --run-id before it mutates",
        "mutated": false,
        "run_id": observed_run_id,
        "active": current,
        "suggested_next_command": observed_run_id.map_or(Value::Null, |run_id| json!({
            "argv": ["input-dynamics", "session", "stop", "--run-id", run_id],
            "reason": "explicit run-id prevents stopping the wrong active session",
        })),
    })
}

fn selector_mismatch_result(command: &str, expected: &str, observed: Option<&str>) -> Value {
    json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": false,
        "command": command,
        "error_code": "selector_mismatch",
        "message": "requested run id does not match the active session",
        "expected_run_id": expected,
        "observed_run_id": observed,
        "mutated": false,
    })
}

fn classified_json(classified: &crate::session_state::io::ClassifiedJson) -> Value {
    json!({
        "status": classified.status,
        "path": classified.path,
        "expected_schema": classified.expected_schema,
        "observed_schema": classified.observed_schema,
        "message": classified.message,
    })
}

struct RuntimeIdentityView<'a> {
    package_name: &'a str,
    device_serial: &'a str,
    current: &'a Value,
    state: &'a CaptureSessionState,
    lock: &'a CaptureSessionLock,
    state_path: &'a Path,
    lock_path: &'a Path,
    current_path: &'a Path,
}

fn validate_runtime_identity(view: &RuntimeIdentityView<'_>) -> CliResult<()> {
    let mismatches = runtime_identity_mismatches(&RuntimeIdentityView {
        package_name: view.package_name,
        device_serial: view.device_serial,
        current: view.current,
        state: view.state,
        lock: view.lock,
        state_path: view.state_path,
        lock_path: view.lock_path,
        current_path: view.current_path,
    });
    if mismatches.is_empty() {
        return Ok(());
    }
    Err(CliError::with_details(
        "active session runtime files do not describe the same session",
        json!({
            "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
            "ok": false,
            "command": "session stop",
            "error_code": "runtime_identity_mismatch",
            "mutated": false,
            "identity_mismatches": mismatches,
            "current_path": view.current_path,
            "state_path": view.state_path,
            "lock_path": view.lock_path,
        }),
    ))
}

fn runtime_identity_mismatches(view: &RuntimeIdentityView<'_>) -> Vec<String> {
    let mut mismatches = Vec::new();
    compare_current_string(
        view.current,
        "package_name",
        view.package_name,
        &mut mismatches,
    );
    compare_current_string(
        view.current,
        "device_serial",
        view.device_serial,
        &mut mismatches,
    );
    compare_current_string(view.current, "run_id", &view.state.run_id, &mut mismatches);
    compare_current_path(
        view.current,
        "output_dir",
        Path::new(&view.state.run_root),
        &mut mismatches,
    );
    compare_current_path(view.current, "state_path", view.state_path, &mut mismatches);
    compare_current_path(view.current, "lock_path", view.lock_path, &mut mismatches);

    compare_named_string(
        "state.package_name",
        &view.state.package_name,
        view.package_name,
        &mut mismatches,
    );
    compare_named_string(
        "state.device_serial",
        &view.state.device_serial,
        view.device_serial,
        &mut mismatches,
    );
    compare_named_string(
        "lock.package_name",
        &view.lock.package_name,
        view.package_name,
        &mut mismatches,
    );
    compare_named_string(
        "lock.device_serial",
        &view.lock.device_serial,
        view.device_serial,
        &mut mismatches,
    );
    compare_named_string(
        "lock.run_id",
        &view.lock.run_id,
        &view.state.run_id,
        &mut mismatches,
    );
    compare_named_path(
        "lock.output_dir",
        Path::new(&view.lock.output_dir),
        Path::new(&view.state.run_root),
        &mut mismatches,
    );
    compare_named_path(
        "lock.state_path",
        Path::new(&view.lock.state_path),
        view.state_path,
        &mut mismatches,
    );
    mismatches
}

fn compare_current_string(
    current: &Value,
    key: &str,
    expected: &str,
    mismatches: &mut Vec<String>,
) {
    let observed = current.get(key).and_then(Value::as_str);
    if observed != Some(expected) {
        mismatches.push(format!(
            "current.{key}: expected {expected:?}, observed {observed:?}"
        ));
    }
}

fn compare_current_path(current: &Value, key: &str, expected: &Path, mismatches: &mut Vec<String>) {
    let observed = current.get(key).and_then(Value::as_str).map(PathBuf::from);
    if observed.as_deref() != Some(expected) {
        mismatches.push(format!(
            "current.{key}: expected {}, observed {:?}",
            expected.display(),
            observed
        ));
    }
}

fn compare_named_string(name: &str, observed: &str, expected: &str, mismatches: &mut Vec<String>) {
    if observed != expected {
        mismatches.push(format!(
            "{name}: expected {expected:?}, observed {observed:?}"
        ));
    }
}

fn compare_named_path(name: &str, observed: &Path, expected: &Path, mismatches: &mut Vec<String>) {
    if observed != expected {
        mismatches.push(format!(
            "{name}: expected {}, observed {}",
            expected.display(),
            observed.display()
        ));
    }
}

fn invalid_runtime_json_result(
    command: &str,
    error_code: &str,
    classified: &crate::session_state::io::ClassifiedJson,
) -> Value {
    json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": false,
        "command": command,
        "error_code": error_code,
        "message": "runtime session JSON is not valid for this command",
        "mutated": false,
        "read": classified_json(classified),
    })
}

struct RuntimeRepairRequired<'a> {
    command: &'a str,
    reason_code: &'a str,
    package_name: &'a str,
    device_serial: &'a str,
    runtime_paths: &'a RuntimeSessionPaths,
    current_read: &'a crate::session_state::io::ClassifiedJson,
    lock_read: &'a crate::session_state::io::ClassifiedJson,
}

fn runtime_repair_required_result(input: &RuntimeRepairRequired<'_>) -> Value {
    json!({
        "schema": SESSION_LIFECYCLE_RESULT_SCHEMA,
        "ok": false,
        "command": input.command,
        "error_code": "session_runtime_repair_required",
        "reason_code": input.reason_code,
        "message": "session runtime files are present but cannot be used as an active session",
        "mutated": false,
        "package_name": input.package_name,
        "device_serial": input.device_serial,
        "current_path": input.runtime_paths.current,
        "lock_path": input.runtime_paths.lock,
        "current_read": classified_json(input.current_read),
        "lock_read": classified_json(input.lock_read),
        "repair_available": false,
        "suggested_next_command": Value::Null,
    })
}

fn required_path(value: &Value, key: &str) -> CliResult<PathBuf> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| CliError::new(format!("active session current is missing {key}")))
}

fn read_state(path: &Path) -> CliResult<CaptureSessionState> {
    let read = read_json_classified(path, STATE_SCHEMA);
    if read.status == ReadStatus::Valid
        && let Some(value) = read.value
    {
        return Ok(serde_json::from_value(value)?);
    }
    Err(CliError::new(format!(
        "failed to read capture session state {}: {:?}",
        path.display(),
        read.status
    )))
}

fn read_lock(path: &Path) -> CliResult<CaptureSessionLock> {
    let read = read_json_classified(path, LOCK_SCHEMA);
    if read.status == ReadStatus::Valid
        && let Some(value) = read.value
    {
        return Ok(serde_json::from_value(value)?);
    }
    Err(CliError::new(format!(
        "failed to read capture session lock {}: {:?}",
        path.display(),
        read.status
    )))
}

fn ensure_result_ok(value: &Value, action: &str) -> CliResult<()> {
    if value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(());
    }
    Err(CliError::new(format!("{action} failed: {value}")))
}

fn history_event(state: LifecycleState, stage: &str, wall_ms: u64) -> Value {
    json!({
        "state": state,
        "stage": stage,
        "host_wall_ms": wall_ms,
    })
}

fn lifecycle_stage(state: LifecycleState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(String::from))
        .unwrap_or_else(|| String::from("unknown"))
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| String::from("unknown-host"))
}

fn invocation_id(wall_ms: u64) -> String {
    format!("{}-{wall_ms}", process::id())
}

fn remote_video_path(run_id: &str) -> String {
    format!(
        "/data/local/tmp/input-dynamics-screen-{}.mp4",
        hash_prefix(run_id, 16_usize)
    )
}

fn hash_prefix(text: &str, length: usize) -> String {
    sha256_hex(text.as_bytes()).chars().take(length).collect()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::session_process::{HostSignal, ProcessLivenessStatus, SignalAttempt};

    use super::*;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0_u64);

    #[test]
    fn human_start_writes_active_state_current_lock_and_process_descriptors() {
        let root = unique_temp_dir("session-lifecycle-start");
        let mut effects = FakeEffects::new("serial-start");
        let request = HumanSessionStart {
            run_id: String::from("run-start"),
            out: root.clone(),
            with_evidence: true,
            full_accessibility_evidence: false,
            video_enabled: true,
        };

        let Some(result) = assert_ok(
            start_human_session_with_effects(&mut effects, &request),
            "fake start",
        ) else {
            return;
        };

        assert_eq!(result.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            result.get("mutated").and_then(Value::as_bool),
            Some(true),
            "human start is now mutating"
        );
        let state_path = root.join("session").join("state.json");
        let Some(state) = assert_ok(read_state(&state_path), "read state") else {
            return;
        };
        assert_eq!(state.lifecycle.state, LifecycleState::Active);
        assert_eq!(
            state
                .processes
                .get(SCREENRECORD_PROCESS)
                .map(|process| process.state),
            Some(ProcessState::Running),
            "screenrecord descriptor should be persisted as running"
        );
        assert_eq!(
            state
                .processes
                .get(GETEVENT_PROCESS)
                .map(|process| process.state),
            Some(ProcessState::Running),
            "getevent descriptor should be persisted as running"
        );
        let runtime = runtime_paths(effects.package_name(), &effects.serial);
        assert!(
            runtime.current.exists(),
            "start should publish active current pointer"
        );
        assert!(runtime.lock.exists(), "start should publish runtime lock");
        cleanup_paths(&root, &runtime);
    }

    #[test]
    fn stop_without_run_id_is_non_mutating_and_suggests_exact_command() {
        let root = unique_temp_dir("session-lifecycle-stop-no-run-id");
        let mut effects = FakeEffects::new("serial-stop-no-run-id");
        let request = HumanSessionStart {
            run_id: String::from("run-stop-no-run-id"),
            out: root.clone(),
            with_evidence: false,
            full_accessibility_evidence: false,
            video_enabled: false,
        };
        let Some(_start) = assert_ok(
            start_human_session_with_effects(&mut effects, &request),
            "fake start",
        ) else {
            return;
        };

        let Some(result) = assert_ok(
            stop_session_with_effects(&mut effects, &SessionStopRequest { run_id: None }),
            "safe stop",
        ) else {
            return;
        };

        assert_eq!(result.get("ok").and_then(Value::as_bool), Some(false));
        assert_eq!(
            result.get("mutated").and_then(Value::as_bool),
            Some(false),
            "no-arg stop must not mutate"
        );
        assert_eq!(
            result
                .pointer("/suggested_next_command/argv/4")
                .and_then(Value::as_str),
            Some("run-stop-no-run-id"),
            "safe stop response should include exact active run id"
        );
        let Some(state) = assert_ok(
            read_state(&root.join("session").join("state.json")),
            "read state after safe stop",
        ) else {
            return;
        };
        assert_eq!(
            state.lifecycle.state,
            LifecycleState::Active,
            "safe stop response must not change lifecycle state"
        );
        let runtime = runtime_paths(effects.package_name(), &effects.serial);
        cleanup_paths(&root, &runtime);
    }

    #[test]
    fn status_reports_liveness_without_mutating_state() {
        let root = unique_temp_dir("session-lifecycle-status");
        let mut effects = FakeEffects::new("serial-status");
        let request = HumanSessionStart {
            run_id: String::from("run-status"),
            out: root.clone(),
            with_evidence: false,
            full_accessibility_evidence: false,
            video_enabled: false,
        };
        let Some(_start) = assert_ok(
            start_human_session_with_effects(&mut effects, &request),
            "fake start",
        ) else {
            return;
        };
        let Some(before_state) = assert_ok(
            read_state(&root.join("session").join("state.json")),
            "read state before status",
        ) else {
            return;
        };
        let before = before_state.transition_seq;

        let Some(status) = assert_ok(
            session_status_with_effects(
                &effects,
                &SessionStatusRequest {
                    run_id: Some(String::from("run-status")),
                },
            ),
            "status",
        ) else {
            return;
        };

        assert_eq!(status.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            status
                .pointer("/process_liveness/getevent/status")
                .and_then(Value::as_str),
            Some("running"),
            "status should include live process probe output"
        );
        let Some(after_state) = assert_ok(
            read_state(&root.join("session").join("state.json")),
            "read state after status",
        ) else {
            return;
        };
        let after = after_state.transition_seq;
        assert_eq!(before, after, "status must not mutate state");
        let runtime = runtime_paths(effects.package_name(), &effects.serial);
        cleanup_paths(&root, &runtime);
    }

    #[test]
    fn status_reports_lock_only_runtime_as_repair_required() {
        let root = unique_temp_dir("session-lifecycle-lock-only");
        let mut effects = FakeEffects::new("serial-lock-only");
        let request = HumanSessionStart {
            run_id: String::from("run-lock-only"),
            out: root.clone(),
            with_evidence: false,
            full_accessibility_evidence: false,
            video_enabled: false,
        };
        let Some(_start) = assert_ok(
            start_human_session_with_effects(&mut effects, &request),
            "fake start",
        ) else {
            return;
        };
        let runtime = runtime_paths(effects.package_name(), &effects.serial);
        let remove_current = fs::remove_file(&runtime.current);
        assert!(
            remove_current.is_ok(),
            "test setup should remove runtime current: {remove_current:?}"
        );

        let Some(status) = assert_ok(
            session_status_with_effects(&effects, &SessionStatusRequest { run_id: None }),
            "status lock-only",
        ) else {
            return;
        };

        assert_eq!(status.get("ok").and_then(Value::as_bool), Some(false));
        assert_eq!(
            status.get("error_code").and_then(Value::as_str),
            Some("session_runtime_repair_required")
        );
        assert_eq!(
            status.get("reason_code").and_then(Value::as_str),
            Some("runtime_incomplete")
        );
        assert_eq!(
            status
                .pointer("/current_read/status")
                .and_then(Value::as_str),
            Some("missing")
        );
        assert_eq!(
            status.pointer("/lock_read/status").and_then(Value::as_str),
            Some("valid")
        );
        cleanup_paths(&root, &runtime);
    }

    #[test]
    fn status_reports_corrupt_current_as_repair_required() {
        let root = unique_temp_dir("session-lifecycle-corrupt-current");
        let mut effects = FakeEffects::new("serial-corrupt-current");
        let request = HumanSessionStart {
            run_id: String::from("run-corrupt-current"),
            out: root.clone(),
            with_evidence: false,
            full_accessibility_evidence: false,
            video_enabled: false,
        };
        let Some(_start) = assert_ok(
            start_human_session_with_effects(&mut effects, &request),
            "fake start",
        ) else {
            return;
        };
        let runtime = runtime_paths(effects.package_name(), &effects.serial);
        let corrupt_current = fs::write(&runtime.current, "{");
        assert!(
            corrupt_current.is_ok(),
            "test setup should corrupt runtime current: {corrupt_current:?}"
        );

        let Some(status) = assert_ok(
            session_status_with_effects(&effects, &SessionStatusRequest { run_id: None }),
            "status corrupt-current",
        ) else {
            return;
        };

        assert_eq!(status.get("ok").and_then(Value::as_bool), Some(false));
        assert_eq!(
            status.get("error_code").and_then(Value::as_str),
            Some("session_runtime_repair_required")
        );
        assert_eq!(
            status.get("reason_code").and_then(Value::as_str),
            Some("runtime_incomplete")
        );
        assert_eq!(
            status
                .pointer("/current_read/status")
                .and_then(Value::as_str),
            Some("corrupt")
        );
        assert_eq!(
            status.pointer("/lock_read/status").and_then(Value::as_str),
            Some("valid")
        );
        cleanup_paths(&root, &runtime);
    }

    #[test]
    fn mutating_stop_clears_runtime_and_records_cleanup() {
        let root = unique_temp_dir("session-lifecycle-stop");
        let mut effects = FakeEffects::new("serial-stop");
        let request = HumanSessionStart {
            run_id: String::from("run-stop"),
            out: root.clone(),
            with_evidence: false,
            full_accessibility_evidence: false,
            video_enabled: false,
        };
        let Some(_start) = assert_ok(
            start_human_session_with_effects(&mut effects, &request),
            "fake start",
        ) else {
            return;
        };
        let runtime = runtime_paths(effects.package_name(), &effects.serial);
        assert!(runtime.current.exists(), "start should publish current");
        assert!(runtime.lock.exists(), "start should publish lock");

        let Some(stop) = assert_ok(
            stop_session_with_effects(
                &mut effects,
                &SessionStopRequest {
                    run_id: Some(String::from("run-stop")),
                },
            ),
            "fake stop",
        ) else {
            return;
        };

        assert_eq!(stop.get("mutated").and_then(Value::as_bool), Some(true));
        assert_eq!(
            stop.pointer("/outcomes/clear_runtime/ok")
                .and_then(Value::as_bool),
            Some(true),
            "runtime cleanup should be a required successful finalization step"
        );
        assert_eq!(
            stop.pointer("/finalization/cleanup_ok")
                .and_then(Value::as_bool),
            Some(true),
            "ledger cleanup_ok should reflect successful cleanup"
        );
        assert!(
            !runtime.current.exists(),
            "stop should remove runtime current pointer"
        );
        assert!(!runtime.lock.exists(), "stop should remove runtime lock");
        let Some(state) = assert_ok(
            read_state(&root.join("session").join("state.json")),
            "read stopped state",
        ) else {
            return;
        };
        assert!(
            state.lifecycle.state.is_terminal(),
            "stop should write a terminal lifecycle state"
        );
        cleanup_paths(&root, &runtime);
    }

    struct FakeEffects {
        package: String,
        adb: String,
        serial: String,
        next_pid: Cell<u32>,
    }

    impl FakeEffects {
        fn new(serial_suffix: &str) -> Self {
            let id = TEST_COUNTER.fetch_add(1_u64, Ordering::Relaxed);
            Self {
                package: String::from("org.inputdynamics.ime.debug"),
                adb: String::from("adb"),
                serial: format!("{serial_suffix}-{id}"),
                next_pid: Cell::new(10_000_u32),
            }
        }
    }

    impl LifecycleEffects for FakeEffects {
        fn package_name(&self) -> &str {
            &self.package
        }

        fn adb_program(&self) -> &str {
            &self.adb
        }

        fn selected_device_serial(&self) -> CliResult<String> {
            Ok(self.serial.clone())
        }

        fn scoped_adb_args(&self, args: &[String]) -> CliResult<Vec<String>> {
            let mut scoped = vec![String::from("-s"), self.serial.clone()];
            scoped.extend(args.iter().cloned());
            Ok(scoped)
        }

        fn broadcast(&self, action_suffix: &str, _extras: Vec<String>) -> CliResult<Value> {
            Ok(json!({
                "ok": true,
                "command": action_suffix,
                "package_name": self.package_name(),
                "pending_writes_drained": true,
            }))
        }

        fn adb_shell(&self, _args: Vec<String>, _failure_mode: FailureMode) -> CliResult<Value> {
            Ok(json!({"ok": true}))
        }

        fn pull_file(&self, _remote: &str, local: &Path) -> CliResult<Value> {
            if let Some(parent) = local.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(local, b"fake-video")?;
            Ok(json!({"ok": true}))
        }

        fn pull_logs(&self, out: &Path) -> CliResult<Value> {
            let log_dir = out.join(LOG_DIR);
            fs::create_dir_all(&log_dir)?;
            fs::write(
                log_dir.join("session-run.jsonl"),
                "{\"schema\":\"typing_event.v1\",\"event\":\"session_start\",\"session_id\":\"s\",\"external_run_id\":\"run\"}\n",
            )?;
            Ok(json!({"ok": true}))
        }

        fn capture_clock_probe(&self, phase: &str) -> CliResult<Value> {
            let raw_tick = self.next_pid.get();
            self.next_pid.set(raw_tick.saturating_add(1_u32));
            let tick = i64::from(raw_tick);
            let tick_after = tick
                .checked_add(1_i64)
                .ok_or_else(|| CliError::new("fake clock probe tick overflow"))?;
            let tick_ns = tick
                .checked_mul(1_000_000_i64)
                .ok_or_else(|| CliError::new("fake clock probe nanosecond overflow"))?;
            Ok(json!({
                "schema": "input_dynamics_device_clock_probe.v1",
                "phase": phase,
                "probe_source": "ime_status_broadcast",
                "request_id": format!("fake-{tick}"),
                "host_monotonic_reference": "cli_process_start",
                "host_bracket": {
                    "clock_domain": "host_process_monotonic_ns",
                    "timestamp_source": "host_process",
                    "timestamp_precision": "nanoseconds",
                    "before_ns": tick,
                    "after_ns": tick_after,
                },
                "host_wall_bracket": {
                    "clock_domain": "host_wall_ms",
                    "timestamp_source": "host_process",
                    "timestamp_precision": "milliseconds",
                    "before_ms": tick,
                    "after_ms": tick_after,
                },
                "clock_domain": "device_elapsed_realtime_ns",
                "clock_alignment_status": "not_estimated",
                "device_clock_probe": {
                    "schema": "input_dynamics_device_clock_probe.v1",
                    "request_id": format!("fake-{tick}"),
                    "probe_source": "status_broadcast",
                    "captured_by": "android_control_status",
                    "canonical_clock_domain": "device_elapsed_realtime_ns",
                    "wall_time_role": "diagnostic",
                    "pending_writes_drained": true,
                    "t_uptime_ms": tick,
                    "t_uptime_ns": tick_ns,
                    "t_elapsed_realtime_ns": tick_ns,
                    "t_wall_ms": tick,
                    "uptime_time": {
                        "clock_domain": "android_uptime_ms",
                        "timestamp_source": "callback_capture",
                        "timestamp_precision": "milliseconds",
                        "field": "t_uptime_ms",
                        "field_ns": "t_uptime_ns",
                        "field_ns_precision": "milliseconds_converted_to_nanoseconds"
                    },
                    "elapsed_realtime_time": {
                        "clock_domain": "device_elapsed_realtime_ns",
                        "timestamp_source": "callback_capture",
                        "timestamp_precision": "nanoseconds",
                        "field": "t_elapsed_realtime_ns"
                    },
                    "wall_time": {
                        "clock_domain": "device_wall_ms",
                        "timestamp_source": "callback_capture",
                        "timestamp_precision": "milliseconds",
                        "field": "t_wall_ms"
                    }
                },
                "t_uptime_ms": tick,
                "t_uptime_ns": tick_ns,
                "t_elapsed_realtime_ns": tick_ns,
                "device_wall_ms": tick,
            }))
        }

        fn capture_evidence(
            &self,
            out: &Path,
            _detail: AccessibilityDetail,
            phase: &str,
        ) -> CliResult<Value> {
            fs::create_dir_all(out)?;
            Ok(json!({
                "ok": true,
                "schema": EVIDENCE_CAPTURE_SCHEMA,
                "phase": phase,
            }))
        }

        fn start_process(
            &self,
            spec: &SessionProcessSpec,
            stdout_path: &Path,
            stderr_path: &Path,
        ) -> CliResult<ProcessDescriptor> {
            if let Some(parent) = stdout_path.parent() {
                fs::create_dir_all(parent)?;
            }
            if let Some(parent) = stderr_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(stdout_path, "")?;
            fs::write(stderr_path, "")?;
            let pid = self.next_pid.get();
            self.next_pid.set(pid.saturating_add(1_u32));
            Ok(ProcessDescriptor {
                state: ProcessState::Running,
                host_pid: Some(pid),
                host_process_group_id: Some(pid),
                started_wall_ms: Some(host_wall_millis()?),
                ..pre_spawn_descriptor(spec)
            })
        }

        fn probe_process(&self, descriptor: &ProcessDescriptor) -> ProcessLiveness {
            ProcessLiveness {
                status: ProcessLivenessStatus::Running,
                host_pid: descriptor.host_pid,
                host_process_group_id: descriptor.host_process_group_id,
                observed_process_group_id: descriptor.host_process_group_id,
                message: None,
            }
        }

        fn stop_process(&mut self, descriptor: &ProcessDescriptor) -> StopOutcome {
            let initial = self.probe_process(descriptor);
            let final_liveness = ProcessLiveness {
                status: ProcessLivenessStatus::Missing,
                host_pid: descriptor.host_pid,
                host_process_group_id: descriptor.host_process_group_id,
                observed_process_group_id: None,
                message: Some(String::from("fake stopped")),
            };
            StopOutcome {
                ok: true,
                method: Some(StopMethod::ProcessGroupTerminateThenKill),
                initial_liveness: initial,
                final_liveness,
                attempts: vec![SignalAttempt {
                    signal: HostSignal::Terminate,
                    target_process_group_id: descriptor.host_process_group_id.unwrap_or(1_u32),
                    ok: true,
                    detail: json!({"ok": true}),
                }],
                recommended_state: ProcessState::Stopped,
            }
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "input-dynamics-{prefix}-{}-{}",
            process::id(),
            TEST_COUNTER.fetch_add(1_u64, Ordering::Relaxed)
        ))
    }

    fn assert_ok<T>(result: CliResult<T>, context: &str) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                let error_text = error.to_string();
                assert!(error_text.is_empty(), "{context} failed: {error_text}");
                None
            }
        }
    }

    fn cleanup_paths(root: &Path, runtime: &RuntimeSessionPaths) {
        let _remove_root = fs::remove_dir_all(root);
        let _remove_lock = fs::remove_file(&runtime.lock);
        let _remove_current = fs::remove_file(&runtime.current);
        let _remove_runs = fs::remove_dir_all(&runtime.runs_dir);
    }
}
