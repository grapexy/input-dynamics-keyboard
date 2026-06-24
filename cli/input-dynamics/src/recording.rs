//! Read-only inspection for local recording directories.

use std::ffi::OsStr;
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use input_dynamics_analysis::clock::{AlignmentStatus, ClockDomain};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::clock_probe::{validate_probe_marker, validate_probe_order};
use crate::error::{CliError, CliResult};
use crate::session_state::io::read_json_classified;
use crate::session_state::schema::{
    CaptureSessionLock, CaptureSessionState, FINALIZATION_SCHEMA, FinalizationLedger, LOCK_SCHEMA,
    ReadStatus, STATE_SCHEMA, SessionErrorCode,
};
use crate::validate::validate_logs;

const INSPECTION_SCHEMA: &str = "input_dynamics_recording_inspection.v1";
const SHA256_PREFIX: &str = "sha256:";

#[derive(Clone, Copy)]
enum ArtifactRequirement {
    Required,
    Optional,
}

#[derive(Clone, Copy)]
enum ArtifactSensitivity {
    Normal,
    Sensitive,
}

struct ArtifactSpec {
    key: &'static str,
    path: PathBuf,
    requirement: ArtifactRequirement,
    sensitivity: ArtifactSensitivity,
}

#[derive(Default)]
struct SessionSelection {
    selected: Option<PathBuf>,
    candidates: Vec<PathBuf>,
    warnings: Vec<String>,
}

#[derive(Default)]
struct ValidationInspection {
    current: Value,
    stored: Value,
    stale_reasons: Vec<String>,
    current_ok: bool,
    stored_present: bool,
}

#[derive(Default)]
struct TimelineInspection {
    stale_reasons: Vec<String>,
    artifact_diagnostics: Value,
    exists: bool,
}

#[derive(Default)]
struct RunSummaryInspection {
    stale_reasons: Vec<String>,
    exists: bool,
}

#[derive(Default)]
struct VideoInspection {
    stale_reasons: Vec<String>,
    enabled: bool,
    required: bool,
    exists: bool,
}

#[derive(Default)]
struct VideoMapInspection {
    stale_reasons: Vec<String>,
    stage: Value,
    frame_count: Value,
    probe_status: Value,
    event_mapping: Value,
    exists: bool,
    frame_index_exists: bool,
    event_map_exists: bool,
}

struct ClockInspection {
    value: Value,
    canonical_clock_ready: bool,
    has_legacy_timing: bool,
    needs_canonical_recording: bool,
    warnings: Vec<String>,
}

struct SessionInspection {
    state: SessionFileInspection,
    finalization: SessionFileInspection,
    lock_snapshot: SessionFileInspection,
    classification: SessionClassification,
    lifecycle_state: Option<String>,
    finalization_run_state: Option<String>,
    command: Value,
    stale_reasons: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionClassification {
    UntrackedLegacy,
    RepairRequired,
    Complete,
    Incomplete,
    Aborted,
    Active,
    InProgress,
    Stale,
}

#[derive(Clone, Copy)]
enum EvidenceRefresh {
    Include,
    Omit,
}

impl EvidenceRefresh {
    const fn include(self) -> bool {
        matches!(self, Self::Include)
    }
}

impl SessionClassification {
    const fn as_str(self) -> &'static str {
        match self {
            Self::UntrackedLegacy => "untracked_legacy",
            Self::RepairRequired => "repair_required",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Aborted => "aborted",
            Self::Active => "active",
            Self::InProgress => "in_progress",
            Self::Stale => "stale",
        }
    }

    const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }

    const fn is_incomplete(self) -> bool {
        matches!(self, Self::Incomplete | Self::Aborted)
    }

    const fn is_active_or_in_progress(self) -> bool {
        matches!(self, Self::Active | Self::InProgress)
    }

    const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    const fn is_in_progress(self) -> bool {
        matches!(self, Self::InProgress)
    }

    const fn needs_stop(self) -> bool {
        matches!(self, Self::Active)
    }

    const fn needs_repair(self) -> bool {
        matches!(self, Self::RepairRequired)
    }

    const fn blocks_analysis(self) -> bool {
        !matches!(self, Self::UntrackedLegacy | Self::Complete)
    }
}

struct SessionFileInspection {
    present: bool,
    status: String,
    path: String,
    schema: Value,
    error: Option<String>,
    summary: Value,
}

type SessionJsonValidator = fn(Value) -> Result<(), serde_json::Error>;

struct FlagInputs<'a> {
    manifest: Option<&'a Value>,
    session_state: &'a SessionInspection,
    session: &'a SessionSelection,
    validation: &'a ValidationInspection,
    run_summary: &'a RunSummaryInspection,
    timeline: &'a TimelineInspection,
    video_map: &'a VideoMapInspection,
    video: &'a VideoInspection,
    clock: &'a ClockInspection,
    artifacts: &'a Value,
    note_flags: &'a Value,
}

struct VideoMapOutputCheck<'a> {
    expected_path: &'a Path,
    output_pointer: &'a str,
    expected_schema: &'a str,
    description: &'a str,
}

struct RequiredProcessFlags {
    ended_early: bool,
    unverifiable: bool,
    stop_failed: bool,
    failure_codes: Vec<String>,
}

impl RequiredProcessFlags {
    const fn failed(&self) -> bool {
        self.ended_early || self.unverifiable || self.stop_failed
    }
}

/// Inspect a recording directory without modifying it.
pub(crate) fn inspect_recording(dir: &Path) -> CliResult<Value> {
    require_directory(dir)?;
    let manifest_path = dir.join("manifest.json");
    let validation_path = dir.join("validation.json");
    let manifest = read_optional_json(&manifest_path)?;
    let manifest_ref = manifest.as_ref();
    let external_run_id = string_at(manifest_ref, "/external_run_id").or_else(|| {
        dir.file_name()
            .and_then(OsStr::to_str)
            .map(ToOwned::to_owned)
    });
    let session_state = inspect_session_state(dir);
    let session = select_session_jsonl(dir)?;
    let validation = inspect_validation(dir, &validation_path, external_run_id.as_deref());
    let run_summary = inspect_run_summary(dir)?;
    let timeline = inspect_timeline(dir)?;
    let artifacts = artifact_map(dir, &artifact_specs(dir, session.selected.as_deref()))?;
    let video = inspect_video(dir, manifest_ref, &artifacts)?;
    let video_map = inspect_video_map(dir)?;
    let clock = inspect_clock(dir, manifest_ref, &timeline, &video)?;
    let note_flags = note_flags(dir)?;
    let flag_inputs = FlagInputs {
        manifest: manifest_ref,
        session_state: &session_state,
        session: &session,
        validation: &validation,
        run_summary: &run_summary,
        timeline: &timeline,
        video_map: &video_map,
        video: &video,
        clock: &clock,
        artifacts: &artifacts,
        note_flags: &note_flags,
    };
    let flags = flags_json(&flag_inputs);
    let next_actions = next_actions(dir, &session_state, external_run_id.as_deref(), &flags)?;
    let warnings = warnings(&flag_inputs);
    Ok(inspection_json(&InspectionJson {
        dir,
        external_run_id,
        manifest_ref,
        artifacts,
        session_state: &session_state,
        session: &session,
        validation: &validation,
        run_summary: &run_summary,
        timeline: &timeline,
        video_map: &video_map,
        video: &video,
        clock: &clock,
        note_flags,
        flags,
        warnings,
        next_actions,
    }))
}

struct InspectionJson<'a> {
    dir: &'a Path,
    external_run_id: Option<String>,
    manifest_ref: Option<&'a Value>,
    artifacts: Value,
    session_state: &'a SessionInspection,
    session: &'a SessionSelection,
    validation: &'a ValidationInspection,
    run_summary: &'a RunSummaryInspection,
    timeline: &'a TimelineInspection,
    video_map: &'a VideoMapInspection,
    video: &'a VideoInspection,
    clock: &'a ClockInspection,
    note_flags: Value,
    flags: Value,
    warnings: Vec<String>,
    next_actions: Value,
}

fn inspection_json(input: &InspectionJson<'_>) -> Value {
    json!({
        "ok": true,
        "schema": INSPECTION_SCHEMA,
        "cli_version": env!("CARGO_PKG_VERSION"),
        "recording_dir": ".",
        "external_run_id": input.external_run_id,
        "package_name": string_at(input.manifest_ref, "/package_name"),
        "provenance": provenance_json(input.manifest_ref),
        "timing": timing_json(input.manifest_ref),
        "clock": input.clock.value,
        "device": input.manifest_ref
            .and_then(|value| value.get("device"))
            .cloned()
            .unwrap_or(Value::Null),
        "artifacts": input.artifacts,
        "session": session_state_json(input.session_state),
        "session_jsonl": session_json(input.dir, input.session),
        "validation": validation_json(input.dir, input.validation),
        "run_summary": {
            "exists": input.run_summary.exists,
            "stale_reasons": input.run_summary.stale_reasons,
        },
        "timeline": {
            "exists": input.timeline.exists,
            "artifact_diagnostics": input.timeline.artifact_diagnostics.clone(),
            "stale_reasons": input.timeline.stale_reasons,
        },
        "video_map": {
            "exists": input.video_map.exists,
            "frame_index_exists": input.video_map.frame_index_exists,
            "event_map_exists": input.video_map.event_map_exists,
            "stage": input.video_map.stage.clone(),
            "frame_count": input.video_map.frame_count.clone(),
            "probe_status": input.video_map.probe_status.clone(),
            "event_mapping": input.video_map.event_mapping.clone(),
            "stale_reasons": input.video_map.stale_reasons,
        },
        "video": {
            "enabled": input.video.enabled,
            "required": input.video.required,
            "exists": input.video.exists,
            "stale_reasons": input.video.stale_reasons,
        },
        "note_flags": input.note_flags,
        "flags": input.flags,
        "warnings": input.warnings,
        "next_actions": input.next_actions,
    })
}

fn require_directory(dir: &Path) -> CliResult<()> {
    let metadata = fs::metadata(dir)?;
    if !metadata.is_dir() {
        return Err(CliError::new(format!(
            "recording path is not a directory: {}",
            dir.display()
        )));
    }
    Ok(())
}

fn inspect_session_state(dir: &Path) -> SessionInspection {
    let state = inspect_session_file(
        dir,
        &dir.join("session").join("state.json"),
        STATE_SCHEMA,
        session_state_summary,
        validate_session_json::<CaptureSessionState>,
    );
    let finalization = inspect_session_file(
        dir,
        &dir.join("session").join("finalization.json"),
        FINALIZATION_SCHEMA,
        finalization_summary,
        validate_session_json::<FinalizationLedger>,
    );
    let lock_snapshot = inspect_session_file(
        dir,
        &dir.join("session").join("lock.snapshot.json"),
        LOCK_SCHEMA,
        lock_snapshot_summary,
        validate_session_json::<CaptureSessionLock>,
    );
    let lifecycle_state = string_from_summary(&state.summary, "lifecycle_state");
    let finalization_run_state = string_from_summary(&finalization.summary, "run_state");
    let command = session_command_summary(&state, &lock_snapshot);
    let stale_reasons = session_stale_reasons(&state, &finalization, &lock_snapshot);
    let classification = session_classification(&SessionClassificationInput {
        state: &state,
        finalization: &finalization,
        lock_snapshot: &lock_snapshot,
        lifecycle_state: lifecycle_state.as_deref(),
        finalization_run_state: finalization_run_state.as_deref(),
        embedded_finalization_state: state
            .summary
            .get("finalization_run_state")
            .and_then(Value::as_str),
        stale_reasons: &stale_reasons,
    });
    let warnings = session_warnings(&state, &finalization, &stale_reasons);
    SessionInspection {
        state,
        finalization,
        lock_snapshot,
        classification,
        lifecycle_state,
        finalization_run_state,
        command,
        stale_reasons,
        warnings,
    }
}

fn inspect_session_file(
    dir: &Path,
    path: &Path,
    expected_schema: &str,
    summarize: fn(&Value) -> Value,
    validate: SessionJsonValidator,
) -> SessionFileInspection {
    let classified = read_json_classified(path, expected_schema);
    let present = classified.status != ReadStatus::Missing;
    let schema = classified
        .observed_schema
        .as_ref()
        .map_or(Value::Null, |schema| json!(schema));
    if classified.status != ReadStatus::Valid {
        return SessionFileInspection {
            present,
            status: read_status_text(classified.status),
            path: relative_path_text(dir, path),
            schema,
            error: classified.message,
            summary: Value::Null,
        };
    }
    let Some(value) = classified.value else {
        return SessionFileInspection {
            present,
            status: String::from("corrupt"),
            path: relative_path_text(dir, path),
            schema,
            error: Some(String::from("valid read had no JSON value")),
            summary: Value::Null,
        };
    };
    if let Err(error) = validate(value.clone()) {
        return SessionFileInspection {
            present: true,
            status: String::from("corrupt"),
            path: relative_path_text(dir, path),
            schema,
            error: Some(error.to_string()),
            summary: Value::Null,
        };
    }
    SessionFileInspection {
        present: true,
        status: String::from("valid"),
        path: relative_path_text(dir, path),
        schema,
        error: None,
        summary: summarize(&value),
    }
}

fn string_from_summary(summary: &Value, key: &str) -> Option<String> {
    summary
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn session_command_summary(
    state: &SessionFileInspection,
    lock_snapshot: &SessionFileInspection,
) -> Value {
    state
        .summary
        .get("command")
        .cloned()
        .or_else(|| lock_snapshot.summary.get("command").cloned())
        .unwrap_or(Value::Null)
}

fn validate_session_json<T>(value: Value) -> Result<(), serde_json::Error>
where
    T: DeserializeOwned,
{
    serde_json::from_value::<T>(value).map(|_parsed| ())
}

fn read_status_text(status: ReadStatus) -> String {
    match status {
        ReadStatus::Missing => "missing",
        ReadStatus::Valid => "valid",
        ReadStatus::Mismatched => "mismatched",
        ReadStatus::Corrupt => "corrupt",
        ReadStatus::UnsupportedSchema => "unsupported_schema",
        ReadStatus::Stale => "stale",
        ReadStatus::IoError => "io_error",
    }
    .to_owned()
}

fn session_state_summary(value: &Value) -> Value {
    json!({
        "run_id": value.get("run_id").cloned().unwrap_or(Value::Null),
        "package_name": value.get("package_name").cloned().unwrap_or(Value::Null),
        "device_serial": value.get("device_serial").cloned().unwrap_or(Value::Null),
        "lifecycle_state": value.pointer("/lifecycle/state").cloned().unwrap_or(Value::Null),
        "lifecycle_stage": value.pointer("/lifecycle/stage").cloned().unwrap_or(Value::Null),
        "history_count": value
            .pointer("/lifecycle/history")
            .and_then(Value::as_array)
            .map_or(Value::Null, |history| json!(history.len())),
        "command": value.pointer("/start_config/command").cloned().unwrap_or(Value::Null),
        "input": value.get("input").cloned().unwrap_or(Value::Null),
        "finalization_run_state": value.pointer("/finalization/run_state").cloned().unwrap_or(Value::Null),
        "finalization_cleanup_ok": value.pointer("/finalization/cleanup_ok").cloned().unwrap_or(Value::Null),
    })
}

fn finalization_summary(value: &Value) -> Value {
    let failure_count = value
        .get("failure_reasons")
        .and_then(Value::as_array)
        .map_or(Value::Null, |reasons| json!(reasons.len()));
    let failed_steps = value
        .get("steps")
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter(|step| step.get("status").and_then(Value::as_str) == Some("failed"))
                .filter_map(|step| step.get("name").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let failed_step_error_codes = value
        .get("steps")
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter(|step| step.get("status").and_then(Value::as_str) == Some("failed"))
                .filter_map(|step| {
                    let name = step.get("name").and_then(Value::as_str)?;
                    let error_code = step.get("error_code").and_then(Value::as_str)?;
                    Some((String::from(name), json!(error_code)))
                })
                .collect::<Map<_, _>>()
        })
        .unwrap_or_default();
    json!({
        "run_id": value.get("run_id").cloned().unwrap_or(Value::Null),
        "run_state": value.get("run_state").cloned().unwrap_or(Value::Null),
        "failure_stage": value.get("failure_stage").cloned().unwrap_or(Value::Null),
        "failure_reason_count": failure_count,
        "cleanup_attempted": value.get("cleanup_attempted").cloned().unwrap_or(Value::Null),
        "cleanup_ok": value.get("cleanup_ok").cloned().unwrap_or(Value::Null),
        "step_count": value
            .get("steps")
            .and_then(Value::as_array)
            .map_or(Value::Null, |steps| json!(steps.len())),
        "failed_steps": failed_steps,
        "failed_step_error_codes": failed_step_error_codes,
    })
}

fn lock_snapshot_summary(value: &Value) -> Value {
    json!({
        "run_id": value.get("run_id").cloned().unwrap_or(Value::Null),
        "package_name": value.get("package_name").cloned().unwrap_or(Value::Null),
        "device_serial": value.get("device_serial").cloned().unwrap_or(Value::Null),
        "lock_state": value.get("lock_state").cloned().unwrap_or(Value::Null),
        "observed_lifecycle_state": value.get("observed_lifecycle_state").cloned().unwrap_or(Value::Null),
        "command": value.get("command").cloned().unwrap_or(Value::Null),
        "mutation_seq": value.get("mutation_seq").cloned().unwrap_or(Value::Null),
    })
}

struct SessionClassificationInput<'a> {
    state: &'a SessionFileInspection,
    finalization: &'a SessionFileInspection,
    lock_snapshot: &'a SessionFileInspection,
    lifecycle_state: Option<&'a str>,
    finalization_run_state: Option<&'a str>,
    embedded_finalization_state: Option<&'a str>,
    stale_reasons: &'a [String],
}

fn session_classification(input: &SessionClassificationInput<'_>) -> SessionClassification {
    if !input.state.present {
        if input.finalization.present || input.lock_snapshot.present {
            return SessionClassification::RepairRequired;
        }
        return SessionClassification::UntrackedLegacy;
    }
    if session_needs_repair(input) {
        return SessionClassification::RepairRequired;
    }
    if session_is_complete(input) {
        return SessionClassification::Complete;
    }
    if input.lifecycle_state == Some("aborted") {
        return SessionClassification::Aborted;
    }
    if session_is_incomplete(input) {
        return SessionClassification::Incomplete;
    }
    if input.lifecycle_state == Some("active") {
        return SessionClassification::Active;
    }
    if lifecycle_is_in_progress(input.lifecycle_state) {
        return SessionClassification::InProgress;
    }
    SessionClassification::Stale
}

fn session_is_complete(input: &SessionClassificationInput<'_>) -> bool {
    input.state.status == "valid"
        && input.finalization.status == "valid"
        && input.lock_snapshot.status == "valid"
        && input.lifecycle_state == Some("complete")
        && input.finalization_run_state == Some("complete")
        && input
            .embedded_finalization_state
            .is_none_or(|state| state == "complete")
}

fn session_is_incomplete(input: &SessionClassificationInput<'_>) -> bool {
    input
        .lifecycle_state
        .is_some_and(|state| matches!(state, "incomplete" | "aborted"))
        || input
            .finalization_run_state
            .is_some_and(|state| matches!(state, "incomplete" | "aborted"))
}

fn session_needs_repair(input: &SessionClassificationInput<'_>) -> bool {
    [input.state, input.finalization, input.lock_snapshot]
        .iter()
        .any(|file| {
            matches!(
                file.status.as_str(),
                "io_error" | "corrupt" | "unsupported_schema"
            )
        })
        || input.stale_reasons.iter().any(|reason| {
            matches!(
                reason.as_str(),
                "session finalization is missing"
                    | "session lock snapshot is missing"
                    | "session finalization exists without session state"
                    | "session lock snapshot exists without session state"
                    | "session state and finalization run ids differ"
                    | "session state and lock snapshot run ids differ"
                    | "session state and lock snapshot package names differ"
                    | "session state and lock snapshot device serials differ"
                    | "embedded and standalone finalization states differ"
            )
        })
}

fn lifecycle_is_in_progress(lifecycle_state: Option<&str>) -> bool {
    lifecycle_state.is_some_and(|state| {
        matches!(
            state,
            "starting"
                | "ime_started"
                | "video_started"
                | "getevent_started"
                | "start_evidence_captured"
                | "controller_started"
                | "stop_requested"
                | "stopping"
                | "end_evidence_capturing"
                | "finalizing"
        )
    })
}

fn session_stale_reasons(
    state: &SessionFileInspection,
    finalization: &SessionFileInspection,
    lock_snapshot: &SessionFileInspection,
) -> Vec<String> {
    let mut reasons = Vec::new();
    append_session_file_reason(&mut reasons, "session state", state);
    append_session_file_reason(&mut reasons, "session finalization", finalization);
    append_session_file_reason(&mut reasons, "session lock snapshot", lock_snapshot);
    if state_lifecycle_value(state) == Some("complete") && !finalization.present {
        reasons.push(String::from("session finalization is missing"));
    }
    if state.present && !lock_snapshot.present {
        reasons.push(String::from("session lock snapshot is missing"));
    }
    if !state.present && finalization.present {
        reasons.push(String::from(
            "session finalization exists without session state",
        ));
    }
    if !state.present && lock_snapshot.present {
        reasons.push(String::from(
            "session lock snapshot exists without session state",
        ));
    }
    if state.summary.get("run_id") != finalization.summary.get("run_id")
        && state.present
        && finalization.present
    {
        reasons.push(String::from(
            "session state and finalization run ids differ",
        ));
    }
    append_summary_mismatch_reason(
        &mut reasons,
        state,
        lock_snapshot,
        "run_id",
        "session state and lock snapshot run ids differ",
    );
    append_summary_mismatch_reason(
        &mut reasons,
        state,
        lock_snapshot,
        "package_name",
        "session state and lock snapshot package names differ",
    );
    append_summary_mismatch_reason(
        &mut reasons,
        state,
        lock_snapshot,
        "device_serial",
        "session state and lock snapshot device serials differ",
    );
    if let (Some(state_value), Some(finalization_value)) = (
        state.summary.get("finalization_run_state"),
        finalization.summary.get("run_state"),
    ) {
        if !state_value.is_null()
            && !finalization_value.is_null()
            && state_value != finalization_value
        {
            reasons.push(String::from(
                "embedded and standalone finalization states differ",
            ));
        }
    }
    reasons
}

fn session_lifecycle_is_terminal(state: &SessionFileInspection) -> bool {
    state_lifecycle_value(state)
        .is_some_and(|lifecycle| matches!(lifecycle, "complete" | "incomplete" | "aborted"))
}

fn state_lifecycle_value(state: &SessionFileInspection) -> Option<&str> {
    state.summary.get("lifecycle_state").and_then(Value::as_str)
}

fn append_summary_mismatch_reason(
    reasons: &mut Vec<String>,
    left: &SessionFileInspection,
    right: &SessionFileInspection,
    key: &str,
    reason: &str,
) {
    if !left.present || !right.present {
        return;
    }
    let left_value = left.summary.get(key).unwrap_or(&Value::Null);
    let right_value = right.summary.get(key).unwrap_or(&Value::Null);
    if !left_value.is_null() && !right_value.is_null() && left_value != right_value {
        reasons.push(String::from(reason));
    }
}

fn append_session_file_reason(
    reasons: &mut Vec<String>,
    label: &str,
    file: &SessionFileInspection,
) {
    match file.status.as_str() {
        "missing" | "valid" => {}
        status => reasons.push(format!("{label} status is {status}")),
    }
}

fn session_warnings(
    state: &SessionFileInspection,
    finalization: &SessionFileInspection,
    stale_reasons: &[String],
) -> Vec<String> {
    let mut warnings = stale_reasons.to_vec();
    if state.summary.get("lifecycle_state").and_then(Value::as_str) == Some("incomplete") {
        warnings.push(String::from("umbrella session lifecycle is incomplete"));
    }
    if state.summary.get("lifecycle_state").and_then(Value::as_str) == Some("aborted") {
        warnings.push(String::from("umbrella session lifecycle is aborted"));
    }
    if finalization
        .summary
        .get("run_state")
        .and_then(Value::as_str)
        == Some("incomplete")
    {
        warnings.push(String::from("umbrella session finalization is incomplete"));
    }
    warnings
}

fn inspect_validation(
    dir: &Path,
    validation_path: &Path,
    external_run_id: Option<&str>,
) -> ValidationInspection {
    let stored = read_optional_json(validation_path).unwrap_or(None);
    let current = validate_logs(&dir.join("ime"), external_run_id).unwrap_or_else(|error| {
        json!({
            "ok": false,
            "error": error.to_string(),
        })
    });
    let stale_reasons = stored.as_ref().map_or_else(
        || vec![String::from("validation.json is missing")],
        |stored_value| validation_stale_reasons(stored_value, &current),
    );
    ValidationInspection {
        current_ok: current.get("ok").and_then(Value::as_bool).unwrap_or(false),
        stored_present: stored.is_some(),
        current,
        stored: stored.unwrap_or(Value::Null),
        stale_reasons,
    }
}

fn inspect_run_summary(dir: &Path) -> CliResult<RunSummaryInspection> {
    let summary_path = dir.join("derived").join("run_summary.json");
    if !summary_path.exists() {
        return Ok(RunSummaryInspection {
            exists: false,
            stale_reasons: vec![String::from("run summary is missing")],
        });
    }
    let Some(summary) = read_optional_json(&summary_path)? else {
        return Ok(RunSummaryInspection {
            exists: false,
            stale_reasons: vec![String::from("run summary is missing")],
        });
    };
    let mut stale_reasons = Vec::new();
    if summary.get("schema").and_then(Value::as_str) != Some("input_dynamics_run_summary.v1") {
        stale_reasons.push(String::from("run summary schema is unsupported"));
    }
    stale_reasons.extend(run_summary_source_stale_reasons(dir, &summary)?);
    Ok(RunSummaryInspection {
        exists: true,
        stale_reasons,
    })
}

fn run_summary_source_stale_reasons(dir: &Path, summary: &Value) -> CliResult<Vec<String>> {
    let mut reasons = Vec::new();
    let Some(path_text_value) = summary.pointer("/source_ref/path").and_then(Value::as_str) else {
        reasons.push(String::from("run summary has no source path"));
        return Ok(reasons);
    };
    let source_path = source_path(dir, path_text_value);
    if !source_path.exists() {
        reasons.push(format!("run summary source is missing: {path_text_value}"));
        return Ok(reasons);
    }
    let current = file_fingerprint(&source_path)?;
    let recorded_sha = summary
        .pointer("/source_ref/fingerprint/sha256")
        .and_then(Value::as_str);
    let current_sha = current.get("sha256").and_then(Value::as_str);
    if recorded_sha.is_some() && recorded_sha != current_sha {
        reasons.push(format!(
            "run summary source fingerprint changed: {path_text_value}"
        ));
    }
    let recorded_count = summary
        .pointer("/source_ref/record_count")
        .and_then(Value::as_u64);
    let current_count = count_nonempty_lines(&source_path)?;
    if recorded_count.is_some() && recorded_count != Some(current_count) {
        reasons.push(format!(
            "run summary source record count changed: {path_text_value}"
        ));
    }
    Ok(reasons)
}

fn source_path(dir: &Path, path_text_value: &str) -> PathBuf {
    let path = PathBuf::from(path_text_value);
    if path.is_absolute() {
        path
    } else {
        dir.join(path)
    }
}

fn inspect_timeline(dir: &Path) -> CliResult<TimelineInspection> {
    let index_path = dir.join("derived").join("timeline").join("index.json");
    let events_path = dir.join("derived").join("timeline").join("events.jsonl");
    if !index_path.exists() || !events_path.exists() {
        return Ok(TimelineInspection {
            exists: false,
            stale_reasons: vec![String::from("timeline bundle is missing")],
            artifact_diagnostics: Value::Null,
        });
    }
    let Some(index) = read_optional_json(&index_path)? else {
        return Ok(TimelineInspection {
            exists: false,
            stale_reasons: vec![String::from("timeline index is missing")],
            artifact_diagnostics: Value::Null,
        });
    };
    let mut stale_reasons = Vec::new();
    if let Some(sources) = index.get("sources").and_then(Value::as_array) {
        for source in sources {
            stale_reasons.extend(timeline_source_stale_reasons(dir, source)?);
        }
    } else {
        stale_reasons.push(String::from("timeline index has no sources array"));
    }
    Ok(TimelineInspection {
        exists: true,
        stale_reasons,
        artifact_diagnostics: index
            .get("artifact_diagnostics")
            .cloned()
            .unwrap_or(Value::Null),
    })
}

fn inspect_video(
    dir: &Path,
    manifest: Option<&Value>,
    artifacts: &Value,
) -> CliResult<VideoInspection> {
    let enabled = bool_at_manifest(manifest, "/video/enabled");
    let required = bool_at_manifest(manifest, "/video/required");
    let screen_exists = artifact_exists(artifacts, "video_screen");
    let timing_exists = artifact_exists(artifacts, "video_timing");
    let exists = screen_exists && timing_exists;
    let mut stale_reasons = Vec::new();
    if required && !screen_exists {
        stale_reasons.push(String::from("required video file is missing"));
    }
    if required && !timing_exists {
        stale_reasons.push(String::from("required video timing metadata is missing"));
    }
    if timing_exists {
        let timing_path = dir.join("video").join("timing.json");
        let Some(timing) = read_optional_json(&timing_path)? else {
            stale_reasons.push(String::from("video timing metadata is unreadable"));
            return Ok(VideoInspection {
                stale_reasons,
                enabled,
                required,
                exists,
            });
        };
        if timing.get("schema").and_then(Value::as_str) != Some("input_dynamics_video_capture.v1")
            && !legacy_video_timing(&timing)
        {
            stale_reasons.push(String::from("video timing metadata schema is unsupported"));
        }
    }
    if screen_exists {
        let current = file_fingerprint(&dir.join("video").join("screen.mp4"))?;
        let recorded_sha = manifest
            .and_then(|value| value.pointer("/video/file/sha256"))
            .and_then(Value::as_str);
        let current_sha = current.get("sha256").and_then(Value::as_str);
        if recorded_sha.is_some() && recorded_sha != current_sha {
            stale_reasons.push(String::from("video file fingerprint changed"));
        }
    }
    Ok(VideoInspection {
        stale_reasons,
        enabled,
        required,
        exists,
    })
}

#[allow(clippy::too_many_lines)]
fn inspect_video_map(dir: &Path) -> CliResult<VideoMapInspection> {
    let index_path = dir.join("derived").join("video_map").join("index.json");
    let frames_path = dir.join("derived").join("video_map").join("frames.jsonl");
    let alignment_path = dir.join("derived").join("video_map").join("alignment.json");
    let event_frames_path = dir
        .join("derived")
        .join("video_map")
        .join("event_frames.jsonl");
    let index_exists = index_path.exists();
    let frames_exists = frames_path.exists();
    if !index_exists && !frames_exists {
        return Ok(VideoMapInspection {
            exists: false,
            frame_index_exists: false,
            event_map_exists: false,
            stale_reasons: vec![String::from("video frame index is missing")],
            ..VideoMapInspection::default()
        });
    }
    let mut stale_reasons = Vec::new();
    if !index_exists {
        stale_reasons.push(String::from("video map index is missing"));
    }
    if !frames_exists {
        stale_reasons.push(String::from("video map frames JSONL is missing"));
    }
    let Some(index) = read_optional_json(&index_path)? else {
        stale_reasons.push(String::from("video map index is unreadable"));
        return Ok(VideoMapInspection {
            stale_reasons,
            exists: index_exists && frames_exists,
            frame_index_exists: false,
            event_map_exists: false,
            ..VideoMapInspection::default()
        });
    };
    let mut frame_index_reasons = Vec::new();
    let mut event_map_reasons = Vec::new();
    if index.get("schema").and_then(Value::as_str) != Some("input_dynamics_video_map_index.v1") {
        let reason = String::from("video map index schema is unsupported");
        stale_reasons.push(reason.clone());
        frame_index_reasons.push(reason.clone());
        event_map_reasons.push(reason);
    }
    let stage = index.get("artifact_stage").and_then(Value::as_str);
    let stage_is_frame_index = stage == Some("frame_index");
    let stage_is_event_frame_map = stage == Some("event_frame_map");
    if !stage_is_frame_index && !stage_is_event_frame_map {
        let reason = String::from("video map index artifact stage is unsupported");
        stale_reasons.push(reason.clone());
        frame_index_reasons.push(reason.clone());
        event_map_reasons.push(reason);
    }
    if let Some(sources) = index.get("sources").and_then(Value::as_array) {
        for source in sources {
            let source_reasons = video_map_source_stale_reasons(dir, source)?;
            let kind = source
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown_source");
            if matches!(kind, "manifest" | "video_screen" | "video_timing") {
                frame_index_reasons.extend(source_reasons.clone());
            }
            event_map_reasons.extend(source_reasons.clone());
            stale_reasons.extend(source_reasons);
        }
    } else {
        let reason = String::from("video map index has no sources array");
        stale_reasons.push(reason.clone());
        frame_index_reasons.push(reason.clone());
        event_map_reasons.push(reason);
    }
    let index_output_reasons = video_map_index_output_stale_reasons(dir, &index, &index_path);
    frame_index_reasons.extend(index_output_reasons.clone());
    event_map_reasons.extend(index_output_reasons.clone());
    stale_reasons.extend(index_output_reasons);
    let frame_source_reasons = required_video_map_source_reasons(
        &index,
        &[
            ("manifest", "manifest.json"),
            ("video_screen", "video/screen.mp4"),
            ("video_timing", "video/timing.json"),
        ],
    );
    frame_index_reasons.extend(frame_source_reasons.clone());
    event_map_reasons.extend(frame_source_reasons.clone());
    stale_reasons.extend(frame_source_reasons);
    if stage_is_event_frame_map {
        let event_source_reasons = required_video_map_source_reasons(
            &index,
            &[
                ("timeline_index", "derived/timeline/index.json"),
                ("timeline_events", "derived/timeline/events.jsonl"),
            ],
        );
        event_map_reasons.extend(event_source_reasons.clone());
        stale_reasons.extend(event_source_reasons);
    }
    if stage_is_frame_index {
        event_map_reasons.push(String::from("video event-frame map is missing"));
    }
    let frames_reasons = video_map_output_stale_reasons(
        dir,
        &index,
        &VideoMapOutputCheck {
            expected_path: &frames_path,
            output_pointer: "/outputs/video_map_frames",
            expected_schema: "input_dynamics_video_frame.v1",
            description: "video map frames",
        },
    )?;
    frame_index_reasons.extend(frames_reasons.clone());
    event_map_reasons.extend(frames_reasons.clone());
    stale_reasons.extend(frames_reasons);
    if stage_is_event_frame_map {
        let alignment_reasons = video_map_output_stale_reasons(
            dir,
            &index,
            &VideoMapOutputCheck {
                expected_path: &alignment_path,
                output_pointer: "/outputs/video_map_alignment",
                expected_schema: "input_dynamics_video_alignment.v1",
                description: "video map alignment",
            },
        )?;
        event_map_reasons.extend(alignment_reasons.clone());
        stale_reasons.extend(alignment_reasons);
        let event_frame_reasons = video_map_output_stale_reasons(
            dir,
            &index,
            &VideoMapOutputCheck {
                expected_path: &event_frames_path,
                output_pointer: "/outputs/video_map_event_frames",
                expected_schema: "input_dynamics_event_video_frame_map.v1",
                description: "video map event frames",
            },
        )?;
        event_map_reasons.extend(event_frame_reasons.clone());
        stale_reasons.extend(event_frame_reasons);
    }
    let frame_index_exists = index_exists
        && frames_exists
        && (stage_is_frame_index || stage_is_event_frame_map)
        && frame_index_reasons.is_empty();
    let event_map_exists = index_exists
        && frames_exists
        && alignment_path.exists()
        && event_frames_path.exists()
        && stage_is_event_frame_map
        && frame_index_reasons.is_empty()
        && event_map_reasons.is_empty();
    Ok(VideoMapInspection {
        stage: index.get("artifact_stage").cloned().unwrap_or(Value::Null),
        frame_count: index.get("frame_count").cloned().unwrap_or(Value::Null),
        probe_status: index.get("probe_status").cloned().unwrap_or(Value::Null),
        event_mapping: index.get("event_mapping").cloned().unwrap_or(Value::Null),
        stale_reasons,
        exists: index_exists && frames_exists,
        frame_index_exists,
        event_map_exists,
    })
}

fn video_map_output_stale_reasons(
    dir: &Path,
    index: &Value,
    check: &VideoMapOutputCheck<'_>,
) -> CliResult<Vec<String>> {
    let mut reasons = Vec::new();
    let Some(output) = index.pointer(check.output_pointer) else {
        reasons.push(format!(
            "video map index has no {} output metadata",
            check.description
        ));
        return Ok(reasons);
    };
    if output.get("schema").and_then(Value::as_str) != Some(check.expected_schema) {
        reasons.push(format!(
            "{} output schema is unsupported",
            check.description
        ));
    }
    let Some(path_text_value) = output.get("path").and_then(Value::as_str) else {
        reasons.push(format!("{} output has no path", check.description));
        return Ok(reasons);
    };
    let recorded_output_path = source_path(dir, path_text_value);
    if recorded_output_path != check.expected_path {
        reasons.push(format!("{} output path changed", check.description));
    }
    let Some(recorded_count) = output.get("record_count").and_then(Value::as_u64) else {
        reasons.push(format!("{} output has no record count", check.description));
        return Ok(reasons);
    };
    let Some(recorded_sha) = output
        .pointer("/fingerprint/sha256")
        .and_then(Value::as_str)
    else {
        reasons.push(format!("{} output has no fingerprint", check.description));
        return Ok(reasons);
    };
    if !check.expected_path.exists() {
        reasons.push(format!("{} output is missing", check.description));
        return Ok(reasons);
    }
    let current = file_fingerprint(check.expected_path)?;
    let current_sha = current.get("sha256").and_then(Value::as_str);
    if Some(recorded_sha) != current_sha {
        reasons.push(format!("{} fingerprint changed", check.description));
    }
    let current_count = if check
        .expected_path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        1_u64
    } else {
        count_nonempty_lines(check.expected_path)?
    };
    if recorded_count != current_count {
        reasons.push(format!("{} record count changed", check.description));
    }
    Ok(reasons)
}

fn video_map_index_output_stale_reasons(
    dir: &Path,
    index: &Value,
    index_path: &Path,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let Some(output) = index.pointer("/outputs/video_map_index") else {
        reasons.push(String::from(
            "video map index has no video map index output metadata",
        ));
        return reasons;
    };
    if output.get("schema").and_then(Value::as_str) != Some("input_dynamics_video_map_index.v1") {
        reasons.push(String::from("video map index output schema is unsupported"));
    }
    let Some(path_text_value) = output.get("path").and_then(Value::as_str) else {
        reasons.push(String::from("video map index output has no path"));
        return reasons;
    };
    if source_path(dir, path_text_value) != index_path {
        reasons.push(String::from("video map index output path changed"));
    }
    if !output.get("record_count").is_some_and(Value::is_null) {
        reasons.push(String::from(
            "video map index output record count must be null",
        ));
    }
    if !output
        .get("sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        reasons.push(String::from(
            "video map index output is not marked sensitive",
        ));
    }
    if !output.get("fingerprint").is_some_and(Value::is_null) {
        reasons.push(String::from(
            "video map index output fingerprint must be null",
        ));
    }
    if output.get("fingerprint_status").and_then(Value::as_str)
        != Some("not_embedded_self_reference")
    {
        reasons.push(String::from(
            "video map index output fingerprint status is unsupported",
        ));
    }
    reasons
}

fn required_video_map_source_reasons(index: &Value, required: &[(&str, &str)]) -> Vec<String> {
    let Some(sources) = index.get("sources").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut reasons = Vec::new();
    for &(kind, expected_path) in required {
        let matches = sources
            .iter()
            .filter(|source| source.get("kind").and_then(Value::as_str) == Some(kind))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            reasons.push(format!("video map index is missing required {kind} source"));
            continue;
        }
        if matches.len() > 1_usize {
            reasons.push(format!(
                "video map index has duplicate required {kind} sources"
            ));
        }
        for source in matches {
            if source.get("path").and_then(Value::as_str) != Some(expected_path) {
                reasons.push(format!("video map {kind} source path changed"));
            }
        }
    }
    reasons
}

fn video_map_source_stale_reasons(dir: &Path, source: &Value) -> CliResult<Vec<String>> {
    let mut reasons = Vec::new();
    let kind = source
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown_source");
    let Some(path_text_value) = source.get("path").and_then(Value::as_str) else {
        reasons.push(format!("{kind} has no source path"));
        return Ok(reasons);
    };
    let path = source_path(dir, path_text_value);
    if !path.exists() {
        reasons.push(format!("{kind} source is missing: {path_text_value}"));
        return Ok(reasons);
    }
    let Some(recorded_sha) = source
        .pointer("/fingerprint/sha256")
        .and_then(Value::as_str)
    else {
        reasons.push(format!(
            "{kind} source has no fingerprint: {path_text_value}"
        ));
        return Ok(reasons);
    };
    let current = file_fingerprint(&path)?;
    let current_sha = current.get("sha256").and_then(Value::as_str);
    if Some(recorded_sha) != current_sha {
        reasons.push(format!(
            "{kind} source fingerprint changed: {path_text_value}"
        ));
    }
    if let Some(recorded_count) = source.get("record_count").and_then(Value::as_u64) {
        let current_count = count_nonempty_lines(&path)?;
        if recorded_count != current_count {
            reasons.push(format!(
                "{kind} source record count changed: {path_text_value}"
            ));
        }
    }
    Ok(reasons)
}

fn inspect_clock(
    dir: &Path,
    manifest: Option<&Value>,
    timeline: &TimelineInspection,
    video: &VideoInspection,
) -> CliResult<ClockInspection> {
    let video_clock = inspect_video_clock(dir, video)?;
    let evidence_clock = inspect_evidence_clock(dir, manifest)?;
    let timeline_status = timeline_clock_status(timeline);
    let canonical_clock_ready = video_clock.readiness.is_canonical()
        && (!evidence_clock.requested || evidence_clock.readiness.is_canonical());
    let has_legacy_timing =
        video_clock.readiness.is_legacy() || evidence_clock.readiness.is_legacy();
    let video_needs_canonical = video.required && !video_clock.readiness.is_canonical();
    let evidence_needs_canonical =
        evidence_clock.requested && !evidence_clock.readiness.is_canonical();
    let needs_canonical_recording =
        video_needs_canonical || evidence_needs_canonical || has_legacy_timing;
    let mut warnings = Vec::new();
    warnings.extend(video_clock.warnings.clone());
    warnings.extend(evidence_clock.warnings.clone());
    if !timeline.stale_reasons.is_empty() {
        warnings.extend(timeline.stale_reasons.iter().cloned());
    }
    let overall_status =
        overall_clock_readiness(video_clock.readiness, evidence_clock.readiness).status();
    let value = json!({
        "canonical_clock_ready": canonical_clock_ready,
        "overall_status": overall_status,
        "legacy_timing": has_legacy_timing,
        "stale_inputs": video_clock.readiness.is_stale_inputs() || evidence_clock.readiness.is_stale_inputs(),
        "missing_sources": merge_arrays(&video_clock.missing_sources, &evidence_clock.missing_sources),
        "video_clock_status": video_clock.readiness.status(),
        "video_clock_canonical": video_clock.readiness.is_canonical(),
        "timeline_clock_alignment_status": AlignmentStatus::NotEstimated.as_str(),
        "getevent_clock_status": AlignmentStatus::UnsupportedClockDomain.as_str(),
        "video": video_clock.value,
        "evidence": evidence_clock.value,
        "timeline": {
            "exists": timeline.exists,
            "status": timeline_status,
            "clock_alignment_status": AlignmentStatus::NotEstimated.as_str(),
            "artifact_diagnostics": timeline.artifact_diagnostics.clone(),
            "stale_reasons": timeline.stale_reasons,
        },
        "warnings": warnings,
    });
    Ok(ClockInspection {
        value,
        canonical_clock_ready,
        has_legacy_timing,
        needs_canonical_recording,
        warnings,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClockReadiness {
    Canonical,
    Legacy,
    MissingSource,
    ProbeFailed,
    StaleInputs,
    NotRequested,
    NotEstimated,
}

impl ClockReadiness {
    const fn status(self) -> &'static str {
        match self {
            Self::Canonical => AlignmentStatus::Bracketed.as_str(),
            Self::Legacy => AlignmentStatus::LegacyWallClockBracketed.as_str(),
            Self::MissingSource => AlignmentStatus::MissingSource.as_str(),
            Self::ProbeFailed => AlignmentStatus::ProbeFailed.as_str(),
            Self::StaleInputs => AlignmentStatus::StaleInputs.as_str(),
            Self::NotRequested => "not_requested",
            Self::NotEstimated => AlignmentStatus::NotEstimated.as_str(),
        }
    }

    const fn is_canonical(self) -> bool {
        matches!(self, Self::Canonical)
    }

    const fn is_legacy(self) -> bool {
        matches!(self, Self::Legacy)
    }

    const fn is_stale_inputs(self) -> bool {
        matches!(self, Self::StaleInputs)
    }

    const fn clock_domain(self) -> Option<&'static str> {
        if self.is_canonical() {
            Some(ClockDomain::DeviceElapsedRealtimeNs.as_str())
        } else {
            None
        }
    }
}

#[derive(Default)]
struct ClockDetails {
    stale_reasons: Vec<String>,
    legacy_reasons: Vec<String>,
    missing_sources: Vec<String>,
    reasons: Vec<String>,
}

impl ClockDetails {
    const fn from_stale_reasons(stale_reasons: Vec<String>) -> Self {
        Self {
            stale_reasons,
            legacy_reasons: Vec::new(),
            missing_sources: Vec::new(),
            reasons: Vec::new(),
        }
    }

    fn merge(first: &Self, second: &Self) -> Self {
        Self {
            stale_reasons: merge_arrays(&first.stale_reasons, &second.stale_reasons),
            legacy_reasons: merge_arrays(&first.legacy_reasons, &second.legacy_reasons),
            missing_sources: merge_arrays(&first.missing_sources, &second.missing_sources),
            reasons: merge_arrays(&first.reasons, &second.reasons),
        }
    }

    fn warnings(&self) -> Vec<String> {
        let first = merge_arrays(&self.stale_reasons, &self.legacy_reasons);
        let second = merge_arrays(&self.missing_sources, &self.reasons);
        merge_arrays(&first, &second)
    }
}

struct ClockSourceInspection {
    value: Value,
    readiness: ClockReadiness,
    requested: bool,
    missing_sources: Vec<String>,
    warnings: Vec<String>,
}

fn inspect_video_clock(dir: &Path, video: &VideoInspection) -> CliResult<ClockSourceInspection> {
    if !video.enabled && !video.required {
        return Ok(video_clock_value(
            video,
            ClockReadiness::NotRequested,
            ClockDetails::default(),
        ));
    }

    let timing_path = dir.join("video").join("timing.json");
    let Some(timing) = read_optional_json(&timing_path)? else {
        let mut details = ClockDetails::from_stale_reasons(video.stale_reasons.clone());
        details
            .missing_sources
            .push(String::from("video/timing.json"));
        return Ok(video_clock_value(
            video,
            ClockReadiness::MissingSource,
            details,
        ));
    };

    let mut details = ClockDetails::from_stale_reasons(video.stale_reasons.clone());
    if let Some(reason) = timing_file_stale_reason(dir, &timing)? {
        details.stale_reasons.push(reason);
    }
    let readiness = video_marker_readiness(&timing, &mut details);
    Ok(video_clock_value(video, readiness, details))
}

fn video_marker_readiness(timing: &Value, details: &mut ClockDetails) -> ClockReadiness {
    let marker_result = classify_marker_set(
        timing,
        &[
            ("/start/before", "video start before"),
            ("/start/after", "video start after"),
            ("/stop/before", "video stop before"),
            ("/stop/after", "video stop after"),
        ],
        &[
            ("/start/before", "/start/after"),
            ("/start/after", "/stop/before"),
            ("/stop/before", "/stop/after"),
        ],
    );
    match marker_result {
        MarkerSetStatus::Canonical if details.stale_reasons.is_empty() => ClockReadiness::Canonical,
        MarkerSetStatus::Canonical => {
            details.reasons.push(String::from(
                "canonical video markers are present but inputs are stale",
            ));
            ClockReadiness::StaleInputs
        }
        MarkerSetStatus::Missing(missing) if legacy_video_timing(timing) => {
            details.legacy_reasons.push(format!(
                "video timing uses legacy wall-clock/device-epoch fields; missing canonical markers: {}",
                missing.join(", ")
            ));
            ClockReadiness::Legacy
        }
        MarkerSetStatus::Missing(missing) => {
            details.missing_sources.extend(missing);
            ClockReadiness::MissingSource
        }
        MarkerSetStatus::Invalid(errors) => {
            if legacy_video_timing(timing) && !video_has_probe_marker_candidate(timing) {
                details.legacy_reasons.push(String::from(
                    "video timing uses nested legacy wall-clock/device timing without canonical probe wrappers",
                ));
                return ClockReadiness::Legacy;
            }
            details.reasons.extend(errors);
            ClockReadiness::ProbeFailed
        }
    }
}

fn video_clock_value(
    video: &VideoInspection,
    readiness: ClockReadiness,
    details: ClockDetails,
) -> ClockSourceInspection {
    let warnings = details.warnings();
    ClockSourceInspection {
        value: json!({
            "required": video.required,
            "enabled": video.enabled,
            "exists": video.exists,
            "requested": video.enabled || video.required,
            "status": readiness.status(),
            "canonical": readiness.is_canonical(),
            "alignment_status": readiness.status(),
            "clock_domain": readiness.clock_domain(),
            "source_path": "video/timing.json",
            "stale_reasons": details.stale_reasons,
            "legacy_reasons": details.legacy_reasons,
            "missing_sources": details.missing_sources,
            "reasons": details.reasons,
            "phases": Value::Null,
        }),
        readiness,
        requested: video.enabled || video.required,
        missing_sources: details.missing_sources,
        warnings,
    }
}

fn inspect_evidence_clock(
    dir: &Path,
    manifest: Option<&Value>,
) -> CliResult<ClockSourceInspection> {
    let enabled = bool_at_manifest(manifest, "/evidence/enabled");
    let policy = string_at(manifest, "/evidence/policy").unwrap_or_else(|| String::from("none"));
    if !enabled {
        let empty_phase = Value::Null;
        return Ok(evidence_clock_value(EvidenceClockValue {
            requested: false,
            policy: &policy,
            readiness: ClockReadiness::NotRequested,
            start: &empty_phase,
            end: &empty_phase,
            details: ClockDetails::default(),
        }));
    }

    let start = evidence_phase_clock(dir, manifest, "start")?;
    let end = evidence_phase_clock(dir, manifest, "end")?;
    let mut details = ClockDetails::merge(&start.details, &end.details);
    let readiness = evidence_readiness(manifest, &start, &end, &mut details);
    Ok(evidence_clock_value(EvidenceClockValue {
        requested: true,
        policy: &policy,
        readiness,
        start: &start.value,
        end: &end.value,
        details,
    }))
}

fn evidence_readiness(
    manifest: Option<&Value>,
    start: &EvidencePhaseInspection,
    end: &EvidencePhaseInspection,
    details: &mut ClockDetails,
) -> ClockReadiness {
    if start.readiness.is_canonical() && end.readiness.is_canonical() {
        let start_after = manifest.and_then(|value| value.pointer("/evidence/start/after"));
        let end_before = manifest.and_then(|value| value.pointer("/evidence/end/before"));
        if let (Some(previous), Some(next)) = (start_after, end_before) {
            if let Err(error) = validate_probe_order(previous, next) {
                details.reasons.push(format!(
                    "evidence start/end probe order is invalid: {error}"
                ));
                return ClockReadiness::ProbeFailed;
            }
        }
        return ClockReadiness::Canonical;
    }
    first_noncanonical_readiness(start.readiness, end.readiness)
}

fn first_noncanonical_readiness(first: ClockReadiness, second: ClockReadiness) -> ClockReadiness {
    for readiness in [
        ClockReadiness::StaleInputs,
        ClockReadiness::ProbeFailed,
        ClockReadiness::MissingSource,
        ClockReadiness::Legacy,
    ] {
        if first == readiness || second == readiness {
            return readiness;
        }
    }
    ClockReadiness::NotEstimated
}

struct EvidenceClockValue<'a> {
    requested: bool,
    policy: &'a str,
    readiness: ClockReadiness,
    start: &'a Value,
    end: &'a Value,
    details: ClockDetails,
}

fn evidence_clock_value(input: EvidenceClockValue<'_>) -> ClockSourceInspection {
    let requested = input.requested;
    let policy = input.policy;
    let readiness = input.readiness;
    let details = input.details;
    let warnings = details.warnings();
    ClockSourceInspection {
        value: json!({
            "requested": requested,
            "enabled": requested,
            "policy": policy,
            "status": readiness.status(),
            "canonical": readiness.is_canonical(),
            "clock_domain": readiness.clock_domain(),
            "start": input.start,
            "end": input.end,
            "stale_reasons": details.stale_reasons,
            "legacy_reasons": details.legacy_reasons,
            "missing_sources": details.missing_sources,
            "reasons": details.reasons,
        }),
        readiness,
        requested,
        missing_sources: details.missing_sources,
        warnings,
    }
}

struct EvidencePhaseInspection {
    value: Value,
    readiness: ClockReadiness,
    details: ClockDetails,
}

fn evidence_phase_clock(
    dir: &Path,
    manifest: Option<&Value>,
    phase: &str,
) -> CliResult<EvidencePhaseInspection> {
    let index_path = format!("evidence/{phase}/index.json");
    let source_path = format!("manifest.json#/evidence/{phase}");
    let index = read_optional_json(&dir.join(&index_path))?;
    let phase_pointer = format!("/evidence/{phase}");
    let Some(phase_value) = manifest.and_then(|value| value.pointer(&phase_pointer)) else {
        let mut details = ClockDetails::default();
        details
            .missing_sources
            .push(format!("manifest.json{phase_pointer}"));
        return Ok(evidence_phase_value(
            phase,
            &source_path,
            &index_path,
            ClockReadiness::MissingSource,
            details,
        ));
    };

    let mut details = ClockDetails::default();
    if index.is_none() {
        details.missing_sources.push(index_path.clone());
    }
    let readiness = evidence_phase_readiness(phase, phase_value, index.as_ref(), &mut details);
    Ok(evidence_phase_value(
        phase,
        &source_path,
        &index_path,
        readiness,
        details,
    ))
}

fn evidence_phase_readiness(
    phase: &str,
    phase_value: &Value,
    index: Option<&Value>,
    details: &mut ClockDetails,
) -> ClockReadiness {
    let marker_result = classify_marker_set(
        phase_value,
        &[("/before", "evidence before"), ("/after", "evidence after")],
        &[("/before", "/after")],
    );
    match marker_result {
        MarkerSetStatus::Canonical if details.missing_sources.is_empty() => {
            ClockReadiness::Canonical
        }
        MarkerSetStatus::Canonical => {
            details.reasons.push(format!(
                "evidence {phase} markers are present but the index artifact is missing"
            ));
            ClockReadiness::MissingSource
        }
        MarkerSetStatus::Missing(missing) if index_has_legacy_wall_time(index) => {
            details.legacy_reasons.push(format!(
                "evidence {phase} uses legacy captured_wall_ms metadata; missing canonical markers: {}",
                missing.join(", ")
            ));
            ClockReadiness::Legacy
        }
        MarkerSetStatus::Missing(missing) => {
            details.missing_sources.extend(missing);
            ClockReadiness::MissingSource
        }
        MarkerSetStatus::Invalid(errors) => {
            details.reasons.extend(errors);
            ClockReadiness::ProbeFailed
        }
    }
}

fn evidence_phase_value(
    phase: &str,
    source_path: &str,
    index_path: &str,
    readiness: ClockReadiness,
    details: ClockDetails,
) -> EvidencePhaseInspection {
    EvidencePhaseInspection {
        value: json!({
            "phase": phase,
            "source_path": source_path,
            "evidence_index_path": index_path,
            "status": readiness.status(),
            "canonical": readiness.is_canonical(),
            "clock_domain": readiness.clock_domain(),
            "stale_reasons": details.stale_reasons,
            "legacy_reasons": details.legacy_reasons,
            "missing_sources": details.missing_sources,
            "reasons": details.reasons,
        }),
        readiness,
        details,
    }
}

enum MarkerSetStatus {
    Canonical,
    Missing(Vec<String>),
    Invalid(Vec<String>),
}

fn classify_marker_set(
    value: &Value,
    markers: &[(&str, &str)],
    orders: &[(&str, &str)],
) -> MarkerSetStatus {
    let mut missing = Vec::new();
    let mut errors = Vec::new();
    for &(pointer, label) in markers {
        let Some(marker) = value.pointer(pointer) else {
            missing.push(pointer.to_owned());
            continue;
        };
        if let Err(error) = validate_probe_marker(marker, label) {
            errors.push(format!("{label} is invalid: {error}"));
        }
    }
    if !errors.is_empty() {
        return MarkerSetStatus::Invalid(errors);
    }
    if !missing.is_empty() {
        return MarkerSetStatus::Missing(missing);
    }
    for &(previous_pointer, next_pointer) in orders {
        let Some(previous) = value.pointer(previous_pointer) else {
            missing.push(previous_pointer.to_owned());
            continue;
        };
        let Some(next) = value.pointer(next_pointer) else {
            missing.push(next_pointer.to_owned());
            continue;
        };
        if let Err(error) = validate_probe_order(previous, next) {
            errors.push(format!(
                "probe order {previous_pointer} <= {next_pointer} is invalid: {error}"
            ));
        }
    }
    if !missing.is_empty() {
        return MarkerSetStatus::Missing(missing);
    }
    if !errors.is_empty() {
        return MarkerSetStatus::Invalid(errors);
    }
    MarkerSetStatus::Canonical
}

const fn overall_clock_readiness(
    video: ClockReadiness,
    evidence: ClockReadiness,
) -> ClockReadiness {
    if video.is_canonical()
        && (evidence.is_canonical() || matches!(evidence, ClockReadiness::NotRequested))
    {
        ClockReadiness::Canonical
    } else if video.is_stale_inputs() || evidence.is_stale_inputs() {
        ClockReadiness::StaleInputs
    } else if matches!(video, ClockReadiness::ProbeFailed)
        || matches!(evidence, ClockReadiness::ProbeFailed)
    {
        ClockReadiness::ProbeFailed
    } else if matches!(video, ClockReadiness::MissingSource)
        || matches!(evidence, ClockReadiness::MissingSource)
    {
        ClockReadiness::MissingSource
    } else if video.is_legacy() || evidence.is_legacy() {
        ClockReadiness::Legacy
    } else {
        ClockReadiness::NotEstimated
    }
}

fn legacy_video_timing(timing: &Value) -> bool {
    timing.pointer("/start/device_epoch_ms").is_some()
        || timing.pointer("/stop/device_epoch_ms").is_some()
        || timing
            .pointer("/start/host_wall_ms_before_device_timestamp")
            .is_some()
        || timing
            .pointer("/stop/host_wall_ms_before_device_timestamp")
            .is_some()
        || timing
            .pointer("/start/before/host_wall_ms_before_device_timestamp")
            .is_some()
        || timing.pointer("/start/after/device_wall_ms").is_some()
        || timing
            .pointer("/stop/before/host_wall_ms_before_device_timestamp")
            .is_some()
        || timing.pointer("/stop/after/device_wall_ms").is_some()
}

fn video_has_probe_marker_candidate(timing: &Value) -> bool {
    [
        "/start/before",
        "/start/after",
        "/stop/before",
        "/stop/after",
    ]
    .iter()
    .filter_map(|pointer| timing.pointer(pointer))
    .any(|marker| {
        marker.get("schema").is_some()
            || marker.get("device_clock_probe").is_some()
            || marker.get("t_elapsed_realtime_ns").is_some()
    })
}

fn index_has_legacy_wall_time(index: Option<&Value>) -> bool {
    index
        .and_then(|value| value.get("captured_wall_ms"))
        .is_some()
}

fn timing_file_stale_reason(dir: &Path, timing: &Value) -> CliResult<Option<String>> {
    let Some(recorded_sha) = timing.pointer("/file/sha256").and_then(Value::as_str) else {
        return Ok(None);
    };
    let screen_path = dir.join("video").join("screen.mp4");
    if !screen_path.exists() {
        return Ok(Some(String::from(
            "video timing source file is missing: video/screen.mp4",
        )));
    }
    let current = file_fingerprint(&screen_path)?;
    let current_sha = current.get("sha256").and_then(Value::as_str);
    if current_sha == Some(recorded_sha) {
        Ok(None)
    } else {
        Ok(Some(String::from(
            "video timing source fingerprint changed: video/screen.mp4",
        )))
    }
}

fn merge_arrays(first: &[String], second: &[String]) -> Vec<String> {
    first.iter().chain(second).cloned().collect()
}

fn timeline_clock_status(timeline: &TimelineInspection) -> &'static str {
    if !timeline.exists {
        AlignmentStatus::MissingSource.as_str()
    } else if timeline.stale_reasons.is_empty() {
        AlignmentStatus::NotEstimated.as_str()
    } else {
        AlignmentStatus::StaleInputs.as_str()
    }
}

fn timeline_source_stale_reasons(dir: &Path, source: &Value) -> CliResult<Vec<String>> {
    let mut reasons = Vec::new();
    let kind = source
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown_source");
    let recorded_exists = source
        .get("exists")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let required = source
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !recorded_exists && !required {
        return Ok(reasons);
    }
    let Some(path_text_value) = source.get("path").and_then(Value::as_str) else {
        reasons.push(format!("{kind} has no source path"));
        return Ok(reasons);
    };
    let path = dir.join(path_text_value);
    if !path.exists() {
        reasons.push(format!("{kind} source is missing: {path_text_value}"));
        return Ok(reasons);
    }
    let current = file_fingerprint(&path)?;
    let recorded_sha = source
        .pointer("/fingerprint/sha256")
        .and_then(Value::as_str);
    let current_sha = current.get("sha256").and_then(Value::as_str);
    if recorded_sha.is_some() && recorded_sha != current_sha {
        reasons.push(format!(
            "{kind} source fingerprint changed: {path_text_value}"
        ));
    }
    Ok(reasons)
}

fn artifact_specs(dir: &Path, session_jsonl: Option<&Path>) -> Vec<ArtifactSpec> {
    let mut specs = Vec::new();
    specs.extend(core_artifact_specs(dir));
    specs.extend(session_artifact_specs(dir));
    specs.extend(adb_artifact_specs(dir));
    specs.extend(video_artifact_specs(dir));
    specs.extend(derived_artifact_specs(dir));
    specs.extend(evidence_artifact_specs(dir));
    if let Some(path) = session_jsonl {
        specs.push(artifact(
            "ime_session_jsonl",
            path.to_path_buf(),
            ArtifactRequirement::Required,
            ArtifactSensitivity::Sensitive,
        ));
    }
    specs
}

fn session_artifact_specs(dir: &Path) -> [ArtifactSpec; 3] {
    [
        artifact(
            "session_state",
            dir.join("session").join("state.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "session_finalization",
            dir.join("session").join("finalization.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "session_lock_snapshot",
            dir.join("session").join("lock.snapshot.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
    ]
}

fn video_artifact_specs(dir: &Path) -> [ArtifactSpec; 5] {
    [
        artifact(
            "video_screen",
            dir.join("video").join("screen.mp4"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "video_timing",
            dir.join("video").join("timing.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "video_stdout",
            dir.join("video").join("screenrecord.stdout.log"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Normal,
        ),
        artifact(
            "video_stderr",
            dir.join("video").join("screenrecord.stderr.log"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Normal,
        ),
        artifact(
            "video_pull_log",
            dir.join("video").join("adb-pull-video.log"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Normal,
        ),
    ]
}

fn core_artifact_specs(dir: &Path) -> [ArtifactSpec; 3] {
    [
        artifact(
            "manifest",
            dir.join("manifest.json"),
            ArtifactRequirement::Required,
            ArtifactSensitivity::Normal,
        ),
        artifact(
            "validation",
            dir.join("validation.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Normal,
        ),
        artifact(
            "readme",
            dir.join("README.md"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Normal,
        ),
    ]
}

fn adb_artifact_specs(dir: &Path) -> [ArtifactSpec; 3] {
    [
        artifact(
            "adb_getevent_raw",
            dir.join("adb").join("getevent.raw.log"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "adb_getevent_jsonl",
            dir.join("adb").join("getevent.jsonl"),
            ArtifactRequirement::Required,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "adb_getevent_stderr",
            dir.join("adb").join("getevent.stderr.log"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Normal,
        ),
    ]
}

fn derived_artifact_specs(dir: &Path) -> [ArtifactSpec; 10] {
    [
        artifact(
            "press_summaries",
            dir.join("derived").join("press_summaries.jsonl"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "run_summary",
            dir.join("derived").join("run_summary.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "touch_gestures",
            dir.join("derived").join("touch_gestures.jsonl"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "dismissal_inferences",
            dir.join("derived").join("dismissal_inferences.jsonl"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "timeline_index",
            dir.join("derived").join("timeline").join("index.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "timeline_events",
            dir.join("derived").join("timeline").join("events.jsonl"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "video_map_index",
            dir.join("derived").join("video_map").join("index.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "video_map_frames",
            dir.join("derived").join("video_map").join("frames.jsonl"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "video_map_alignment",
            dir.join("derived").join("video_map").join("alignment.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "video_map_event_frames",
            dir.join("derived")
                .join("video_map")
                .join("event_frames.jsonl"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
    ]
}

fn evidence_artifact_specs(dir: &Path) -> [ArtifactSpec; 2] {
    [
        artifact(
            "evidence_start_index",
            dir.join("evidence").join("start").join("index.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
        artifact(
            "evidence_end_index",
            dir.join("evidence").join("end").join("index.json"),
            ArtifactRequirement::Optional,
            ArtifactSensitivity::Sensitive,
        ),
    ]
}

const fn artifact(
    key: &'static str,
    path: PathBuf,
    requirement: ArtifactRequirement,
    sensitivity: ArtifactSensitivity,
) -> ArtifactSpec {
    ArtifactSpec {
        key,
        path,
        requirement,
        sensitivity,
    }
}

fn artifact_map(dir: &Path, specs: &[ArtifactSpec]) -> CliResult<Value> {
    let mut map = Map::new();
    for spec in specs {
        map.insert(spec.key.to_owned(), artifact_json(dir, spec)?);
    }
    Ok(Value::Object(map))
}

fn artifact_json(dir: &Path, spec: &ArtifactSpec) -> CliResult<Value> {
    let exists = spec.path.exists();
    let fingerprint = if exists {
        file_fingerprint(&spec.path)?
    } else {
        Value::Null
    };
    Ok(json!({
        "path": relative_path_text(dir, &spec.path),
        "exists": exists,
        "required": matches!(spec.requirement, ArtifactRequirement::Required),
        "sensitive": matches!(spec.sensitivity, ArtifactSensitivity::Sensitive),
        "schema": artifact_schema(&spec.path)?,
        "record_count": artifact_record_count(&spec.path)?,
        "fingerprint": fingerprint,
    }))
}

fn select_session_jsonl(dir: &Path) -> CliResult<SessionSelection> {
    let ime_dir = dir.join("ime");
    if !ime_dir.exists() {
        return Ok(SessionSelection {
            selected: None,
            candidates: Vec::new(),
            warnings: vec![String::from("ime directory is missing")],
        });
    }
    let mut candidates = Vec::new();
    collect_session_jsonl_files(&ime_dir, &mut candidates)?;
    candidates.sort();
    let mut warnings = Vec::new();
    let selected = match candidates.len() {
        0 => {
            warnings.push(String::from("no IME session JSONL file found"));
            None
        }
        1 => candidates.first().cloned(),
        count => {
            warnings.push(format!(
                "multiple IME session JSONL files found: {count}; pass explicit paths to derivation commands"
            ));
            None
        }
    };
    Ok(SessionSelection {
        selected,
        candidates,
        warnings,
    })
}

fn collect_session_jsonl_files(dir: &Path, candidates: &mut Vec<PathBuf>) -> CliResult<()> {
    for entry_result in fs::read_dir(dir)? {
        let entry = entry_result?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_session_jsonl_files(&path, candidates)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let is_jsonl = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
        if file_name.starts_with("session-") && is_jsonl {
            candidates.push(path);
        }
    }
    Ok(())
}

fn session_json(dir: &Path, session: &SessionSelection) -> Value {
    json!({
        "selected": session.selected.as_ref().map(|path| relative_path_text(dir, path)),
        "candidates": session.candidates
            .iter()
            .map(|path| relative_path_text(dir, path))
            .collect::<Vec<_>>(),
        "warnings": session.warnings,
    })
}

fn validation_json(dir: &Path, validation: &ValidationInspection) -> Value {
    json!({
        "stored_present": validation.stored_present,
        "stored": sanitize_inspect_value(dir, &validation.stored),
        "current": sanitize_inspect_value(dir, &validation.current),
        "stale_reasons": validation
            .stale_reasons
            .iter()
            .map(|reason| sanitize_inspect_text(dir, reason))
            .collect::<Vec<_>>(),
    })
}

fn sanitize_inspect_value(dir: &Path, value: &Value) -> Value {
    match *value {
        Value::String(ref text) => json!(sanitize_inspect_text(dir, text)),
        Value::Array(ref items) => Value::Array(
            items
                .iter()
                .map(|item| sanitize_inspect_value(dir, item))
                .collect(),
        ),
        Value::Object(ref map) => {
            let sanitized = map
                .iter()
                .map(|(key, item)| (key.clone(), sanitize_inspect_value(dir, item)))
                .collect::<Map<_, _>>();
            Value::Object(sanitized)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

fn sanitize_inspect_text(dir: &Path, text: &str) -> String {
    if let Some(relative) = recording_relative_text(dir, text) {
        return relative;
    }
    if contains_private_path_marker(text) {
        return String::from("<redacted-path>");
    }
    text.to_owned()
}

fn recording_relative_text(dir: &Path, text: &str) -> Option<String> {
    let path = Path::new(text);
    if let Ok(relative) = path.strip_prefix(dir) {
        return Some(path_text(relative));
    }
    if path.is_absolute() {
        let canonical_dir = fs::canonicalize(dir).ok()?;
        if let Ok(relative) = path.strip_prefix(&canonical_dir) {
            return Some(path_text(relative));
        }
    }
    None
}

fn contains_private_path_marker(text: &str) -> bool {
    text.contains("/Users/")
        || text.contains("/home/")
        || text.contains("../lab/")
        || text.contains("lab/experiments")
}

fn session_state_json(session: &SessionInspection) -> Value {
    json!({
        "classification": session.classification.as_str(),
        "state": session_file_json(&session.state),
        "finalization": session_file_json(&session.finalization),
        "lock_snapshot": session_file_json(&session.lock_snapshot),
        "lifecycle_state": session.lifecycle_state.clone(),
        "finalization_run_state": session.finalization_run_state.clone(),
        "command": session.command.clone(),
        "terminal": session_lifecycle_is_terminal(&session.state),
        "complete": session.classification.is_complete(),
        "incomplete": session.classification.is_incomplete(),
        "active": session.classification.is_active_or_in_progress(),
        "needs_stop": session.classification.needs_stop(),
        "needs_repair": session.classification.needs_repair(),
        "stale_reasons": session.stale_reasons.clone(),
    })
}

fn session_file_json(file: &SessionFileInspection) -> Value {
    json!({
        "present": file.present,
        "status": file.status.clone(),
        "path": file.path.clone(),
        "schema": file.schema.clone(),
        "error": file.error.clone(),
        "summary": file.summary.clone(),
    })
}

fn note_flags(dir: &Path) -> CliResult<Value> {
    let readme_path = dir.join("README.md");
    if !readme_path.exists() {
        return Ok(json!({
            "readme_present": false,
            "mentions_incomplete": false,
            "mentions_superseded": false,
        }));
    }
    let text = fs::read_to_string(readme_path)?.to_lowercase();
    Ok(json!({
        "readme_present": true,
        "mentions_incomplete": text.contains("incomplete"),
        "mentions_superseded": text.contains("superseded"),
    }))
}

fn flags_json(inputs: &FlagInputs<'_>) -> Value {
    let has_getevent_jsonl = artifact_exists(inputs.artifacts, "adb_getevent_jsonl");
    let has_press_summaries = artifact_exists(inputs.artifacts, "press_summaries");
    let has_touch_gestures = artifact_exists(inputs.artifacts, "touch_gestures");
    let has_dismissals = artifact_exists(inputs.artifacts, "dismissal_inferences");
    let has_video = inputs.video.exists && inputs.video.stale_reasons.is_empty();
    let needs_video = inputs.video.required && !has_video;
    let needs_timeline = !inputs.timeline.exists || !inputs.timeline.stale_reasons.is_empty();
    let timeline_ready = !needs_timeline;
    let has_video_frame_index = has_video && inputs.video_map.frame_index_exists;
    let needs_video_frame_index = has_video && !inputs.video_map.frame_index_exists;
    let has_video_map = has_video && timeline_ready && inputs.video_map.event_map_exists;
    let needs_video_map = has_video && timeline_ready && !inputs.video_map.event_map_exists;
    let has_evidence = artifact_exists(inputs.artifacts, "evidence_start_index")
        || artifact_exists(inputs.artifacts, "evidence_end_index");
    let needs_derivation = !has_touch_gestures || !has_dismissals;
    let needs_run_summary =
        !inputs.run_summary.exists || !inputs.run_summary.stale_reasons.is_empty();
    let needs_canonical_video = bool_at(&inputs.clock.value, "/video/required")
        && !bool_at(&inputs.clock.value, "/video/canonical");
    let needs_canonical_evidence = bool_at(&inputs.clock.value, "/evidence/requested")
        && !bool_at(&inputs.clock.value, "/evidence/canonical");
    let session_blocks_analysis = inputs.session_state.classification.blocks_analysis();
    let video_ended_early = finalization_has_error_code(
        &inputs.session_state.finalization.summary,
        SessionErrorCode::VideoEndedEarly,
    );
    let required_process = required_process_flags(&inputs.session_state.finalization.summary);
    let needs_session_rerun = video_ended_early
        || required_process.failed()
        || inputs.session_state.classification.is_incomplete();
    let incomplete_or_superseded = incomplete_or_superseded(inputs);
    json!({
        "valid_for_analysis": inputs.manifest.is_some()
            && inputs.session.selected.is_some()
            && has_getevent_jsonl
            && inputs.validation.current_ok
            && !session_blocks_analysis
            && !needs_video,
        "needs_validation": !inputs.validation.stored_present
            || !inputs.validation.stale_reasons.is_empty(),
        "needs_video": needs_video,
        "has_video": has_video,
        "canonical_clock_ready": inputs.clock.canonical_clock_ready,
        "has_legacy_timing": inputs.clock.has_legacy_timing,
        "needs_canonical_recording": inputs.clock.needs_canonical_recording,
        "needs_canonical_video": needs_canonical_video,
        "needs_canonical_evidence": needs_canonical_evidence,
        "needs_press_summaries": !has_press_summaries,
        "needs_run_summary": needs_run_summary,
        "needs_derivation": needs_derivation,
        "needs_timeline": needs_timeline,
        "has_video_frame_index": has_video_frame_index,
        "needs_video_frame_index": needs_video_frame_index,
        "has_video_map": has_video_map,
        "needs_video_map": needs_video_map,
        "lifecycle_complete": inputs.session_state.classification.is_complete(),
        "lifecycle_incomplete": inputs.session_state.classification.is_incomplete(),
        "lifecycle_active": inputs.session_state.classification.is_active(),
        "lifecycle_in_progress": inputs.session_state.classification.is_in_progress(),
        "session_classification": inputs.session_state.classification.as_str(),
        "needs_session_stop": inputs.session_state.classification.needs_stop(),
        "needs_session_repair": inputs.session_state.classification.needs_repair(),
        "video_ended_early": video_ended_early,
        "required_process_failed": required_process.failed(),
        "required_process_ended_early": required_process.ended_early,
        "required_process_unverifiable": required_process.unverifiable,
        "required_process_stop_failed": required_process.stop_failed,
        "required_process_failure_codes": required_process.failure_codes,
        "needs_session_rerun": needs_session_rerun,
        "has_sensitive_evidence": has_evidence || has_video,
        "incomplete_or_superseded": incomplete_or_superseded,
        "needs_cleanup": needs_cleanup(inputs.manifest),
    })
}

fn incomplete_or_superseded(inputs: &FlagInputs<'_>) -> bool {
    !inputs.validation.current_ok
        || inputs.session_state.classification.blocks_analysis()
        || bool_at(inputs.note_flags, "/mentions_incomplete")
        || bool_at(inputs.note_flags, "/mentions_superseded")
}

fn required_process_flags(summary: &Value) -> RequiredProcessFlags {
    let ended_early =
        finalization_has_error_code(summary, SessionErrorCode::RequiredProcessEndedEarly);
    let unverifiable =
        finalization_has_error_code(summary, SessionErrorCode::RequiredProcessUnverifiable);
    let stop_failed =
        finalization_has_error_code(summary, SessionErrorCode::RequiredProcessStopFailed);
    let mut failure_codes = Vec::new();
    if ended_early {
        failure_codes.push(String::from("required_process_ended_early"));
    }
    if unverifiable {
        failure_codes.push(String::from("required_process_unverifiable"));
    }
    if stop_failed {
        failure_codes.push(String::from("required_process_stop_failed"));
    }
    RequiredProcessFlags {
        ended_early,
        unverifiable,
        stop_failed,
        failure_codes,
    }
}

fn finalization_has_error_code(summary: &Value, error_code: SessionErrorCode) -> bool {
    let Ok(expected) = serde_json::to_value(error_code) else {
        return false;
    };
    summary
        .get("failed_step_error_codes")
        .and_then(Value::as_object)
        .is_some_and(|codes| codes.values().any(|code| code == &expected))
}

fn next_actions(
    dir: &Path,
    session: &SessionInspection,
    external_run_id: Option<&str>,
    flags: &Value,
) -> CliResult<Value> {
    let mut actions = Vec::new();
    add_lifecycle_next_action(&mut actions, session, external_run_id, flags);
    if bool_at(flags, "/needs_validation") {
        let mut command = format!("input-dynamics validate {}", shellish(&dir.join("ime"))?);
        if let Some(run_id) = external_run_id {
            command.push_str(" --run-id ");
            command.push_str(&shellish_text(run_id));
        }
        actions.push(json!({
            "kind": "validate",
            "command": command,
            "reason": "refresh validation from current IME JSONL files",
        }));
    }
    add_session_next_action(&mut actions, flags);
    if !bool_at(flags, "/incomplete_or_superseded") && !bool_at(flags, "/lifecycle_active") {
        add_derivation_next_actions(&mut actions, dir, flags)?;
    }
    Ok(Value::Array(actions))
}

fn add_lifecycle_next_action(
    actions: &mut Vec<Value>,
    session: &SessionInspection,
    external_run_id: Option<&str>,
    flags: &Value,
) {
    if bool_at(flags, "/needs_session_stop") {
        push_session_stop_action(actions, session_action_run_id(session, external_run_id));
        return;
    }
    if bool_at(flags, "/lifecycle_in_progress") {
        push_session_status_action(actions, session_action_run_id(session, external_run_id));
        return;
    }
    if bool_at(flags, "/needs_session_repair") {
        push_session_repair_action(actions);
    }
}

fn session_action_run_id<'a>(
    session: &'a SessionInspection,
    external_run_id: Option<&'a str>,
) -> &'a str {
    session_run_id(session)
        .or(external_run_id)
        .unwrap_or("<run-id>")
}

fn push_session_stop_action(actions: &mut Vec<Value>, run_id: &str) {
    actions.push(json!({
        "kind": "session_stop",
        "workflow": "session_status_stop_inspect",
        "command": format!(
            "input-dynamics session stop --run-id {}",
            shellish_text(run_id)
        ),
        "commands": [
            {
                "step": "status",
                "command": format!(
                    "input-dynamics session status --run-id {}",
                    shellish_text(run_id)
                ),
                "argv": ["input-dynamics", "session", "status", "--run-id", run_id],
            },
            {
                "step": "stop",
                "command": format!(
                    "input-dynamics session stop --run-id {}",
                    shellish_text(run_id)
                ),
                "argv": ["input-dynamics", "session", "stop", "--run-id", run_id],
            },
            {
                "step": "inspect",
                "command": "input-dynamics recording inspect --dir <run-dir>",
                "argv": ["input-dynamics", "recording", "inspect", "--dir", "<run-dir>"],
            },
        ],
        "reason": "umbrella session state is active; finalize the active session before analysis",
    }));
}

fn push_session_status_action(actions: &mut Vec<Value>, run_id: &str) {
    actions.push(json!({
        "kind": "session_status",
        "workflow": "session_status_inspect",
        "command": format!(
            "input-dynamics session status --run-id {}",
            shellish_text(run_id)
        ),
        "commands": [
            {
                "step": "status",
                "command": format!(
                    "input-dynamics session status --run-id {}",
                    shellish_text(run_id)
                ),
                "argv": ["input-dynamics", "session", "status", "--run-id", run_id],
            },
            {
                "step": "inspect",
                "command": "input-dynamics recording inspect --dir <run-dir>",
                "argv": ["input-dynamics", "recording", "inspect", "--dir", "<run-dir>"],
            },
        ],
        "reason": "umbrella session state is in progress; check lifecycle status before taking another action",
    }));
}

fn push_session_repair_action(actions: &mut Vec<Value>) {
    actions.push(json!({
        "kind": "session_repair_required",
        "workflow": "inspect_session_files",
        "command": "input-dynamics recording inspect --dir <run-dir>",
        "commands": [
            {
                "step": "inspect",
                "command": "input-dynamics recording inspect --dir <run-dir>",
                "argv": ["input-dynamics", "recording", "inspect", "--dir", "<run-dir>"],
            },
        ],
        "reason": "umbrella session files are missing, corrupt, unsupported, or inconsistent; do not analyze this directory as complete",
    }));
}

fn session_run_id(session: &SessionInspection) -> Option<&str> {
    session
        .state
        .summary
        .get("run_id")
        .and_then(Value::as_str)
        .or_else(|| {
            session
                .lock_snapshot
                .summary
                .get("run_id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            session
                .finalization
                .summary
                .get("run_id")
                .and_then(Value::as_str)
        })
}

fn add_session_next_action(actions: &mut Vec<Value>, flags: &Value) {
    if session_refresh_needed(flags) {
        let evidence = if bool_at(flags, "/needs_canonical_evidence") {
            EvidenceRefresh::Include
        } else {
            EvidenceRefresh::Omit
        };
        let kind = session_refresh_kind(flags);
        let start_command = session_refresh_start_command(evidence);
        let commands = session_refresh_commands(&start_command, evidence);
        actions.push(json!({
            "kind": kind,
            "workflow": "session_start_status_stop_inspect",
            "command": start_command,
            "commands": commands,
            "reason": session_refresh_reason(flags, evidence),
        }));
    }
}

fn session_refresh_needed(flags: &Value) -> bool {
    bool_at(flags, "/needs_session_rerun")
        || bool_at(flags, "/needs_video")
        || bool_at(flags, "/needs_canonical_recording")
}

fn session_refresh_kind(flags: &Value) -> &'static str {
    if bool_at(flags, "/needs_session_rerun") {
        "session_rerun"
    } else if bool_at(flags, "/needs_video") {
        "session_with_video"
    } else {
        "session_with_canonical_clocks"
    }
}

fn session_refresh_start_command(evidence: EvidenceRefresh) -> String {
    let mut command = String::from(
        "input-dynamics session start --input-actor human --run-id <new-run-id> --out <new-run-dir>",
    );
    if evidence.include() {
        command.push_str(" --with-evidence");
    }
    command
}

fn session_refresh_commands(start_command: &str, evidence: EvidenceRefresh) -> Value {
    json!([
        {
            "step": "start",
            "command": start_command,
            "argv": session_refresh_start_argv(evidence),
        },
        {
            "step": "status",
            "command": "input-dynamics session status --run-id <new-run-id>",
            "argv": ["input-dynamics", "session", "status", "--run-id", "<new-run-id>"],
        },
        {
            "step": "stop",
            "command": "input-dynamics session stop --run-id <new-run-id>",
            "argv": ["input-dynamics", "session", "stop", "--run-id", "<new-run-id>"],
        },
        {
            "step": "inspect",
            "command": "input-dynamics recording inspect --dir <new-run-dir>",
            "argv": ["input-dynamics", "recording", "inspect", "--dir", "<new-run-dir>"],
        },
    ])
}

fn session_refresh_start_argv(evidence: EvidenceRefresh) -> Value {
    let mut argv = vec![
        "input-dynamics",
        "session",
        "start",
        "--input-actor",
        "human",
        "--run-id",
        "<new-run-id>",
        "--out",
        "<new-run-dir>",
    ];
    if evidence.include() {
        argv.push("--with-evidence");
    }
    json!(argv)
}

fn session_refresh_reason(flags: &Value, evidence: EvidenceRefresh) -> &'static str {
    if bool_at(flags, "/video_ended_early") {
        return "rerun because screen recording ended before session finalization";
    }
    if bool_at(flags, "/required_process_ended_early") {
        return "rerun because a required capture process ended before session finalization";
    }
    if bool_at(flags, "/required_process_unverifiable") {
        return "rerun because a required capture process could not be verified during finalization";
    }
    if bool_at(flags, "/required_process_stop_failed") {
        return "rerun because a required capture process did not stop cleanly";
    }
    match (evidence, bool_at(flags, "/needs_video")) {
        (EvidenceRefresh::Include, true) => {
            "rerun with video and evidence to refresh request-correlated device clock anchors"
        }
        (EvidenceRefresh::Include, false) => {
            "rerun with evidence to refresh request-correlated device clock anchors"
        }
        (EvidenceRefresh::Omit, true) => {
            "rerun with video to refresh request-correlated device clock anchors"
        }
        (EvidenceRefresh::Omit, false) => "start a new session to refresh device clock anchors",
    }
}

fn add_derivation_next_actions(
    actions: &mut Vec<Value>,
    dir: &Path,
    flags: &Value,
) -> CliResult<()> {
    if bool_at(flags, "/needs_press_summaries") {
        actions.push(json!({
            "kind": "derive_presses",
            "command": format!(
                "input-dynamics derive presses --recording-dir {}",
                shellish(dir)?
            ),
            "reason": "derive or refresh per-press timing and pointer summaries",
        }));
    }
    if bool_at(flags, "/needs_run_summary") {
        actions.push(json!({
            "kind": "derive_summary",
            "command": format!(
                "input-dynamics derive summary --recording-dir {}",
                shellish(dir)?
            ),
            "reason": "derive or refresh the run-level press summary",
        }));
    }
    if bool_at(flags, "/needs_derivation") {
        actions.push(json!({
            "kind": "derive_dismissals",
            "command": format!(
                "input-dynamics derive dismissals --recording-dir {}",
                shellish(dir)?
            ),
            "reason": "derive or refresh touch gestures and dismissal inferences",
        }));
    }
    if bool_at(flags, "/needs_timeline") {
        actions.push(json!({
            "kind": "derive_timeline",
            "command": format!(
                "input-dynamics derive timeline --recording-dir {}",
                shellish(dir)?
            ),
            "reason": "derive or refresh the cross-source recording timeline",
        }));
    }
    if bool_at(flags, "/needs_video_map") {
        actions.push(json!({
            "kind": "derive_video_map",
            "command": format!(
                "input-dynamics derive video-map --recording-dir {}",
                shellish(dir)?
            ),
            "reason": "derive or refresh the video event-frame map",
        }));
    }
    Ok(())
}

fn warnings(inputs: &FlagInputs<'_>) -> Vec<String> {
    let mut warnings = inputs.session.warnings.clone();
    warnings.extend(inputs.session_state.warnings.iter().cloned());
    warnings.extend(inputs.validation.stale_reasons.iter().cloned());
    warnings.extend(inputs.run_summary.stale_reasons.iter().cloned());
    warnings.extend(inputs.timeline.stale_reasons.iter().cloned());
    warnings.extend(inputs.video_map.stale_reasons.iter().cloned());
    warnings.extend(inputs.video.stale_reasons.iter().cloned());
    warnings.extend(inputs.clock.warnings.iter().cloned());
    warnings
}

fn validation_stale_reasons(stored: &Value, current: &Value) -> Vec<String> {
    let fields = [
        "ok",
        "record_count",
        "selected_record_count",
        "session_start_count",
        "session_stop_count",
        "password_record_count",
        "invalid_timestamp_metadata_count",
        "clock_validation",
        "failure_reasons",
        "diagnostic_reasons",
        "target_package_seen",
    ];
    fields
        .into_iter()
        .filter_map(|field| validation_field_stale_reason(stored, current, field))
        .collect()
}

fn validation_field_stale_reason(stored: &Value, current: &Value, field: &str) -> Option<String> {
    let stored_value = stored.get(field);
    let current_value = current.get(field);
    (stored_value != current_value).then(|| format!("validation field changed: {field}"))
}

fn provenance_json(manifest: Option<&Value>) -> Value {
    json!({
        "input_actor": string_at(manifest, "/input_actor"),
        "input_controller": value_at(manifest, "/input_controller"),
        "input_backend": value_at(manifest, "/input_backend"),
        "input_cadence_policy": string_at(manifest, "/input_cadence_policy"),
        "input_profile": value_at(manifest, "/input_controller_runtime/summary/input_profile"),
    })
}

fn timing_json(manifest: Option<&Value>) -> Value {
    json!({
        "host_start_wall_ms": value_at(manifest, "/host_start_wall_ms"),
        "host_stop_wall_ms": value_at(manifest, "/host_stop_wall_ms"),
    })
}

fn artifact_exists(artifacts: &Value, key: &str) -> bool {
    artifacts
        .get(key)
        .and_then(|artifact| artifact.get("exists"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn needs_cleanup(manifest: Option<&Value>) -> bool {
    manifest
        .and_then(|value| value.pointer("/input_controller_runtime/summary/cleanup/ok"))
        .and_then(Value::as_bool)
        .is_some_and(|ok| !ok)
}

fn artifact_schema(path: &Path) -> CliResult<Value> {
    if !path.exists() {
        return Ok(Value::Null);
    }
    if path.extension().and_then(OsStr::to_str) == Some("json") {
        let Some(value) = read_optional_json(path)? else {
            return Ok(Value::Null);
        };
        return Ok(value.get("schema").cloned().unwrap_or(Value::Null));
    }
    if path.extension().and_then(OsStr::to_str) == Some("jsonl") {
        return Ok(first_jsonl_value(path)?
            .and_then(|value| value.get("schema").cloned())
            .unwrap_or(Value::Null));
    }
    Ok(Value::Null)
}

fn artifact_record_count(path: &Path) -> CliResult<Value> {
    if !path.exists() || path.extension().and_then(OsStr::to_str) != Some("jsonl") {
        return Ok(Value::Null);
    }
    Ok(json!(count_nonempty_lines(path)?))
}

fn count_nonempty_lines(path: &Path) -> CliResult<u64> {
    let reader = BufReader::new(File::open(path)?);
    let mut count = 0_u64;
    for line_result in reader.lines() {
        let line = line_result?;
        if !line.trim().is_empty() {
            count = count
                .checked_add(1)
                .ok_or_else(|| CliError::new("line count overflow"))?;
        }
    }
    Ok(count)
}

fn first_jsonl_value(path: &Path) -> CliResult<Option<Value>> {
    let reader = BufReader::new(File::open(path)?);
    for line_result in reader.lines() {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }
        return Ok(Some(serde_json::from_str(&line)?));
    }
    Ok(None)
}

fn read_optional_json(path: &Path) -> CliResult<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&text)?))
}

fn file_fingerprint(path: &Path) -> CliResult<Value> {
    let metadata = fs::metadata(path)?;
    Ok(json!({
        "byte_count": metadata.len(),
        "modified_wall_ms": modified_wall_ms(&metadata)?,
        "sha256": format!("{SHA256_PREFIX}{}", sha256_file(path)?),
    }))
}

fn modified_wall_ms(metadata: &fs::Metadata) -> CliResult<Option<u64>> {
    let modified = metadata.modified()?;
    let duration = match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration,
        Err(_time_error) => return Ok(None),
    };
    Ok(Some(u64::try_from(duration.as_millis()).map_err(
        |error| CliError::new(format!("modified time overflow: {error}")),
    )?))
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

fn string_at(manifest: Option<&Value>, pointer: &str) -> Option<String> {
    manifest
        .and_then(|value| value.pointer(pointer))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn value_at(manifest: Option<&Value>, pointer: &str) -> Value {
    manifest
        .and_then(|value| value.pointer(pointer))
        .cloned()
        .unwrap_or(Value::Null)
}

fn bool_at(value: &Value, pointer: &str) -> bool {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn bool_at_manifest(manifest: Option<&Value>, pointer: &str) -> bool {
    manifest
        .and_then(|value| value.pointer(pointer))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn relative_path_text(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .map_or_else(|_strip_error| path_text(path), path_text)
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

fn shellish(path: &Path) -> CliResult<String> {
    let text = path
        .to_str()
        .ok_or_else(|| CliError::new(format!("path is not valid UTF-8: {}", path.display())))?;
    Ok(shellish_text(text))
}

fn shellish_text(text: &str) -> String {
    if text
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_./:".contains(character))
    {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
#[path = "recording/tests.rs"]
mod tests;
