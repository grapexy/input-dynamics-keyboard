//! Capture-session schema vocabulary and validation helpers.

#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const STATE_SCHEMA: &str = "input_dynamics_capture_session_state.v1";
pub(crate) const LOCK_SCHEMA: &str = "input_dynamics_capture_session_lock.v1";
pub(crate) const CURRENT_SCHEMA: &str = "input_dynamics_capture_session_current.v1";
pub(crate) const FINALIZATION_SCHEMA: &str = "input_dynamics_capture_session_finalization.v1";
pub(crate) const COMMAND_RESULT_SCHEMA: &str = "input_dynamics_session_command_result.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LockState {
    Starting,
    Active,
    StopRequested,
    Stopping,
    Finalizing,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecycleState {
    Starting,
    ImeStarted,
    VideoStarted,
    GeteventStarted,
    StartEvidenceCaptured,
    ControllerStarted,
    Active,
    StopRequested,
    Stopping,
    EndEvidenceCapturing,
    Finalizing,
    Complete,
    Incomplete,
    Aborted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProcessState {
    NotStarted,
    Starting,
    Running,
    Exited,
    StopRequested,
    Stopped,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StepStatus {
    Pending,
    Running,
    Ok,
    Failed,
    Skipped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadStatus {
    Missing,
    Valid,
    Mismatched,
    Corrupt,
    UnsupportedSchema,
    Stale,
    IoError,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionErrorCode {
    ActiveSessionExists,
    SelectorMismatch,
    StateCorrupt,
    UnsupportedSchema,
    StaleLock,
    IoError,
    RepairRequired,
    FinalizationInProgress,
    ControllerNotEnabled,
    VideoEndedEarly,
    NoActiveSession,
    SequenceMismatch,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum CaptureSessionCommandName {
    #[serde(rename = "session start")]
    SessionStart,
    #[serde(rename = "session run")]
    SessionRun,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaptureSessionCommand {
    pub(crate) name: CaptureSessionCommandName,
    pub(crate) bounded: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProcessKind {
    Host,
    AdbShell,
    AndroidApp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Requirement {
    Required,
    Optional,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ArtifactStatus {
    pub(crate) path: String,
    pub(crate) required: bool,
    pub(crate) present: bool,
    pub(crate) valid: bool,
    pub(crate) schema: Option<String>,
    pub(crate) fingerprint: Option<String>,
    pub(crate) producer_step: String,
    pub(crate) failure_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FinalizationStep {
    pub(crate) name: String,
    pub(crate) required: bool,
    pub(crate) status: StepStatus,
    pub(crate) attempt_count: u64,
    pub(crate) can_retry: bool,
    pub(crate) started_wall_ms: Option<u64>,
    pub(crate) finished_wall_ms: Option<u64>,
    pub(crate) message: Option<String>,
    pub(crate) error_code: Option<String>,
    #[serde(flatten)]
    pub(crate) cleanup: FinalizationStepCleanup,
    pub(crate) artifact_keys: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FinalizationStepCleanup {
    pub(crate) cleanup_attempted: bool,
    pub(crate) cleanup_ok: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ProfileProvenance {
    pub(crate) source: String,
    pub(crate) id: Option<String>,
    pub(crate) schema: Option<String>,
    pub(crate) hash: Option<String>,
    pub(crate) seed: Option<u64>,
    pub(crate) parameter_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SuggestedNextCommand {
    pub(crate) argv: Vec<String>,
    pub(crate) reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CommandErrorEnvelope {
    pub(crate) schema: String,
    pub(crate) ok: bool,
    pub(crate) command: String,
    pub(crate) error_code: SessionErrorCode,
    pub(crate) message: String,
    pub(crate) classification: Option<ReadStatus>,
    pub(crate) selector: Value,
    pub(crate) observed: Value,
    pub(crate) paths: Value,
    pub(crate) suggested_next_command: Option<SuggestedNextCommand>,
    pub(crate) mutated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FinalizationOwner {
    pub(crate) owner_pid: u32,
    pub(crate) owner_host: String,
    pub(crate) invocation_id: String,
    pub(crate) claimed_wall_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaptureSessionLock {
    pub(crate) schema: String,
    pub(crate) lock_state: LockState,
    pub(crate) observed_lifecycle_state: LifecycleState,
    pub(crate) mutation_seq: u64,
    pub(crate) package_name: String,
    pub(crate) device_serial: String,
    pub(crate) run_id: String,
    pub(crate) command: CaptureSessionCommand,
    pub(crate) output_dir: String,
    pub(crate) state_path: String,
    pub(crate) owner_pid: u32,
    pub(crate) owner_host: String,
    pub(crate) invocation_id: String,
    pub(crate) created_wall_ms: u64,
    pub(crate) updated_wall_ms: u64,
    pub(crate) cli_version: String,
    pub(crate) finalization_owner: Option<FinalizationOwner>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaptureSessionCurrent {
    pub(crate) schema: String,
    pub(crate) package_name: String,
    pub(crate) device_serial: String,
    pub(crate) run_id: String,
    pub(crate) output_dir: String,
    pub(crate) state_path: String,
    pub(crate) lock_path: String,
    pub(crate) observed_lifecycle_state: LifecycleState,
    pub(crate) observed_lock_state: Option<LockState>,
    pub(crate) updated_wall_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct LifecycleSnapshot {
    pub(crate) state: LifecycleState,
    pub(crate) stage: String,
    pub(crate) history: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ProcessDescriptor {
    pub(crate) name: String,
    pub(crate) kind: ProcessKind,
    pub(crate) required: bool,
    pub(crate) state: ProcessState,
    pub(crate) host_pid: Option<u32>,
    pub(crate) host_process_group_id: Option<u32>,
    pub(crate) remote_pid: Option<u32>,
    pub(crate) argv: Vec<String>,
    pub(crate) remote_command: Vec<String>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) started_wall_ms: Option<u64>,
    pub(crate) stop_method: Option<String>,
    pub(crate) expected_exit: bool,
    pub(crate) exit_status: Option<i32>,
    pub(crate) exit_observed_wall_ms: Option<u64>,
    pub(crate) failure: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CaptureSessionState {
    pub(crate) schema: String,
    pub(crate) run_id: String,
    pub(crate) run_root: String,
    pub(crate) package_name: String,
    pub(crate) device_serial: String,
    pub(crate) cli_version: String,
    pub(crate) transition_seq: u64,
    pub(crate) created_wall_ms: u64,
    pub(crate) updated_wall_ms: u64,
    pub(crate) lifecycle: LifecycleSnapshot,
    pub(crate) start_config: Value,
    pub(crate) artifacts: BTreeMap<String, ArtifactStatus>,
    pub(crate) processes: BTreeMap<String, ProcessDescriptor>,
    pub(crate) ime: Value,
    pub(crate) controller: Option<Value>,
    pub(crate) finalization: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FinalizationLedger {
    pub(crate) schema: String,
    pub(crate) run_id: String,
    pub(crate) run_state: LifecycleState,
    pub(crate) attempt_id: String,
    pub(crate) owner_pid: u32,
    pub(crate) owner_host: String,
    pub(crate) started_wall_ms: u64,
    pub(crate) finished_wall_ms: Option<u64>,
    pub(crate) failure_stage: Option<String>,
    pub(crate) failure_reasons: Vec<String>,
    pub(crate) cleanup_attempted: bool,
    pub(crate) cleanup_ok: bool,
    pub(crate) last_completed_step: Option<String>,
    pub(crate) steps: Vec<FinalizationStep>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct RepairResult {
    pub(crate) ok: bool,
    pub(crate) repair_action: String,
    pub(crate) run_id: String,
    pub(crate) package_name: String,
    pub(crate) device_serial: String,
    pub(crate) state_path: String,
    pub(crate) lock_path: String,
    pub(crate) previous_snapshot_paths: Vec<String>,
    pub(crate) reason: String,
    pub(crate) files_changed: Vec<String>,
}

impl LifecycleState {
    pub(crate) const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Starting => matches!(next, Self::ImeStarted | Self::Incomplete | Self::Aborted),
            Self::ImeStarted => {
                matches!(
                    next,
                    Self::VideoStarted | Self::GeteventStarted | Self::Incomplete
                )
            }
            Self::VideoStarted => matches!(next, Self::GeteventStarted | Self::Incomplete),
            Self::GeteventStarted => {
                matches!(
                    next,
                    Self::StartEvidenceCaptured
                        | Self::ControllerStarted
                        | Self::Active
                        | Self::Incomplete
                )
            }
            Self::StartEvidenceCaptured => {
                matches!(
                    next,
                    Self::ControllerStarted | Self::Active | Self::Incomplete
                )
            }
            Self::ControllerStarted => matches!(next, Self::Active | Self::Incomplete),
            Self::Active => {
                matches!(next, Self::StopRequested | Self::Incomplete | Self::Aborted)
            }
            Self::StopRequested => {
                matches!(next, Self::Stopping | Self::Finalizing | Self::Incomplete)
            }
            Self::Stopping => {
                matches!(
                    next,
                    Self::EndEvidenceCapturing | Self::Finalizing | Self::Incomplete
                )
            }
            Self::EndEvidenceCapturing => matches!(next, Self::Finalizing | Self::Incomplete),
            Self::Finalizing => matches!(next, Self::Complete | Self::Incomplete),
            Self::Complete | Self::Incomplete | Self::Aborted => false,
        }
    }

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Incomplete | Self::Aborted)
    }
}

impl ArtifactStatus {
    pub(crate) fn new(path: &str, requirement: Requirement, producer_step: &str) -> Self {
        Self {
            path: String::from(path),
            required: requirement.is_required(),
            present: false,
            valid: false,
            schema: None,
            fingerprint: None,
            producer_step: String::from(producer_step),
            failure_reason: None,
        }
    }

    pub(crate) const fn mark_valid(mut self) -> Self {
        self.present = true;
        self.valid = true;
        self
    }
}

impl FinalizationStep {
    pub(crate) fn new(name: &str, requirement: Requirement, status: StepStatus) -> Self {
        Self {
            name: String::from(name),
            required: requirement.is_required(),
            status,
            attempt_count: 0,
            can_retry: false,
            started_wall_ms: None,
            finished_wall_ms: None,
            message: None,
            error_code: None,
            cleanup: FinalizationStepCleanup {
                cleanup_attempted: false,
                cleanup_ok: false,
            },
            artifact_keys: Vec::new(),
        }
    }
}

impl StepStatus {
    const fn satisfies_required_step(self) -> bool {
        matches!(self, Self::Ok | Self::Skipped)
    }
}

impl Requirement {
    const fn is_required(self) -> bool {
        matches!(self, Self::Required)
    }
}

impl CommandErrorEnvelope {
    pub(crate) fn new(command: &str, error_code: SessionErrorCode, message: &str) -> Self {
        Self {
            schema: String::from(COMMAND_RESULT_SCHEMA),
            ok: false,
            command: String::from(command),
            error_code,
            message: String::from(message),
            classification: None,
            selector: Value::Null,
            observed: Value::Null,
            paths: Value::Null,
            suggested_next_command: None,
            mutated: false,
        }
    }
}

pub(crate) fn required_artifacts_complete(artifacts: &BTreeMap<String, ArtifactStatus>) -> bool {
    artifacts
        .values()
        .all(|artifact| !artifact.required || artifact.present && artifact.valid)
}

pub(crate) fn finalization_complete(
    steps: &[FinalizationStep],
    artifacts: &BTreeMap<String, ArtifactStatus>,
) -> bool {
    let steps_complete = steps
        .iter()
        .all(|step| !step.required || step.status.satisfies_required_step());
    steps_complete && required_artifacts_complete(artifacts)
}

pub(crate) fn profile_provenance_value_is_public_safe(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.iter().all(|(key, child)| match key.as_str() {
        "source" | "id" | "schema" | "hash" => public_safe_optional_string(child),
        "seed" | "parameter_count" => child.is_null() || child.as_u64().is_some(),
        _ => false,
    })
}

fn public_safe_optional_string(value: &Value) -> bool {
    value.is_null()
        || value
            .as_str()
            .is_some_and(|text| !string_has_private_location_shape(text))
}

fn string_has_private_location_shape(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with('/')
        || value.contains('\\')
        || lower.contains("://")
        || lower.contains("/users/")
        || lower.contains("/home/")
        || lower.contains("private-capture")
        || lower.contains("private_profile")
}
