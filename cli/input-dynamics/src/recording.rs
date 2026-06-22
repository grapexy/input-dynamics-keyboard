//! Read-only inspection for local recording directories.

use std::ffi::OsStr;
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::error::{CliError, CliResult};
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
    exists: bool,
}

#[derive(Default)]
struct RunSummaryInspection {
    stale_reasons: Vec<String>,
    exists: bool,
}

struct FlagInputs<'a> {
    manifest: Option<&'a Value>,
    session: &'a SessionSelection,
    validation: &'a ValidationInspection,
    run_summary: &'a RunSummaryInspection,
    timeline: &'a TimelineInspection,
    artifacts: &'a Value,
    note_flags: &'a Value,
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
    let session = select_session_jsonl(dir)?;
    let validation = inspect_validation(dir, &validation_path, external_run_id.as_deref());
    let run_summary = inspect_run_summary(dir)?;
    let timeline = inspect_timeline(dir)?;
    let artifacts = artifact_map(dir, &artifact_specs(dir, session.selected.as_deref()))?;
    let note_flags = note_flags(dir)?;
    let flag_inputs = FlagInputs {
        manifest: manifest_ref,
        session: &session,
        validation: &validation,
        run_summary: &run_summary,
        timeline: &timeline,
        artifacts: &artifacts,
        note_flags: &note_flags,
    };
    let flags = flags_json(&flag_inputs);
    let next_actions = next_actions(dir, external_run_id.as_deref(), &flags)?;
    let warnings = warnings(&session, &validation, &run_summary, &timeline);

    Ok(json!({
        "ok": true,
        "schema": INSPECTION_SCHEMA,
        "cli_version": env!("CARGO_PKG_VERSION"),
        "recording_dir": path_text(dir),
        "external_run_id": external_run_id,
        "package_name": string_at(manifest_ref, "/package_name"),
        "provenance": provenance_json(manifest_ref),
        "timing": timing_json(manifest_ref),
        "device": manifest
            .as_ref()
            .and_then(|value| value.get("device"))
            .cloned()
            .unwrap_or(Value::Null),
        "artifacts": artifacts,
        "session_jsonl": session_json(dir, &session),
        "validation": {
            "stored_present": validation.stored_present,
            "stored": validation.stored,
            "current": validation.current,
            "stale_reasons": validation.stale_reasons,
        },
        "run_summary": {
            "exists": run_summary.exists,
            "stale_reasons": run_summary.stale_reasons,
        },
        "timeline": {
            "exists": timeline.exists,
            "stale_reasons": timeline.stale_reasons,
        },
        "note_flags": note_flags,
        "flags": flags,
        "warnings": warnings,
        "next_actions": next_actions,
    }))
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
        });
    }
    let Some(index) = read_optional_json(&index_path)? else {
        return Ok(TimelineInspection {
            exists: false,
            stale_reasons: vec![String::from("timeline index is missing")],
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
    })
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
    specs.extend(adb_artifact_specs(dir));
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

fn derived_artifact_specs(dir: &Path) -> [ArtifactSpec; 6] {
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
    for entry_result in fs::read_dir(&ime_dir)? {
        let entry = entry_result?;
        let path = entry.path();
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
    let has_evidence = artifact_exists(inputs.artifacts, "evidence_start_index")
        || artifact_exists(inputs.artifacts, "evidence_end_index");
    let needs_derivation = !has_touch_gestures || !has_dismissals;
    let needs_run_summary =
        !inputs.run_summary.exists || !inputs.run_summary.stale_reasons.is_empty();
    let needs_timeline = !inputs.timeline.exists || !inputs.timeline.stale_reasons.is_empty();
    let incomplete_or_superseded = !inputs.validation.current_ok
        || inputs
            .note_flags
            .get("mentions_incomplete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || inputs
            .note_flags
            .get("mentions_superseded")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    json!({
        "valid_for_analysis": inputs.manifest.is_some()
            && inputs.session.selected.is_some()
            && has_getevent_jsonl
            && inputs.validation.current_ok,
        "needs_validation": !inputs.validation.stored_present
            || !inputs.validation.stale_reasons.is_empty(),
        "needs_press_summaries": !has_press_summaries,
        "needs_run_summary": needs_run_summary,
        "needs_derivation": needs_derivation,
        "needs_timeline": needs_timeline,
        "has_sensitive_evidence": has_evidence,
        "incomplete_or_superseded": incomplete_or_superseded,
        "needs_cleanup": needs_cleanup(inputs.manifest),
    })
}

fn next_actions(dir: &Path, external_run_id: Option<&str>, flags: &Value) -> CliResult<Value> {
    let mut actions = Vec::new();
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
    Ok(Value::Array(actions))
}

fn warnings(
    session: &SessionSelection,
    validation: &ValidationInspection,
    run_summary: &RunSummaryInspection,
    timeline: &TimelineInspection,
) -> Vec<String> {
    let mut warnings = session.warnings.clone();
    warnings.extend(validation.stale_reasons.iter().cloned());
    warnings.extend(run_summary.stale_reasons.iter().cloned());
    warnings.extend(timeline.stale_reasons.iter().cloned());
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
