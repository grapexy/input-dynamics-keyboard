//! Atomic JSON IO and read classification for capture-session state.

#![cfg_attr(not(test), allow(dead_code))]

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::error::{CliError, CliResult};

use super::schema::{
    CURRENT_SCHEMA, FINALIZATION_SCHEMA, LOCK_SCHEMA, ReadStatus, STATE_SCHEMA, SessionErrorCode,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClassifiedJson {
    pub(crate) status: ReadStatus,
    pub(crate) path: PathBuf,
    pub(crate) expected_schema: String,
    pub(crate) observed_schema: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) value: Option<Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SessionSelector {
    pub(crate) package_name: Option<String>,
    pub(crate) device_serial: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) output_dir: Option<String>,
    pub(crate) state_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IdentityCheck {
    pub(crate) status: ReadStatus,
    pub(crate) mismatches: Vec<String>,
    pub(crate) malformed: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StaleEvidence {
    pub(crate) owner_process_missing: bool,
    pub(crate) required_child_process_missing: bool,
    pub(crate) state_file_missing: bool,
}

pub(crate) fn write_json_atomic(path: &Path, value: &Value) -> CliResult<()> {
    ensure_parent_dir(path)?;
    let (temp_path, mut temp_file) = create_temp_file(path)?;
    let write_result = write_json_to_file(&mut temp_file, value);
    if let Err(error) = write_result {
        remove_temp_file(&temp_path);
        return Err(error);
    }
    fs::rename(&temp_path, path).map_err(|error| {
        remove_temp_file(&temp_path);
        CliError::from(error)
    })?;
    sync_parent_dir(path);
    Ok(())
}

pub(crate) fn acquire_lock_exclusive(path: &Path, value: &Value) -> CliResult<()> {
    ensure_parent_dir(path)?;
    let (temp_path, mut temp_file) = create_temp_file(path)?;
    let write_result = write_json_to_file(&mut temp_file, value);
    if let Err(error) = write_result {
        remove_temp_file(&temp_path);
        return Err(error);
    }
    let guard = acquire_file_update_guard(path, "create").inspect_err(|_error| {
        remove_temp_file(&temp_path);
    })?;
    let final_path_exists = match path_exists(path) {
        Ok(exists) => exists,
        Err(error) => {
            remove_temp_file(&temp_path);
            drop(guard);
            return Err(error);
        }
    };
    if final_path_exists {
        remove_temp_file(&temp_path);
        drop(guard);
        return Err(active_session_exists_error(path));
    }
    fs::rename(&temp_path, path).map_err(|error| {
        remove_temp_file(&temp_path);
        CliError::from(error)
    })?;
    drop(guard);
    sync_parent_dir(path);
    Ok(())
}

pub(crate) fn checked_update_json(
    path: &Path,
    expected_schema: &str,
    sequence_field: &str,
    expected_sequence: u64,
    value: &Value,
) -> CliResult<()> {
    validate_expected_schema(expected_schema)?;
    validate_replacement_schema(value, expected_schema)?;
    let guard = acquire_file_update_guard(path, "update")?;
    let current = read_json_classified(path, expected_schema);
    if current.status != ReadStatus::Valid {
        drop(guard);
        return Err(CliError::with_details(
            "cannot update invalid capture session JSON",
            json!({
                "error_code": SessionErrorCode::StateCorrupt,
                "path": path.to_string_lossy(),
                "status": current.status,
            }),
        ));
    }
    let observed = current
        .value
        .as_ref()
        .and_then(|json_value| json_value.get(sequence_field))
        .and_then(Value::as_u64);
    if observed != Some(expected_sequence) {
        drop(guard);
        return Err(CliError::with_details(
            "capture session mutation sequence changed",
            json!({
                "error_code": SessionErrorCode::SequenceMismatch,
                "sequence_field": sequence_field,
                "expected_sequence": expected_sequence,
                "observed_sequence": observed,
            }),
        ));
    }
    let expected_next = expected_sequence
        .checked_add(1_u64)
        .ok_or_else(|| CliError::new("capture session sequence cannot advance"))?;
    let replacement_sequence = value.get(sequence_field).and_then(Value::as_u64);
    if replacement_sequence != Some(expected_next) {
        drop(guard);
        return Err(CliError::with_details(
            "capture session replacement sequence must advance by one",
            json!({
                "error_code": SessionErrorCode::SequenceMismatch,
                "sequence_field": sequence_field,
                "expected_replacement_sequence": expected_next,
                "observed_replacement_sequence": replacement_sequence,
            }),
        ));
    }
    let result = write_json_atomic(path, value);
    drop(guard);
    result
}

pub(crate) fn read_json_classified(path: &Path, expected_schema: &str) -> ClassifiedJson {
    let text = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return classified(path, expected_schema, ReadStatus::Missing);
        }
        Err(error) => {
            let mut result = classified(path, expected_schema, ReadStatus::IoError);
            result.message = Some(error.to_string());
            return result;
        }
    };
    let value = match serde_json::from_str::<Value>(text.trim()) {
        Ok(parsed) => parsed,
        Err(error) => {
            let mut result = classified(path, expected_schema, ReadStatus::Corrupt);
            result.message = Some(error.to_string());
            return result;
        }
    };
    let observed_schema = value_schema(&value);
    let Some(observed) = observed_schema.as_deref() else {
        let mut result = classified(path, expected_schema, ReadStatus::Corrupt);
        result.message = Some(String::from("missing schema"));
        result.value = Some(value);
        return result;
    };
    if observed != expected_schema {
        let mut result = classified(path, expected_schema, ReadStatus::UnsupportedSchema);
        result.observed_schema = observed_schema;
        result.value = Some(value);
        return result;
    }
    if !supported_schema(expected_schema) {
        let mut result = classified(path, expected_schema, ReadStatus::UnsupportedSchema);
        result.observed_schema = observed_schema;
        result.value = Some(value);
        return result;
    }
    let malformed = malformed_required_fields(&value, expected_schema);
    if !malformed.is_empty() {
        let mut result = classified(path, expected_schema, ReadStatus::Corrupt);
        result.message = Some(format!(
            "missing or malformed required fields: {}",
            malformed.join(", ")
        ));
        result.observed_schema = observed_schema;
        result.value = Some(value);
        return result;
    }
    let mut result = classified(path, expected_schema, ReadStatus::Valid);
    result.observed_schema = observed_schema;
    result.value = Some(value);
    result
}

pub(crate) fn classify_identity(value: &Value, selector: &SessionSelector) -> IdentityCheck {
    let mut mismatches = Vec::new();
    let mut malformed = Vec::new();
    compare_field(
        value,
        "package_name",
        selector.package_name.as_deref(),
        &mut mismatches,
        &mut malformed,
    );
    compare_field(
        value,
        "device_serial",
        selector.device_serial.as_deref(),
        &mut mismatches,
        &mut malformed,
    );
    compare_field(
        value,
        "run_id",
        selector.run_id.as_deref(),
        &mut mismatches,
        &mut malformed,
    );
    compare_field(
        value,
        "output_dir",
        selector.output_dir.as_deref(),
        &mut mismatches,
        &mut malformed,
    );
    compare_field(
        value,
        "state_path",
        selector.state_path.as_deref(),
        &mut mismatches,
        &mut malformed,
    );
    let status = if !malformed.is_empty() {
        ReadStatus::Corrupt
    } else if !mismatches.is_empty() {
        ReadStatus::Mismatched
    } else {
        ReadStatus::Valid
    };
    IdentityCheck {
        status,
        mismatches,
        malformed,
    }
}

pub(crate) fn classify_stale_evidence(status: ReadStatus, evidence: &StaleEvidence) -> ReadStatus {
    if status == ReadStatus::Valid && evidence.indicates_stale_state() {
        ReadStatus::Stale
    } else {
        status
    }
}

impl StaleEvidence {
    const fn indicates_stale_state(&self) -> bool {
        self.owner_process_missing || self.required_child_process_missing || self.state_file_missing
    }
}

fn ensure_parent_dir(path: &Path) -> CliResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn acquire_file_update_guard(path: &Path, operation: &str) -> CliResult<FileUpdateGuard> {
    ensure_parent_dir(path)?;
    let guard_path = guard_path_for(path, operation);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&guard_path)
        .map_err(|error| {
            if error.kind() == ErrorKind::AlreadyExists {
                CliError::with_details(
                    "capture session state update is already in progress",
                    json!({
                        "error_code": SessionErrorCode::FinalizationInProgress,
                        "guard_path": guard_path.to_string_lossy(),
                    }),
                )
            } else {
                CliError::from(error)
            }
        })?;
    Ok(FileUpdateGuard {
        path: guard_path,
        file,
    })
}

struct FileUpdateGuard {
    path: PathBuf,
    file: File,
}

impl Drop for FileUpdateGuard {
    fn drop(&mut self) {
        let sync_result = self.file.sync_all();
        match sync_result {
            Ok(()) | Err(_) => {}
        }
        remove_temp_file(&self.path);
    }
}

fn active_session_exists_error(path: &Path) -> CliError {
    let existing = read_json_classified(path, LOCK_SCHEMA);
    CliError::with_details(
        "capture session lock already exists",
        json!({
            "error_code": SessionErrorCode::ActiveSessionExists,
            "lock_path": path.to_string_lossy(),
            "existing_status": existing.status,
            "observed_schema": existing.observed_schema,
        }),
    )
}

fn validate_expected_schema(expected_schema: &str) -> CliResult<()> {
    if supported_schema(expected_schema) {
        Ok(())
    } else {
        Err(CliError::with_details(
            "unsupported capture session schema",
            json!({
                "error_code": SessionErrorCode::UnsupportedSchema,
                "expected_schema": expected_schema,
            }),
        ))
    }
}

fn validate_replacement_schema(value: &Value, expected_schema: &str) -> CliResult<()> {
    if value_schema(value).as_deref() == Some(expected_schema) {
        Ok(())
    } else {
        Err(CliError::with_details(
            "replacement JSON uses the wrong capture session schema",
            json!({
                "error_code": SessionErrorCode::UnsupportedSchema,
                "expected_schema": expected_schema,
                "observed_schema": value_schema(value),
            }),
        ))
    }
}

fn path_exists(path: &Path) -> CliResult<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn create_temp_file(path: &Path) -> CliResult<(PathBuf, File)> {
    for attempt in 0_u8..16_u8 {
        let temp_path = temp_path_for(path, attempt);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(CliError::new("failed to create unique temporary JSON file"))
}

fn temp_path_for(path: &Path, attempt: u8) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}-{attempt}",
        process::id(),
        wall_time_ms()
    ))
}

fn guard_path_for(path: &Path, operation: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    path.with_file_name(format!(".{file_name}.{operation}.guard"))
}

fn write_json_to_file(file: &mut File, value: &Value) -> CliResult<()> {
    let text = serde_json::to_string_pretty(value)?;
    file.write_all(text.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn sync_parent_dir(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Ok(directory) = File::open(parent) {
        match directory.sync_all() {
            Ok(()) | Err(_) => {}
        }
    }
}

fn remove_temp_file(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) | Err(_) => {}
    }
}

fn classified(path: &Path, expected_schema: &str, status: ReadStatus) -> ClassifiedJson {
    ClassifiedJson {
        status,
        path: path.to_path_buf(),
        expected_schema: String::from(expected_schema),
        observed_schema: None,
        message: None,
        value: None,
    }
}

fn value_schema(value: &Value) -> Option<String> {
    value
        .get("schema")
        .and_then(Value::as_str)
        .map(String::from)
}

fn supported_schema(schema: &str) -> bool {
    matches!(
        schema,
        STATE_SCHEMA | LOCK_SCHEMA | CURRENT_SCHEMA | FINALIZATION_SCHEMA
    )
}

fn malformed_required_fields(value: &Value, schema: &str) -> Vec<String> {
    let Some(fields) = required_fields_for_schema(schema) else {
        return Vec::new();
    };
    let mut malformed = Vec::new();
    require_string_fields(value, fields.strings, &mut malformed);
    require_u64_fields(value, fields.u64s, &mut malformed);
    require_object_fields(value, fields.objects, &mut malformed);
    malformed
}

struct RequiredFields {
    strings: &'static [&'static str],
    u64s: &'static [&'static str],
    objects: &'static [&'static str],
}

fn required_fields_for_schema(schema: &str) -> Option<RequiredFields> {
    match schema {
        LOCK_SCHEMA => Some(RequiredFields {
            strings: &[
                "package_name",
                "device_serial",
                "run_id",
                "output_dir",
                "state_path",
            ],
            u64s: &["mutation_seq"],
            objects: &["command"],
        }),
        CURRENT_SCHEMA => Some(RequiredFields {
            strings: &[
                "package_name",
                "device_serial",
                "run_id",
                "output_dir",
                "state_path",
                "lock_path",
            ],
            u64s: &[],
            objects: &[],
        }),
        STATE_SCHEMA => Some(RequiredFields {
            strings: &["run_id", "run_root", "package_name", "device_serial"],
            u64s: &["transition_seq"],
            objects: &["lifecycle", "artifacts", "processes"],
        }),
        FINALIZATION_SCHEMA => Some(RequiredFields {
            strings: &["run_id", "attempt_id"],
            u64s: &["started_wall_ms"],
            objects: &[],
        }),
        _ => None,
    }
}

fn require_string_fields(value: &Value, fields: &[&str], malformed: &mut Vec<String>) {
    for field in fields {
        match value.get(*field).and_then(Value::as_str) {
            Some(text) if !text.is_empty() => {}
            Some(_) | None => malformed.push(String::from(*field)),
        }
    }
}

fn require_u64_fields(value: &Value, fields: &[&str], malformed: &mut Vec<String>) {
    for field in fields {
        if value.get(*field).and_then(Value::as_u64).is_none() {
            malformed.push(String::from(*field));
        }
    }
}

fn require_object_fields(value: &Value, fields: &[&str], malformed: &mut Vec<String>) {
    for field in fields {
        if !value.get(*field).is_some_and(Value::is_object) {
            malformed.push(String::from(*field));
        }
    }
}

fn compare_field(
    value: &Value,
    field: &str,
    expected: Option<&str>,
    mismatches: &mut Vec<String>,
    malformed: &mut Vec<String>,
) {
    let Some(expected_value) = expected else {
        return;
    };
    match value.get(field).and_then(Value::as_str) {
        Some(observed) if observed == expected_value => {}
        Some(_) => mismatches.push(String::from(field)),
        None => malformed.push(String::from(field)),
    }
}

fn wall_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_millis())
}
