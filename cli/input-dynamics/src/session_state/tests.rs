use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use serde_json::{Value, json};

use super::io::{
    SessionSelector, StaleEvidence, acquire_lock_exclusive, checked_update_json, classify_identity,
    classify_stale_evidence, read_json_classified, write_json_atomic,
};
use super::paths::{RunSessionPaths, RuntimeSessionPaths, sanitize_path_component};
use super::schema::{
    ArtifactStatus, CURRENT_SCHEMA, CaptureSessionCommand, CaptureSessionCommandName,
    CaptureSessionCurrent, CaptureSessionLock, CaptureSessionState, CommandErrorEnvelope,
    FINALIZATION_SCHEMA, FinalizationLedger, FinalizationStep, InputProvenance, LOCK_SCHEMA,
    LifecycleSnapshot, LifecycleState, LockState, ProcessDescriptor, ProcessKind, ProcessState,
    ProfileProvenance, ReadStatus, RepairResult, Requirement, STATE_SCHEMA, SessionErrorCode,
    StepStatus, finalization_complete, input_provenance_value_is_valid,
    profile_provenance_value_is_public_safe, required_artifacts_complete,
};

#[test]
fn runtime_paths_use_capture_session_namespace() {
    let base = PathBuf::from("/tmp/input-dynamics-test");
    let paths =
        RuntimeSessionPaths::from_base_dir(&base, "org.inputdynamics.ime/debug", "device/123");

    assert!(
        file_name(&paths.lock).ends_with(".capture-session.lock.json"),
        "lock path should use capture-session namespace"
    );
    assert!(
        file_name(&paths.current).ends_with(".capture-session.current.json"),
        "current pointer path should use capture-session namespace"
    );
    assert!(
        file_name(&paths.runs_dir).ends_with(".capture-session.runs"),
        "runs directory should use capture-session namespace"
    );
    assert!(
        file_name(&paths.lock).contains("org.inputdynamics.ime_debug-"),
        "lock path should keep a readable package prefix"
    );
    assert!(
        file_name(&paths.lock).contains("device_123-"),
        "lock path should keep a readable serial prefix"
    );
}

#[test]
fn runtime_paths_do_not_collide_after_sanitization() {
    let base = PathBuf::from("/tmp/input-dynamics-test");
    let first = RuntimeSessionPaths::from_base_dir(&base, "org.inputdynamics.ime", "device/123");
    let second = RuntimeSessionPaths::from_base_dir(&base, "org.inputdynamics.ime", "device_123");

    assert_ne!(
        first.lock, second.lock,
        "distinct raw serials should not share a lock path after sanitization"
    );
}

#[test]
fn run_paths_are_under_session_directory() {
    let paths = RunSessionPaths::from_run_dir(Path::new("/tmp/run-synthetic"));

    assert_eq!(
        paths.state,
        PathBuf::from("/tmp/run-synthetic/session/state.json"),
        "state should live under run-local session directory"
    );
    assert_eq!(
        paths.finalization,
        PathBuf::from("/tmp/run-synthetic/session/finalization.json"),
        "finalization should live under run-local session directory"
    );
    assert_eq!(
        paths.lock_snapshot,
        PathBuf::from("/tmp/run-synthetic/session/lock.snapshot.json"),
        "lock snapshot should live under run-local session directory"
    );
}

#[test]
fn path_component_sanitizer_is_stable() {
    assert_eq!(
        sanitize_path_component("org.inputdynamics.ime/debug device"),
        "org.inputdynamics.ime_debug_device",
        "path sanitizer should replace separators and spaces"
    );
    assert_eq!(
        sanitize_path_component(""),
        "default",
        "empty path components should be replaced"
    );
}

#[test]
fn schema_vocabulary_serializes_to_public_contract_names() {
    assert_eq!(
        FINALIZATION_SCHEMA, "input_dynamics_capture_session_finalization.v1",
        "finalization schema constant should stay stable"
    );
    assert_eq!(
        serde_json::to_value(LockState::StopRequested).unwrap_or(Value::Null),
        json!("stop_requested"),
        "lock states should serialize as snake_case"
    );
    assert_eq!(
        serde_json::to_value(ProcessState::NotStarted).unwrap_or(Value::Null),
        json!("not_started"),
        "process states should serialize as snake_case"
    );
    assert_eq!(
        serde_json::to_value(ReadStatus::UnsupportedSchema).unwrap_or(Value::Null),
        json!("unsupported_schema"),
        "read classifications should serialize as stable snake_case"
    );
    let provenance = ProfileProvenance {
        source: String::from("bundled"),
        id: Some(String::from("baseline-v1")),
        schema: Some(String::from("input_dynamics_profile.v1")),
        hash: Some(String::from("sha256:abc")),
        seed: Some(123_u64),
        parameter_count: Some(4_u64),
    };
    let provenance_json = serde_json::to_value(provenance).unwrap_or(Value::Null);
    assert!(
        profile_provenance_value_is_public_safe(&provenance_json),
        "profile provenance struct should serialize to public-safe fields"
    );
    let human_input = serde_json::to_value(InputProvenance::human()).unwrap_or(Value::Null);
    assert_eq!(
        human_input,
        json!({
            "input_actor": "human",
            "input_controller": null,
            "input_backend": null,
            "input_cadence_policy": "manual",
            "profile_provenance": null,
        }),
        "human input provenance should use explicit nulls for non-applicable generated-input fields"
    );
    let agent_input = serde_json::to_value(InputProvenance::agent(synthetic_profile_provenance()))
        .unwrap_or(Value::Null);
    assert_eq!(
        agent_input,
        json!({
            "input_actor": "agent",
            "input_controller": "input-dynamics-cli",
            "input_backend": "uinput",
            "input_cadence_policy": "input_profile",
            "profile_provenance": {
                "source": "bundled",
                "id": "baseline-v1",
                "schema": "input_dynamics_profile.v1",
                "hash": "sha256:abc",
                "seed": 123_u64,
                "parameter_count": 4_u64,
            },
        }),
        "agent input provenance should use the complete normalized umbrella actor vocabulary"
    );
    assert!(
        input_provenance_value_is_valid(&agent_input),
        "agent input provenance should satisfy nested state validation"
    );
}

#[test]
fn top_level_schema_structs_emit_expected_schema_fields() {
    let lock = synthetic_lock();
    let current = synthetic_current(&lock);
    let state = synthetic_state(&lock);
    let finalization = synthetic_finalization(&lock);
    let repair = synthetic_repair(&lock);
    let lock_json = serde_json::to_value(lock).unwrap_or(Value::Null);
    let current_json = serde_json::to_value(current).unwrap_or(Value::Null);
    let state_json = serde_json::to_value(state).unwrap_or(Value::Null);
    let finalization_json = serde_json::to_value(finalization).unwrap_or(Value::Null);
    let repair_json = serde_json::to_value(repair).unwrap_or(Value::Null);

    assert_eq!(
        lock_json.get("schema").and_then(Value::as_str),
        Some(LOCK_SCHEMA),
        "lock struct should carry lock schema"
    );
    assert_eq!(
        current_json.get("schema").and_then(Value::as_str),
        Some(CURRENT_SCHEMA),
        "current pointer struct should carry current schema"
    );
    assert_eq!(
        state_json.get("schema").and_then(Value::as_str),
        Some(STATE_SCHEMA),
        "state struct should carry state schema"
    );
    assert_eq!(
        finalization_json.get("schema").and_then(Value::as_str),
        Some(FINALIZATION_SCHEMA),
        "finalization struct should carry finalization schema"
    );
    assert_eq!(
        repair_json.get("repair_action").and_then(Value::as_str),
        Some("inspect"),
        "repair result should carry repair action"
    );
    assert_eq!(
        lock_json
            .get("command")
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str),
        Some("session start"),
        "lock should describe the canonical command that created it"
    );
    assert_eq!(
        lock_json
            .get("command")
            .and_then(|value| value.get("bounded"))
            .and_then(Value::as_bool),
        Some(false),
        "lock command should expose bounded workflow status"
    );
    assert_has_fields(
        first_step(&finalization_json),
        &[
            "name",
            "required",
            "status",
            "attempt_count",
            "can_retry",
            "started_wall_ms",
            "finished_wall_ms",
            "message",
            "error_code",
            "cleanup_attempted",
            "cleanup_ok",
            "artifact_keys",
        ],
    );
}

fn synthetic_lock() -> CaptureSessionLock {
    CaptureSessionLock {
        schema: String::from(LOCK_SCHEMA),
        lock_state: LockState::Active,
        observed_lifecycle_state: LifecycleState::Active,
        mutation_seq: 1_u64,
        package_name: String::from("org.inputdynamics.ime.debug"),
        device_serial: String::from("test-device"),
        run_id: String::from("run-synthetic-001"),
        command: CaptureSessionCommand {
            name: CaptureSessionCommandName::SessionStart,
            bounded: false,
        },
        output_dir: String::from("/tmp/input-dynamics-runs/run-synthetic-001"),
        state_path: String::from("/tmp/input-dynamics-runs/run-synthetic-001/session/state.json"),
        owner_pid: 1_u32,
        owner_host: String::from("synthetic-host"),
        invocation_id: String::from("synthetic-invocation"),
        created_wall_ms: 1_u64,
        updated_wall_ms: 2_u64,
        cli_version: String::from("0.1.0"),
        finalization_owner: None,
    }
}

fn synthetic_current(lock: &CaptureSessionLock) -> CaptureSessionCurrent {
    CaptureSessionCurrent {
        schema: String::from(CURRENT_SCHEMA),
        package_name: lock.package_name.clone(),
        device_serial: lock.device_serial.clone(),
        run_id: lock.run_id.clone(),
        output_dir: lock.output_dir.clone(),
        state_path: lock.state_path.clone(),
        lock_path: String::from("/tmp/input-dynamics-runtime/lock.json"),
        observed_lifecycle_state: LifecycleState::Active,
        observed_lock_state: Some(LockState::Active),
        updated_wall_ms: 2_u64,
    }
}

fn synthetic_state(lock: &CaptureSessionLock) -> CaptureSessionState {
    CaptureSessionState {
        schema: String::from(STATE_SCHEMA),
        run_id: lock.run_id.clone(),
        run_root: String::from("."),
        package_name: lock.package_name.clone(),
        device_serial: lock.device_serial.clone(),
        cli_version: lock.cli_version.clone(),
        transition_seq: 1_u64,
        created_wall_ms: 1_u64,
        updated_wall_ms: 2_u64,
        lifecycle: synthetic_lifecycle(),
        start_config: json!({}),
        input: InputProvenance::human(),
        artifacts: BTreeMap::new(),
        processes: BTreeMap::from([(String::from("screenrecord"), synthetic_process())]),
        ime: json!({}),
        controller: None,
        finalization: None,
    }
}

fn synthetic_agent_state(lock: &CaptureSessionLock) -> CaptureSessionState {
    let mut state = synthetic_state(lock);
    state.input = InputProvenance::agent(synthetic_profile_provenance());
    state
}

fn synthetic_lifecycle() -> LifecycleSnapshot {
    LifecycleSnapshot {
        state: LifecycleState::Active,
        stage: String::from("active"),
        history: Vec::new(),
    }
}

fn synthetic_profile_provenance() -> ProfileProvenance {
    ProfileProvenance {
        source: String::from("bundled"),
        id: Some(String::from("baseline-v1")),
        schema: Some(String::from("input_dynamics_profile.v1")),
        hash: Some(String::from("sha256:abc")),
        seed: Some(123_u64),
        parameter_count: Some(4_u64),
    }
}

fn synthetic_process() -> ProcessDescriptor {
    ProcessDescriptor {
        name: String::from("screenrecord"),
        kind: ProcessKind::AdbShell,
        required: true,
        state: ProcessState::Running,
        host_pid: Some(1_u32),
        host_process_group_id: Some(1_u32),
        remote_pid: None,
        argv: vec![String::from("adb")],
        remote_command: vec![String::from("screenrecord")],
        stdout: String::from("video/screenrecord.stdout.log"),
        stderr: String::from("video/screenrecord.stderr.log"),
        started_wall_ms: Some(1_u64),
        stop_method: None,
        expected_exit: false,
        exit_status: None,
        exit_observed_wall_ms: None,
        failure: None,
    }
}

fn synthetic_finalization(lock: &CaptureSessionLock) -> FinalizationLedger {
    FinalizationLedger {
        schema: String::from(FINALIZATION_SCHEMA),
        run_id: lock.run_id.clone(),
        run_state: LifecycleState::Incomplete,
        attempt_id: String::from("synthetic-finalization-001"),
        owner_pid: 1_u32,
        owner_host: String::from("synthetic-host"),
        started_wall_ms: 3_u64,
        finished_wall_ms: None,
        failure_stage: None,
        failure_reasons: Vec::new(),
        cleanup_attempted: false,
        cleanup_ok: false,
        last_completed_step: None,
        steps: vec![FinalizationStep::new(
            "write_manifest",
            Requirement::Required,
            StepStatus::Pending,
        )],
    }
}

fn synthetic_repair(lock: &CaptureSessionLock) -> RepairResult {
    RepairResult {
        ok: true,
        repair_action: String::from("inspect"),
        run_id: lock.run_id.clone(),
        package_name: lock.package_name.clone(),
        device_serial: lock.device_serial.clone(),
        state_path: lock.state_path.clone(),
        lock_path: String::from("/tmp/input-dynamics-runtime/lock.json"),
        previous_snapshot_paths: Vec::new(),
        reason: String::from("synthetic"),
        files_changed: Vec::new(),
    }
}

#[test]
fn read_json_classifies_missing_corrupt_unsupported_and_valid() {
    let root = unique_temp_dir("session-state-read");
    let missing = root.join("missing.json");
    let missing_result = read_json_classified(&missing, STATE_SCHEMA);
    assert_eq!(
        missing_result.status,
        ReadStatus::Missing,
        "missing state should be classified explicitly"
    );

    let corrupt = root.join("corrupt.json");
    assert!(
        fs::write(&corrupt, "{not-json").is_ok(),
        "test should write corrupt fixture"
    );
    let corrupt_result = read_json_classified(&corrupt, STATE_SCHEMA);
    assert_eq!(
        corrupt_result.status,
        ReadStatus::Corrupt,
        "invalid JSON should be corrupt"
    );

    let unsupported = root.join("unsupported.json");
    assert!(
        fs::write(&unsupported, json!({"schema": "bad.v9"}).to_string()).is_ok(),
        "test should write unsupported fixture"
    );
    let unsupported_result = read_json_classified(&unsupported, STATE_SCHEMA);
    assert_eq!(
        unsupported_result.status,
        ReadStatus::UnsupportedSchema,
        "wrong schema should be unsupported"
    );

    let valid = root.join("valid.json");
    let valid_json = synthetic_state_value();
    assert!(
        write_json_atomic(&valid, &valid_json).is_ok(),
        "atomic write should create valid fixture"
    );
    let valid_result = read_json_classified(&valid, STATE_SCHEMA);
    assert_eq!(
        valid_result.status,
        ReadStatus::Valid,
        "matching schema should be valid"
    );
    cleanup_dir(&root);
}

#[test]
fn read_json_requires_schema_identity_fields() {
    let root = unique_temp_dir("session-state-required-fields");
    let lock_path = root.join("lock.json");
    let state_path = root.join("state.json");
    let incomplete_lock = json!({"schema": LOCK_SCHEMA, "run_id": "run-synthetic-001"});
    let incomplete_state = json!({"schema": STATE_SCHEMA, "run_id": "run-synthetic-001"});

    assert!(
        write_json_atomic(&lock_path, &incomplete_lock).is_ok(),
        "test should write incomplete lock fixture"
    );
    assert!(
        write_json_atomic(&state_path, &incomplete_state).is_ok(),
        "test should write incomplete state fixture"
    );

    assert_eq!(
        read_json_classified(&lock_path, LOCK_SCHEMA).status,
        ReadStatus::Corrupt,
        "lock JSON missing required identity should be corrupt"
    );
    assert_eq!(
        read_json_classified(&state_path, STATE_SCHEMA).status,
        ReadStatus::Corrupt,
        "state JSON missing required identity should be corrupt"
    );
    cleanup_dir(&root);
}

#[test]
fn legacy_state_without_input_remains_readable_during_schema_transition() {
    let root = unique_temp_dir("session-state-legacy-input-transition");
    let path = root.join("state.json");
    let mut legacy_state = synthetic_state_value();
    if let Some(object) = legacy_state.as_object_mut() {
        object.remove("input");
    }

    assert!(
        write_json_atomic(&path, &legacy_state).is_ok(),
        "test should write legacy state fixture"
    );

    let classified = read_json_classified(&path, STATE_SCHEMA);
    assert_eq!(
        classified.status,
        ReadStatus::Valid,
        "unchanged .v1 states without input provenance should remain readable"
    );
    let decode_result =
        serde_json::from_value::<CaptureSessionState>(classified.value.unwrap_or(Value::Null));
    assert!(
        decode_result.is_ok(),
        "legacy state should deserialize with human input provenance default: {decode_result:?}"
    );
    if let Ok(decoded) = decode_result {
        assert_eq!(
            decoded.input,
            InputProvenance::human(),
            "typed legacy deserialization should default to human input provenance"
        );
    }
    cleanup_dir(&root);
}

#[test]
fn state_with_malformed_input_provenance_is_corrupt() {
    let root = unique_temp_dir("session-state-malformed-input");
    let bad_inputs = [
        json!({}),
        json!({
            "input_actor": "human",
            "input_controller": "input-dynamics-cli",
            "input_backend": null,
            "input_cadence_policy": "manual",
            "profile_provenance": null
        }),
        json!({
            "input_actor": "agent",
            "input_controller": "input-dynamics-cli",
            "input_backend": "uinput",
            "input_cadence_policy": "input_profile",
            "profile_provenance": null
        }),
        json!({
            "input_actor": "agent",
            "input_controller": "input-dynamics-cli",
            "input_backend": "uinput",
            "input_cadence_policy": "input_profile",
            "profile_provenance": {
                "source": "profiles/baseline-v1.json",
                "id": "baseline-v1",
                "schema": "input_dynamics_profile.v1",
                "hash": "sha256:abc",
                "seed": 123_u64,
                "parameter_count": 4_u64
            }
        }),
    ];

    for (index, input) in bad_inputs.iter().enumerate() {
        let path = root.join(format!("state-{index}.json"));
        let mut state = synthetic_state_value();
        if let Some(object) = state.as_object_mut() {
            object.insert(String::from("input"), input.clone());
        }
        assert!(
            write_json_atomic(&path, &state).is_ok(),
            "test should write malformed state fixture {index}"
        );
        let classified = read_json_classified(&path, STATE_SCHEMA);
        assert_eq!(
            classified.status,
            ReadStatus::Corrupt,
            "malformed input provenance should corrupt state fixture {index}"
        );
    }
    cleanup_dir(&root);
}

#[test]
fn identity_selector_mismatch_is_non_mutating_classification() {
    let state = json!({
        "schema": LOCK_SCHEMA,
        "package_name": "org.inputdynamics.ime.debug",
        "device_serial": "test-device",
        "run_id": "run-active",
        "output_dir": "/tmp/run-active",
        "state_path": "/tmp/run-active/session/state.json"
    });
    let selector = SessionSelector {
        run_id: Some(String::from("run-other")),
        package_name: Some(String::from("org.inputdynamics.ime.debug")),
        device_serial: Some(String::from("test-device")),
        output_dir: None,
        state_path: None,
    };

    let check = classify_identity(&state, &selector);

    assert_eq!(
        check.status,
        ReadStatus::Mismatched,
        "selector mismatch should be classified without mutation"
    );
    assert_eq!(
        check.mismatches,
        vec![String::from("run_id")],
        "mismatch should identify exact field"
    );
}

#[test]
fn identity_selector_checks_all_identity_fields() {
    let state = json!({
        "schema": LOCK_SCHEMA,
        "package_name": "org.inputdynamics.ime.debug",
        "device_serial": "test-device",
        "run_id": "run-active",
        "output_dir": "/tmp/run-active",
        "state_path": "/tmp/run-active/session/state.json"
    });
    let mismatches = [
        ("package_name", selector_with_package("org.other.ime")),
        ("device_serial", selector_with_serial("other-device")),
        ("run_id", selector_with_run("run-other")),
        ("output_dir", selector_with_output("/tmp/run-other")),
        (
            "state_path",
            selector_with_state_path("/tmp/run-other/session/state.json"),
        ),
    ];

    for (field, selector) in mismatches {
        let check = classify_identity(&state, &selector);
        assert_eq!(
            check.status,
            ReadStatus::Mismatched,
            "selector mismatch should be classified for {field}"
        );
        assert_eq!(
            check.mismatches,
            vec![String::from(field)],
            "mismatch should identify {field}"
        );
    }
}

#[test]
fn stale_evidence_is_explicit_and_non_age_based() {
    let no_evidence = StaleEvidence::default();
    let stale_owner = StaleEvidence {
        owner_process_missing: true,
        required_child_process_missing: false,
        state_file_missing: false,
    };

    assert_eq!(
        classify_stale_evidence(ReadStatus::Valid, &no_evidence),
        ReadStatus::Valid,
        "valid reads should not become stale without evidence"
    );
    assert_eq!(
        classify_stale_evidence(ReadStatus::Valid, &stale_owner),
        ReadStatus::Stale,
        "stale classification should require explicit evidence"
    );
    assert_eq!(
        classify_stale_evidence(ReadStatus::Corrupt, &stale_owner),
        ReadStatus::Corrupt,
        "corrupt state should not be reclassified as stale"
    );
}

#[test]
fn atomic_json_overwrites_complete_file() {
    let root = unique_temp_dir("session-state-atomic");
    let path = root.join("state.json");
    let first = value_with_string_field(synthetic_state_value(), "run_id", "one");
    let second = value_with_string_field(synthetic_state_value(), "run_id", "two");

    assert!(
        write_json_atomic(&path, &first).is_ok(),
        "first atomic write should succeed"
    );
    assert!(
        write_json_atomic(&path, &second).is_ok(),
        "second atomic write should replace the first"
    );
    let read = read_json_classified(&path, STATE_SCHEMA);

    assert_eq!(
        read.status,
        ReadStatus::Valid,
        "overwritten JSON should parse"
    );
    assert_eq!(
        read.value
            .as_ref()
            .and_then(|value| value.get("run_id"))
            .and_then(Value::as_str),
        Some("two"),
        "second write should be visible"
    );
    cleanup_dir(&root);
}

#[test]
fn atomic_json_creates_parent_and_writes_trailing_newline() {
    let root = unique_temp_dir("session-state-atomic-format");
    let path = root.join("nested").join("state.json");

    assert!(
        write_json_atomic(&path, &synthetic_state_value()).is_ok(),
        "atomic write should create parent directories"
    );
    let text = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        text.ends_with('\n'),
        "atomic JSON helper should write a trailing newline"
    );
    cleanup_dir(&root);
}

#[test]
fn exclusive_lock_acquire_fails_when_lock_exists() {
    let root = unique_temp_dir("session-state-lock");
    let path = root.join("lock.json");
    let lock = synthetic_lock_value();
    let competing_lock = value_with_string_field(synthetic_lock_value(), "run_id", "run-other");

    assert!(
        acquire_lock_exclusive(&path, &lock).is_ok(),
        "first lock acquisition should create the file"
    );
    assert!(
        acquire_lock_exclusive(&path, &competing_lock).is_err(),
        "second lock acquisition should fail"
    );
    assert_eq!(
        read_json_classified(&path, LOCK_SCHEMA)
            .value
            .and_then(|value| value
                .get("run_id")
                .and_then(Value::as_str)
                .map(String::from)),
        Some(String::from("run-synthetic-001")),
        "failed competing acquire must not replace existing lock"
    );
    cleanup_dir(&root);
}

#[test]
fn checked_update_rejects_stale_mutation_sequence() {
    let root = unique_temp_dir("session-state-checked-update");
    let path = root.join("lock.json");
    let original = value_with_u64_field(synthetic_lock_value(), "mutation_seq", 2_u64);
    let update = value_with_u64_field(synthetic_lock_value(), "mutation_seq", 3_u64);

    assert!(
        write_json_atomic(&path, &original).is_ok(),
        "test should write starting state"
    );
    assert!(
        checked_update_json(&path, LOCK_SCHEMA, "mutation_seq", 1_u64, &update).is_err(),
        "checked update should reject stale mutation sequence"
    );
    assert_eq!(
        read_sequence(&path, LOCK_SCHEMA, "mutation_seq"),
        Some(2_u64),
        "failed stale update must not mutate the lock"
    );
    assert!(
        checked_update_json(&path, LOCK_SCHEMA, "mutation_seq", 2_u64, &update).is_ok(),
        "checked update should accept current mutation sequence"
    );
    assert_eq!(
        read_sequence(&path, LOCK_SCHEMA, "mutation_seq"),
        Some(3_u64),
        "successful checked update should advance mutation sequence"
    );
    cleanup_dir(&root);
}

#[test]
fn checked_update_rejects_wrong_schema_and_bad_sequence_step() {
    let root = unique_temp_dir("session-state-checked-update-contract");
    let path = root.join("lock.json");
    let original = value_with_u64_field(synthetic_lock_value(), "mutation_seq", 2_u64);
    let skipped = value_with_u64_field(synthetic_lock_value(), "mutation_seq", 4_u64);
    let unsupported = json!({"schema": "bad.v9", "mutation_seq": 3_u64});

    assert!(
        write_json_atomic(&path, &original).is_ok(),
        "test should write starting state"
    );
    assert!(
        checked_update_json(&path, "bad.v9", "mutation_seq", 2_u64, &unsupported).is_err(),
        "checked update should reject unsupported schemas explicitly"
    );
    assert!(
        checked_update_json(&path, LOCK_SCHEMA, "mutation_seq", 2_u64, &skipped).is_err(),
        "checked update should require exactly one sequence advance"
    );
    assert_eq!(
        read_sequence(&path, LOCK_SCHEMA, "mutation_seq"),
        Some(2_u64),
        "failed checked updates must leave the original state"
    );
    cleanup_dir(&root);
}

#[test]
fn checked_update_supports_state_transition_sequence() {
    let root = unique_temp_dir("session-state-transition-update");
    let path = root.join("state.json");
    let original = value_with_u64_field(synthetic_state_value(), "transition_seq", 7_u64);
    let update = value_with_u64_field(synthetic_state_value(), "transition_seq", 8_u64);

    assert!(
        write_json_atomic(&path, &original).is_ok(),
        "test should write starting state"
    );
    assert!(
        checked_update_json(&path, STATE_SCHEMA, "transition_seq", 7_u64, &update).is_ok(),
        "state update should use transition_seq rather than mutation_seq"
    );
    cleanup_dir(&root);
}

#[test]
fn lifecycle_transition_table_matches_contract() {
    for state in all_lifecycle_states().iter().copied() {
        for next in all_lifecycle_states().iter().copied() {
            assert_eq!(
                state.can_transition_to(next),
                allowed_next_states(state).contains(&next),
                "unexpected lifecycle transition from {state:?} to {next:?}"
            );
        }
    }
    for terminal in [
        LifecycleState::Complete,
        LifecycleState::Incomplete,
        LifecycleState::Aborted,
    ] {
        assert!(
            terminal.is_terminal(),
            "terminal lifecycle state should report terminal"
        );
    }
}

#[test]
fn artifact_and_finalization_completeness_are_machine_checkable() {
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        String::from("manifest"),
        ArtifactStatus::new("manifest.json", Requirement::Required, "write_manifest").mark_valid(),
    );
    artifacts.insert(
        String::from("video_screen"),
        ArtifactStatus::new("video/screen.mp4", Requirement::Required, "stop_video"),
    );
    let steps = vec![
        FinalizationStep::new("write_manifest", Requirement::Required, StepStatus::Ok),
        FinalizationStep::new("stop_video", Requirement::Required, StepStatus::Ok),
    ];

    assert!(
        !required_artifacts_complete(&artifacts),
        "missing required artifact validity should block completion"
    );
    assert!(
        !finalization_complete(&steps, &artifacts),
        "finalization should depend on artifact validity"
    );
    let video = artifacts.get_mut("video_screen").map(|artifact| {
        artifact.present = true;
        artifact.valid = true;
    });
    assert!(
        video.is_some(),
        "test fixture should include video_screen artifact"
    );
    assert!(
        finalization_complete(&steps, &artifacts),
        "valid required artifacts and ok required steps should complete"
    );
}

#[test]
fn profile_provenance_rejects_private_profile_shapes() {
    let safe = json!({
        "source": "bundled",
        "id": "baseline-v1",
        "schema": "input_dynamics_profile.v1",
        "hash": "sha256:abc",
        "seed": 123_u64,
        "parameter_count": 4_u64
    });
    let unsafe_value = json!({
        "source": "local",
        "path": "/Users/example/private/profile.json",
        "parameters": {
            "hold_ms": {"distribution": "normal"}
        }
    });
    let path_under_allowed_key = json!({"source": "/absolute/profile/location.json"});
    let relative_path_under_allowed_key = json!({"source": "profiles/baseline-v1.json"});
    let learned_value = json!({"source": "bundled", "threshold_ms": 42_u64});
    let nested_distribution = json!({
        "source": "bundled",
        "hold_ms": {"distribution": "normal"}
    });

    assert!(
        profile_provenance_value_is_public_safe(&safe),
        "public provenance summary should be allowed"
    );
    assert!(
        !profile_provenance_value_is_public_safe(&unsafe_value),
        "profile definitions and local paths should be rejected"
    );
    assert!(
        !profile_provenance_value_is_public_safe(&path_under_allowed_key),
        "allowed string fields should still reject path-like values"
    );
    assert!(
        !profile_provenance_value_is_public_safe(&relative_path_under_allowed_key),
        "allowed string fields should reject relative path-like values"
    );
    assert!(
        !profile_provenance_value_is_public_safe(&learned_value),
        "unknown learned-value fields should be rejected"
    );
    assert!(
        !profile_provenance_value_is_public_safe(&nested_distribution),
        "nested profile definitions should be rejected"
    );
}

#[test]
fn command_error_envelope_has_agent_branch_fields() {
    let error = CommandErrorEnvelope::new(
        "session stop",
        SessionErrorCode::SelectorMismatch,
        "active session run id does not match supplied run id",
    );
    let serialized = serde_json::to_value(error).unwrap_or(Value::Null);

    assert_eq!(
        serialized.get("schema").and_then(Value::as_str),
        Some(super::schema::COMMAND_RESULT_SCHEMA),
        "error envelope should include schema"
    );
    assert_eq!(
        serialized.get("ok").and_then(Value::as_bool),
        Some(false),
        "error envelope should be machine-branchable"
    );
    assert_eq!(
        serialized.get("error_code").and_then(Value::as_str),
        Some("selector_mismatch"),
        "error code should be stable snake_case"
    );
    assert_eq!(
        serialized.get("mutated").and_then(Value::as_bool),
        Some(false),
        "selector errors should default to non-mutating"
    );
}

#[test]
fn public_fixture_sanitizer_rejects_private_fragments() {
    let lock = synthetic_lock();
    let safe = json!({
        "schema": CURRENT_SCHEMA,
        "package_name": "org.inputdynamics.ime.debug",
        "device_serial": "test-device",
        "run_id": "run-synthetic-001"
    });
    let unsafe_value = json!({
        "path": "private_capture_root/run-private/media_frame.bin"
    });
    let fixtures = [
        serde_json::to_value(lock.clone()).unwrap_or(Value::Null),
        serde_json::to_value(synthetic_current(&lock)).unwrap_or(Value::Null),
        serde_json::to_value(synthetic_state(&lock)).unwrap_or(Value::Null),
        serde_json::to_value(synthetic_agent_state(&lock)).unwrap_or(Value::Null),
        serde_json::to_value(synthetic_finalization(&lock)).unwrap_or(Value::Null),
        serde_json::to_value(synthetic_repair(&lock)).unwrap_or(Value::Null),
        serde_json::to_value(CommandErrorEnvelope::new(
            "session stop",
            SessionErrorCode::SelectorMismatch,
            "synthetic selector mismatch",
        ))
        .unwrap_or(Value::Null),
    ];

    assert!(
        public_fixture_is_sanitized(&safe),
        "synthetic fixture should pass sanitizer"
    );
    for fixture in fixtures {
        assert!(
            public_fixture_is_sanitized(&fixture),
            "all synthetic public fixtures should pass sanitizer: {fixture:?}"
        );
    }
    assert!(
        !public_fixture_is_sanitized(&unsafe_value),
        "private capture artifacts should fail sanitizer"
    );
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(String::new, String::from)
}

fn synthetic_lock_value() -> Value {
    serde_json::to_value(synthetic_lock()).unwrap_or(Value::Null)
}

fn synthetic_state_value() -> Value {
    serde_json::to_value(synthetic_state(&synthetic_lock())).unwrap_or(Value::Null)
}

fn value_with_string_field(mut value: Value, field: &str, text: &str) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert(String::from(field), json!(text));
    }
    value
}

fn value_with_u64_field(mut value: Value, field: &str, number: u64) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert(String::from(field), json!(number));
    }
    value
}

fn selector_with_package(package_name: &str) -> SessionSelector {
    SessionSelector {
        package_name: Some(String::from(package_name)),
        ..SessionSelector::default()
    }
}

fn selector_with_serial(device_serial: &str) -> SessionSelector {
    SessionSelector {
        device_serial: Some(String::from(device_serial)),
        ..SessionSelector::default()
    }
}

fn selector_with_run(run_id: &str) -> SessionSelector {
    SessionSelector {
        run_id: Some(String::from(run_id)),
        ..SessionSelector::default()
    }
}

fn selector_with_output(output_dir: &str) -> SessionSelector {
    SessionSelector {
        output_dir: Some(String::from(output_dir)),
        ..SessionSelector::default()
    }
}

fn selector_with_state_path(state_path: &str) -> SessionSelector {
    SessionSelector {
        state_path: Some(String::from(state_path)),
        ..SessionSelector::default()
    }
}

fn read_sequence(path: &Path, schema: &str, field: &str) -> Option<u64> {
    read_json_classified(path, schema)
        .value
        .and_then(|value| value.get(field).and_then(Value::as_u64))
}

fn first_step(finalization: &Value) -> Option<&Value> {
    finalization
        .get("steps")
        .and_then(Value::as_array)
        .and_then(|steps| steps.first())
}

fn assert_has_fields(optional_value: Option<&Value>, fields: &[&str]) {
    let Some(serialized_value) = optional_value else {
        assert!(
            fields.is_empty(),
            "serialized value should include at least one step"
        );
        return;
    };
    for field in fields {
        assert!(
            serialized_value.get(*field).is_some(),
            "serialized value should include field {field}"
        );
    }
}

fn all_lifecycle_states() -> &'static [LifecycleState] {
    &[
        LifecycleState::Starting,
        LifecycleState::ImeStarted,
        LifecycleState::VideoStarted,
        LifecycleState::GeteventStarted,
        LifecycleState::StartEvidenceCaptured,
        LifecycleState::ControllerStarted,
        LifecycleState::Active,
        LifecycleState::StopRequested,
        LifecycleState::Stopping,
        LifecycleState::EndEvidenceCapturing,
        LifecycleState::Finalizing,
        LifecycleState::Complete,
        LifecycleState::Incomplete,
        LifecycleState::Aborted,
    ]
}

fn allowed_next_states(state: LifecycleState) -> &'static [LifecycleState] {
    match state {
        LifecycleState::Starting => &[
            LifecycleState::ImeStarted,
            LifecycleState::Incomplete,
            LifecycleState::Aborted,
        ],
        LifecycleState::ImeStarted => &[
            LifecycleState::VideoStarted,
            LifecycleState::GeteventStarted,
            LifecycleState::Incomplete,
        ],
        LifecycleState::VideoStarted => {
            &[LifecycleState::GeteventStarted, LifecycleState::Incomplete]
        }
        LifecycleState::GeteventStarted => &[
            LifecycleState::StartEvidenceCaptured,
            LifecycleState::ControllerStarted,
            LifecycleState::Active,
            LifecycleState::Incomplete,
        ],
        LifecycleState::StartEvidenceCaptured => &[
            LifecycleState::ControllerStarted,
            LifecycleState::Active,
            LifecycleState::Incomplete,
        ],
        LifecycleState::ControllerStarted => &[LifecycleState::Active, LifecycleState::Incomplete],
        LifecycleState::Active => &[
            LifecycleState::StopRequested,
            LifecycleState::Incomplete,
            LifecycleState::Aborted,
        ],
        LifecycleState::StopRequested => &[
            LifecycleState::Stopping,
            LifecycleState::Finalizing,
            LifecycleState::Incomplete,
        ],
        LifecycleState::Stopping => &[
            LifecycleState::EndEvidenceCapturing,
            LifecycleState::Finalizing,
            LifecycleState::Incomplete,
        ],
        LifecycleState::EndEvidenceCapturing => {
            &[LifecycleState::Finalizing, LifecycleState::Incomplete]
        }
        LifecycleState::Finalizing => &[LifecycleState::Complete, LifecycleState::Incomplete],
        LifecycleState::Complete | LifecycleState::Incomplete | LifecycleState::Aborted => &[],
    }
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("input-dynamics-{label}-{}", process::id()));
    cleanup_dir(&root);
    let create_result = fs::create_dir_all(&root);
    assert!(
        create_result.is_ok(),
        "test temp dir should be created: {create_result:?}"
    );
    root
}

fn cleanup_dir(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) | Err(_) => {}
    }
}

fn public_fixture_is_sanitized(value: &Value) -> bool {
    let forbidden = [
        "/Users/",
        "/home/",
        "\\Users\\",
        "\\home\\",
        "private_capture_root",
        "run-private",
        "raw_event_log",
        "accessibility_dump",
        "screenshot_frame",
        "video_frame",
        "typed_text",
        "learned_profile_value",
    ];
    !json_contains_forbidden_string(value, &forbidden)
}

fn json_contains_forbidden_string(value: &Value, forbidden: &[&str]) -> bool {
    if let Some(text) = value.as_str() {
        return forbidden.iter().any(|fragment| text.contains(fragment));
    }
    if let Some(values) = value.as_array() {
        return values
            .iter()
            .any(|child| json_contains_forbidden_string(child, forbidden));
    }
    if let Some(object) = value.as_object() {
        return object
            .values()
            .any(|child| json_contains_forbidden_string(child, forbidden));
    }
    false
}
