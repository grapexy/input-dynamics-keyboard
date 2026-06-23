//! Cross-source timeline derivation for a recorded input-dynamics run.

use std::cmp::Ordering;
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::clock::{AlignmentStatus, ClockDomain};
use crate::derivation::jsonl::write_jsonl;
use crate::derivation::{
    DeriveError, DeriveResult, TIMELINE_EVENT_SCHEMA, TIMELINE_INDEX_SCHEMA, find_ime_jsonl,
    path_text, read_manifest, string_at, string_field,
};

const ORDER_METHOD: &str = "best_effort_clock_domain_milliseconds_then_source_order";
const CLOCK_ALIGNMENT_STATUS: &str = AlignmentStatus::NotEstimated.as_str();
const IME_SOURCE_RANK: u8 = 10;
const TOUCH_GESTURE_SOURCE_RANK: u8 = 20;
const DISMISSAL_SOURCE_RANK: u8 = 30;
const VIDEO_SOURCE_RANK: u8 = 40;
const EVIDENCE_START_SOURCE_RANK: u8 = 50;
const EVIDENCE_END_SOURCE_RANK: u8 = 60;

/// Configuration for deriving a cross-source recording timeline.
#[derive(Clone, Debug)]
pub struct DeriveTimelineConfig {
    /// Recording directory created by `input-dynamics record`.
    pub recording_dir: PathBuf,
    /// IME session JSONL path. Defaults to the single `ime/session-*.jsonl`.
    pub ime_jsonl: Option<PathBuf>,
    /// Derived touch gesture JSONL path. Defaults under `recording_dir`.
    pub touch_gestures_jsonl: Option<PathBuf>,
    /// Derived dismissal inference JSONL path. Defaults under `recording_dir`.
    pub dismissals_jsonl: Option<PathBuf>,
    /// Timeline output directory. Defaults to `derived/timeline`.
    pub output_dir: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct TimelinePaths {
    ime_jsonl: PathBuf,
    touch_gestures_jsonl: PathBuf,
    dismissals_jsonl: PathBuf,
    video_timing_json: PathBuf,
    evidence_start_index: PathBuf,
    evidence_end_index: PathBuf,
    output_dir: PathBuf,
    index_output: PathBuf,
    events_output: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TimelineOrder {
    group: u8,
    time_ms: Option<i64>,
    source_rank: u8,
    source_line_index: Option<u64>,
}

#[derive(Clone, Debug)]
struct TimelineRecord {
    order: TimelineOrder,
    value: Value,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SourceKind {
    ImeJsonl,
    TouchGestures,
    Dismissals,
    VideoTiming,
    EvidenceStart,
    EvidenceEnd,
}

#[derive(Clone, Copy, Debug)]
enum SourceRequirement {
    Required,
    Optional,
}

#[derive(Debug)]
struct OptionalRecords {
    path: PathBuf,
    records: Vec<LineRecord>,
}

#[derive(Clone, Debug)]
struct EvidenceRecord {
    kind: SourceKind,
    value: Option<Value>,
}

#[derive(Clone, Debug)]
struct LineRecord {
    line_index: u64,
    value: Value,
}

struct TimelineIndexInputs<'a> {
    config: &'a DeriveTimelineConfig,
    paths: &'a TimelinePaths,
    manifest: &'a Value,
    ime_records: &'a [LineRecord],
    touch_records: Option<&'a OptionalRecords>,
    dismissal_records: Option<&'a OptionalRecords>,
    video_timing: Option<&'a Value>,
    evidence_records: &'a [EvidenceRecord],
    warnings: &'a [String],
    event_count: usize,
}

impl TimelinePaths {
    fn from_config(config: &DeriveTimelineConfig) -> DeriveResult<Self> {
        let derived_dir = config.recording_dir.join("derived");
        let output_dir = config
            .output_dir
            .clone()
            .unwrap_or_else(|| derived_dir.join("timeline"));
        let ime_jsonl = match config.ime_jsonl.clone() {
            Some(path) => path,
            None => find_ime_jsonl(&config.recording_dir)?,
        };
        let touch_gestures_jsonl = config
            .touch_gestures_jsonl
            .clone()
            .unwrap_or_else(|| derived_dir.join("touch_gestures.jsonl"));
        let dismissals_jsonl = config
            .dismissals_jsonl
            .clone()
            .unwrap_or_else(|| derived_dir.join("dismissal_inferences.jsonl"));
        let index_output = output_dir.join("index.json");
        let events_output = output_dir.join("events.jsonl");
        Ok(Self {
            ime_jsonl,
            touch_gestures_jsonl,
            dismissals_jsonl,
            video_timing_json: config.recording_dir.join("video").join("timing.json"),
            evidence_start_index: config
                .recording_dir
                .join("evidence")
                .join("start")
                .join("index.json"),
            evidence_end_index: config
                .recording_dir
                .join("evidence")
                .join("end")
                .join("index.json"),
            output_dir,
            index_output,
            events_output,
        })
    }
}

impl Ord for TimelineOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        self.group
            .cmp(&other.group)
            .then_with(|| optional_time_order(self.time_ms, other.time_ms))
            .then_with(|| self.source_rank.cmp(&other.source_rank))
            .then_with(|| self.source_line_index.cmp(&other.source_line_index))
    }
}

impl PartialOrd for TimelineOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Derive a cross-source event timeline for a recording.
pub fn derive_timeline(config: &DeriveTimelineConfig) -> DeriveResult<Value> {
    let paths = TimelinePaths::from_config(config)?;
    let manifest = read_manifest(&config.recording_dir)?;
    let ime_records = read_jsonl_with_line_indexes(&paths.ime_jsonl)?;
    let touch_records = read_optional_jsonl(&paths.touch_gestures_jsonl)?;
    let dismissal_records = read_optional_jsonl(&paths.dismissals_jsonl)?;
    let video_timing = read_optional_json(&paths.video_timing_json)?;
    let evidence_records = read_evidence_records(&paths)?;
    let mut warnings = Vec::new();
    collect_missing_warning(
        &mut warnings,
        touch_records.as_ref(),
        "derived touch gestures are not present; run derive dismissals first if gesture rows are needed",
    );
    collect_missing_warning(
        &mut warnings,
        dismissal_records.as_ref(),
        "derived dismissal inferences are not present; run derive dismissals first if inference rows are needed",
    );
    collect_missing_evidence_warnings(&mut warnings, &evidence_records);

    let mut timeline = Vec::new();
    append_ime_records(
        &mut timeline,
        &config.recording_dir,
        &paths.ime_jsonl,
        &ime_records,
    );
    append_optional_jsonl_records(
        &mut timeline,
        &config.recording_dir,
        SourceKind::TouchGestures,
        touch_records.as_ref(),
    )?;
    append_optional_jsonl_records(
        &mut timeline,
        &config.recording_dir,
        SourceKind::Dismissals,
        dismissal_records.as_ref(),
    )?;
    append_video_records(
        &mut timeline,
        &config.recording_dir,
        &paths.video_timing_json,
        video_timing.as_ref(),
    );
    append_evidence_records(&mut timeline, &config.recording_dir, &evidence_records)?;
    let timeline_values = finalize_timeline_values(timeline)?;

    fs::create_dir_all(&paths.output_dir)?;
    write_jsonl(&paths.events_output, &timeline_values)?;
    let index = timeline_index(&TimelineIndexInputs {
        config,
        paths: &paths,
        manifest: &manifest,
        ime_records: &ime_records,
        touch_records: touch_records.as_ref(),
        dismissal_records: dismissal_records.as_ref(),
        video_timing: video_timing.as_ref(),
        evidence_records: &evidence_records,
        warnings: &warnings,
        event_count: timeline_values.len(),
    })?;
    write_json_file(&paths.index_output, &index)?;
    Ok(json!({
        "ok": true,
        "schema": "input_dynamics_derivation_summary.v1",
        "derivation": "timeline",
        "recording_dir": path_text(&config.recording_dir),
        "output_dir": path_text(&paths.output_dir),
        "index_output": path_text(&paths.index_output),
        "events_output": path_text(&paths.events_output),
        "event_count": timeline_values.len(),
        "warnings": warnings,
    }))
}

fn finalize_timeline_values(mut timeline: Vec<TimelineRecord>) -> DeriveResult<Vec<Value>> {
    timeline.sort_by(|left, right| left.order.cmp(&right.order));
    timeline
        .into_iter()
        .enumerate()
        .map(|(index, mut record)| {
            let event_index = u64::try_from(index)
                .map_err(|error| {
                    DeriveError::new(format!("timeline event index overflow: {error}"))
                })?
                .checked_add(1)
                .ok_or_else(|| DeriveError::new("timeline event index overflow"))?;
            insert_timeline_id(&mut record.value, event_index);
            Ok(record.value)
        })
        .collect()
}

fn append_ime_records(
    timeline: &mut Vec<TimelineRecord>,
    recording_dir: &Path,
    path: &Path,
    records: &[LineRecord],
) {
    for record in records {
        if !is_timeline_ime_event(&record.value) {
            continue;
        }
        let Some(event_name) = string_field(&record.value, "event") else {
            continue;
        };
        let value = json!({
            "schema": TIMELINE_EVENT_SCHEMA,
            "record_kind": "ime_event",
            "event": event_name,
            "source_layer": "raw_ime",
            "source": "ime_jsonl",
            "source_ref": source_ref(recording_dir, path, record.line_index, extra_refs(&record.value)),
            "clock_domain": ClockDomain::AndroidUptimeMs.as_str(),
            "ordering": ordering_json(ORDER_METHOD, CLOCK_ALIGNMENT_STATUS),
            "t_uptime_ms": record.value.get("t_uptime_ms").cloned().unwrap_or(Value::Null),
            "t_wall_ms": record.value.get("t_wall_ms").cloned().unwrap_or(Value::Null),
            "external_run_id": record.value.get("external_run_id").cloned().unwrap_or(Value::Null),
            "session_id": record.value.get("session_id").cloned().unwrap_or(Value::Null),
            "package_name": record.value.get("package_name").cloned().unwrap_or(Value::Null),
            "target_package": record.value.get("target_package").cloned().unwrap_or(Value::Null),
            "password_field": record.value.get("password_field").cloned().unwrap_or(Value::Null),
            "press_id": record.value.get("press_id").cloned().unwrap_or(Value::Null),
            "gesture_id": record.value.get("gesture_id").cloned().unwrap_or(Value::Null),
            "key": key_summary(&record.value),
        });
        timeline.push(TimelineRecord {
            order: TimelineOrder {
                group: 1,
                time_ms: record.value.get("t_uptime_ms").and_then(Value::as_i64),
                source_rank: IME_SOURCE_RANK,
                source_line_index: Some(record.line_index),
            },
            value,
        });
    }
}

fn append_optional_jsonl_records(
    timeline: &mut Vec<TimelineRecord>,
    recording_dir: &Path,
    kind: SourceKind,
    records: Option<&OptionalRecords>,
) -> DeriveResult<()> {
    let Some(optional_records) = records else {
        return Ok(());
    };
    for record in &optional_records.records {
        let value = match kind {
            SourceKind::TouchGestures => touch_gesture_timeline_record(
                recording_dir,
                &optional_records.path,
                record.line_index,
                &record.value,
            ),
            SourceKind::Dismissals => dismissal_timeline_record(
                recording_dir,
                &optional_records.path,
                record.line_index,
                &record.value,
            ),
            SourceKind::ImeJsonl
            | SourceKind::VideoTiming
            | SourceKind::EvidenceStart
            | SourceKind::EvidenceEnd => {
                return Err(DeriveError::new("unsupported optional JSONL source kind"));
            }
        };
        timeline.push(TimelineRecord {
            order: timeline_order_for(kind, record.line_index, &record.value),
            value,
        });
    }
    Ok(())
}

fn touch_gesture_timeline_record(
    recording_dir: &Path,
    path: &Path,
    line_index: u64,
    record: &Value,
) -> Value {
    json!({
        "schema": TIMELINE_EVENT_SCHEMA,
        "record_kind": "touch_gesture",
        "event": record.get("event").cloned().unwrap_or_else(|| json!("touch_gesture")),
        "source_layer": "derived",
        "source": "derived_touch_gesture",
        "source_ref": source_ref(recording_dir, path, line_index, extra_refs(record)),
        "clock_domain": ClockDomain::KernelGeteventUs.as_str(),
        "ordering": ordering_json(ORDER_METHOD, CLOCK_ALIGNMENT_STATUS),
        "external_run_id": record.get("external_run_id").cloned().unwrap_or(Value::Null),
        "session_id": record.get("session_id").cloned().unwrap_or(Value::Null),
        "package_name": record.get("package_name").cloned().unwrap_or(Value::Null),
        "gesture_id": record.get("gesture_id").cloned().unwrap_or(Value::Null),
        "classification": record.get("classification").cloned().unwrap_or(Value::Null),
        "classification_confidence": record
            .get("classification_confidence")
            .cloned()
            .unwrap_or(Value::Null),
        "edge_side": record.get("edge_side").cloned().unwrap_or(Value::Null),
        "start": compact_touch_endpoint(record, "start"),
        "end": compact_touch_endpoint(record, "end"),
        "delta": record.get("delta").cloned().unwrap_or(Value::Null),
    })
}

fn dismissal_timeline_record(
    recording_dir: &Path,
    path: &Path,
    line_index: u64,
    record: &Value,
) -> Value {
    json!({
        "schema": TIMELINE_EVENT_SCHEMA,
        "record_kind": "dismissal_inference",
        "event": record.get("event").cloned().unwrap_or_else(|| json!("dismissal_inference")),
        "source_layer": "derived",
        "source": "derived_dismissal_inference",
        "source_ref": source_ref(recording_dir, path, line_index, extra_refs(record)),
        "clock_domain": ClockDomain::AndroidUptimeMs.as_str(),
        "ordering": ordering_json(ORDER_METHOD, CLOCK_ALIGNMENT_STATUS),
        "inference_id": record.get("inference_id").cloned().unwrap_or(Value::Null),
        "inferred_dismissal": record.get("inferred_dismissal").cloned().unwrap_or(Value::Null),
        "confidence": record.get("confidence").cloned().unwrap_or(Value::Null),
        "time_delta_ms": record.get("time_delta_ms").cloned().unwrap_or(Value::Null),
        "time_delta_status": record.get("time_delta_status").cloned().unwrap_or(Value::Null),
        "clock_alignment_status": record.get("clock_alignment_status").cloned().unwrap_or(Value::Null),
        "clock_alignment": record.get("clock_alignment").cloned().unwrap_or(Value::Null),
        "observed_ime_event": record.get("observed_ime_event").cloned().unwrap_or(Value::Null),
        "target_package": record.get("target_package").cloned().unwrap_or(Value::Null),
        "evidence": record.get("evidence").cloned().unwrap_or(Value::Null),
    })
}

fn append_video_records(
    timeline: &mut Vec<TimelineRecord>,
    recording_dir: &Path,
    path: &Path,
    timing: Option<&Value>,
) {
    let Some(timing_record) = timing else {
        return;
    };
    for phase in ["start", "stop"] {
        let Some(phase_record) = timing_record.get(phase) else {
            continue;
        };
        let value = json!({
            "schema": TIMELINE_EVENT_SCHEMA,
            "record_kind": "video_marker",
            "event": format!("video_{phase}"),
            "source_layer": "video",
            "source": "video_timing",
            "source_ref": {
                "path": relative_path_text(recording_dir, path),
                "phase": phase,
            },
            "clock_domain": ClockDomain::HostWallMs.as_str(),
            "clock_alignment_status": AlignmentStatus::LegacyWallClockBracketed.as_str(),
            "ordering": ordering_json(ORDER_METHOD, AlignmentStatus::LegacyWallClockBracketed.as_str()),
            "phase": phase,
            "remote_path": timing_record.get("remote_path").cloned().unwrap_or(Value::Null),
            "local_path": timing_record.get("local_path").cloned().unwrap_or(Value::Null),
            "marker": phase_record,
            "file": timing_record.get("file").cloned().unwrap_or(Value::Null),
        });
        timeline.push(TimelineRecord {
            order: TimelineOrder {
                group: video_order_group(phase),
                time_ms: video_order_time_ms(phase_record),
                source_rank: VIDEO_SOURCE_RANK,
                source_line_index: None,
            },
            value,
        });
    }
}

fn append_evidence_records(
    timeline: &mut Vec<TimelineRecord>,
    recording_dir: &Path,
    records: &[EvidenceRecord],
) -> DeriveResult<()> {
    for evidence_record in records {
        let Some(record) = evidence_record.value.as_ref() else {
            continue;
        };
        let kind = evidence_record.kind;
        let phase = evidence_phase(kind)?;
        let path = evidence_index_path(recording_dir, kind)?;
        let value = json!({
            "schema": TIMELINE_EVENT_SCHEMA,
            "record_kind": "evidence_bundle",
            "event": "evidence_bundle",
            "source_layer": "evidence",
            "source": "observation_bundle",
            "source_ref": {
                "path": relative_path_text(recording_dir, &path),
                "phase": phase,
            },
            "clock_domain": "host_wall_ms",
            "ordering": ordering_json(ORDER_METHOD, CLOCK_ALIGNMENT_STATUS),
            "captured_wall_ms": record.get("captured_wall_ms").cloned().unwrap_or(Value::Null),
            "phase": phase,
            "package_name": record.get("package_name").cloned().unwrap_or(Value::Null),
            "artifacts": record.get("artifacts").cloned().unwrap_or(Value::Null),
            "state_ok": record.pointer("/state/ok").cloned().unwrap_or(Value::Null),
        });
        timeline.push(TimelineRecord {
            order: TimelineOrder {
                group: evidence_order_group(kind)?,
                time_ms: record.get("captured_wall_ms").and_then(Value::as_i64),
                source_rank: source_rank(kind),
                source_line_index: None,
            },
            value,
        });
    }
    Ok(())
}

fn timeline_index(inputs: &TimelineIndexInputs<'_>) -> DeriveResult<Value> {
    let mut sources = vec![
        source_index(
            SourceKind::ImeJsonl,
            &inputs.config.recording_dir,
            &inputs.paths.ime_jsonl,
            Some(inputs.ime_records.len()),
            SourceRequirement::Required,
        )?,
        source_index(
            SourceKind::TouchGestures,
            &inputs.config.recording_dir,
            &inputs.paths.touch_gestures_jsonl,
            inputs.touch_records.map(|records| records.records.len()),
            SourceRequirement::Optional,
        )?,
        source_index(
            SourceKind::Dismissals,
            &inputs.config.recording_dir,
            &inputs.paths.dismissals_jsonl,
            inputs
                .dismissal_records
                .map(|records| records.records.len()),
            SourceRequirement::Optional,
        )?,
        source_index(
            SourceKind::VideoTiming,
            &inputs.config.recording_dir,
            &inputs.paths.video_timing_json,
            inputs.video_timing.map(|_record| 2_usize),
            SourceRequirement::Optional,
        )?,
    ];
    for evidence_record in inputs.evidence_records {
        let path = evidence_index_path(&inputs.config.recording_dir, evidence_record.kind)?;
        sources.push(source_index(
            evidence_record.kind,
            &inputs.config.recording_dir,
            &path,
            evidence_record.value.as_ref().map(|_value| 1_usize),
            SourceRequirement::Optional,
        )?);
    }
    Ok(json!({
        "ok": true,
        "schema": TIMELINE_INDEX_SCHEMA,
        "recording_dir": path_text(&inputs.config.recording_dir),
        "external_run_id": string_at(inputs.manifest, "/external_run_id"),
        "package_name": string_at(inputs.manifest, "/package_name"),
        "index_output": path_text(&inputs.paths.index_output),
        "events_output": path_text(&inputs.paths.events_output),
        "event_count": inputs.event_count,
        "sources": sources,
        "ordering": {
            "method": ORDER_METHOD,
            "clock_alignment_status": CLOCK_ALIGNMENT_STATUS,
            "notes": [
                "source clock domains are preserved",
                "cross-domain ordering is a best-effort inspection view, not clock-aligned ground truth"
            ],
        },
        "warnings": inputs.warnings,
    }))
}

fn source_index(
    kind: SourceKind,
    recording_dir: &Path,
    path: &Path,
    record_count: Option<usize>,
    requirement: SourceRequirement,
) -> DeriveResult<Value> {
    let exists = path.exists();
    let fingerprint = if exists {
        file_fingerprint(path)?
    } else {
        Value::Null
    };
    Ok(json!({
        "kind": source_kind_name(kind),
        "path": relative_path_text(recording_dir, path),
        "required": matches!(requirement, SourceRequirement::Required),
        "exists": exists,
        "record_count": record_count,
        "fingerprint": fingerprint,
    }))
}

fn read_jsonl_with_line_indexes(path: &Path) -> DeriveResult<Vec<LineRecord>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for (zero_index, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }
        let line_index = u64::try_from(zero_index)
            .map_err(|error| DeriveError::new(format!("line index overflow: {error}")))?
            .checked_add(1)
            .ok_or_else(|| DeriveError::new("line index overflow"))?;
        records.push(LineRecord {
            line_index,
            value: serde_json::from_str(&line)?,
        });
    }
    Ok(records)
}

fn read_optional_jsonl(path: &Path) -> DeriveResult<Option<OptionalRecords>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(OptionalRecords {
        path: path.to_path_buf(),
        records: read_jsonl_with_line_indexes(path)?,
    }))
}

fn read_evidence_records(paths: &TimelinePaths) -> DeriveResult<Vec<EvidenceRecord>> {
    Ok(vec![
        EvidenceRecord {
            kind: SourceKind::EvidenceStart,
            value: read_optional_json(&paths.evidence_start_index)?,
        },
        EvidenceRecord {
            kind: SourceKind::EvidenceEnd,
            value: read_optional_json(&paths.evidence_end_index)?,
        },
    ])
}

fn read_optional_json(path: &Path) -> DeriveResult<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&text)?))
}

fn write_json_file(path: &Path, value: &Value) -> DeriveResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn insert_timeline_id(value: &mut Value, event_index: u64) {
    if let &mut Value::Object(ref mut object) = value {
        object.insert(
            String::from("timeline_event_id"),
            json!(format!("timeline:{event_index:06}")),
        );
    }
}

fn is_timeline_ime_event(record: &Value) -> bool {
    let Some(event) = record.get("event").and_then(Value::as_str) else {
        return false;
    };
    event == "session_start"
        || event == "session_stop"
        || event.starts_with("field_")
        || event.starts_with("ime_")
        || event.starts_with("input_view_")
        || event.starts_with("keyboard_")
        || event.starts_with("key_")
        || event.starts_with("editor_")
}

fn timeline_order_for(kind: SourceKind, line_index: u64, record: &Value) -> TimelineOrder {
    TimelineOrder {
        group: 1,
        time_ms: order_time_ms(kind, record),
        source_rank: source_rank(kind),
        source_line_index: Some(line_index),
    }
}

fn order_time_ms(kind: SourceKind, record: &Value) -> Option<i64> {
    match kind {
        SourceKind::ImeJsonl => record.get("t_uptime_ms").and_then(Value::as_i64),
        SourceKind::TouchGestures => record
            .pointer("/start/t_getevent_ms")
            .and_then(Value::as_i64),
        SourceKind::Dismissals => dismissal_order_ms(record),
        SourceKind::VideoTiming | SourceKind::EvidenceStart | SourceKind::EvidenceEnd => None,
    }
}

fn dismissal_order_ms(record: &Value) -> Option<i64> {
    record
        .get("evidence")
        .and_then(Value::as_array)?
        .iter()
        .find(|evidence| evidence.get("kind").and_then(Value::as_str) == Some("ime_event"))
        .and_then(|evidence| evidence.get("t_uptime_ms").and_then(Value::as_i64))
}

fn evidence_order_group(kind: SourceKind) -> DeriveResult<u8> {
    match kind {
        SourceKind::EvidenceStart => Ok(0),
        SourceKind::EvidenceEnd => Ok(2),
        SourceKind::ImeJsonl
        | SourceKind::TouchGestures
        | SourceKind::Dismissals
        | SourceKind::VideoTiming => Err(DeriveError::new("unsupported evidence order group")),
    }
}

const fn source_rank(kind: SourceKind) -> u8 {
    match kind {
        SourceKind::ImeJsonl => IME_SOURCE_RANK,
        SourceKind::TouchGestures => TOUCH_GESTURE_SOURCE_RANK,
        SourceKind::Dismissals => DISMISSAL_SOURCE_RANK,
        SourceKind::VideoTiming => VIDEO_SOURCE_RANK,
        SourceKind::EvidenceStart => EVIDENCE_START_SOURCE_RANK,
        SourceKind::EvidenceEnd => EVIDENCE_END_SOURCE_RANK,
    }
}

const fn source_kind_name(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::ImeJsonl => "ime_jsonl",
        SourceKind::TouchGestures => "derived_touch_gestures",
        SourceKind::Dismissals => "derived_dismissal_inferences",
        SourceKind::VideoTiming => "video_timing",
        SourceKind::EvidenceStart => "evidence_start",
        SourceKind::EvidenceEnd => "evidence_end",
    }
}

fn evidence_phase(kind: SourceKind) -> DeriveResult<&'static str> {
    match kind {
        SourceKind::EvidenceStart => Ok("start"),
        SourceKind::EvidenceEnd => Ok("end"),
        SourceKind::ImeJsonl
        | SourceKind::TouchGestures
        | SourceKind::Dismissals
        | SourceKind::VideoTiming => Err(DeriveError::new("unsupported evidence phase")),
    }
}

fn evidence_index_path(recording_dir: &Path, kind: SourceKind) -> DeriveResult<PathBuf> {
    match kind {
        SourceKind::EvidenceStart => Ok(recording_dir
            .join("evidence")
            .join("start")
            .join("index.json")),
        SourceKind::EvidenceEnd => Ok(recording_dir
            .join("evidence")
            .join("end")
            .join("index.json")),
        SourceKind::ImeJsonl
        | SourceKind::TouchGestures
        | SourceKind::Dismissals
        | SourceKind::VideoTiming => Err(DeriveError::new("unsupported evidence index path")),
    }
}

fn video_order_group(phase: &str) -> u8 {
    if phase == "start" { 0 } else { 2 }
}

fn video_order_time_ms(record: &Value) -> Option<i64> {
    record
        .get("host_wall_ms_before_device_timestamp")
        .and_then(Value::as_i64)
}

fn optional_time_order(left: Option<i64>, right: Option<i64>) -> Ordering {
    match (left, right) {
        (Some(left_value), Some(right_value)) => left_value.cmp(&right_value),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn source_ref(recording_dir: &Path, path: &Path, line_index: u64, extra: Value) -> Value {
    let mut object = Map::new();
    object.insert(
        String::from("path"),
        json!(relative_path_text(recording_dir, path)),
    );
    object.insert(String::from("line_index"), json!(line_index));
    if let Value::Object(extra_object) = extra {
        object.extend(extra_object);
    }
    Value::Object(object)
}

fn extra_refs(record: &Value) -> Value {
    let mut object = Map::new();
    insert_if_present(&mut object, record, "session_id");
    insert_if_present(&mut object, record, "press_id");
    insert_if_present(&mut object, record, "gesture_id");
    insert_if_present(&mut object, record, "inference_id");
    Value::Object(object)
}

fn insert_if_present(object: &mut Map<String, Value>, record: &Value, field: &str) {
    if let Some(value) = record.get(field) {
        object.insert(field.to_owned(), value.clone());
    }
}

fn key_summary(record: &Value) -> Value {
    if !record
        .get("event")
        .and_then(Value::as_str)
        .is_some_and(|event| event.starts_with("key_"))
    {
        return Value::Null;
    }
    json!({
        "code": record.get("key_code").cloned().unwrap_or(Value::Null),
        "label": record.get("key_label").cloned().unwrap_or(Value::Null),
        "class": record.get("key_class").cloned().unwrap_or(Value::Null),
        "x_screen_px": record.get("x_screen_px").cloned().unwrap_or(Value::Null),
        "y_screen_px": record.get("y_screen_px").cloned().unwrap_or(Value::Null),
    })
}

fn compact_touch_endpoint(record: &Value, field: &str) -> Value {
    let Some(endpoint) = record.get(field) else {
        return Value::Null;
    };
    json!({
        "line_index": endpoint.get("line_index").cloned().unwrap_or(Value::Null),
        "t_getevent_us": endpoint.get("t_getevent_us").cloned().unwrap_or(Value::Null),
        "t_getevent_ms": endpoint.get("t_getevent_ms").cloned().unwrap_or(Value::Null),
        "x_px": endpoint.get("x_px").cloned().unwrap_or(Value::Null),
        "y_px": endpoint.get("y_px").cloned().unwrap_or(Value::Null),
        "pressure": endpoint.get("pressure").cloned().unwrap_or(Value::Null),
        "touch_major": endpoint.get("touch_major").cloned().unwrap_or(Value::Null),
        "touch_minor": endpoint.get("touch_minor").cloned().unwrap_or(Value::Null),
        "orientation": endpoint.get("orientation").cloned().unwrap_or(Value::Null),
    })
}

fn ordering_json(method: &str, clock_alignment_status: &str) -> Value {
    json!({
        "method": method,
        "clock_alignment_status": clock_alignment_status,
    })
}

fn relative_path_text(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .map_or_else(|_strip_error| path_text(path), path_text)
}

fn file_fingerprint(path: &Path) -> DeriveResult<Value> {
    let metadata = fs::metadata(path)?;
    Ok(json!({
        "byte_count": metadata.len(),
        "modified_wall_ms": modified_wall_ms(&metadata)?,
        "sha256": format!("sha256:{}", sha256_file(path)?),
    }))
}

fn modified_wall_ms(metadata: &fs::Metadata) -> DeriveResult<Option<u64>> {
    let modified = metadata.modified()?;
    let duration = match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration,
        Err(_time_error) => return Ok(None),
    };
    Ok(Some(u64::try_from(duration.as_millis()).map_err(
        |error| DeriveError::new(format!("modified time overflow: {error}")),
    )?))
}

fn sha256_file(path: &Path) -> DeriveResult<String> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> DeriveResult<String> {
    let capacity = bytes
        .len()
        .checked_mul(2)
        .ok_or_else(|| DeriveError::new("hex capacity overflow"))?;
    let mut output = String::with_capacity(capacity);
    for byte in bytes {
        write!(&mut output, "{byte:02x}")
            .map_err(|error| DeriveError::new(format!("failed to format digest: {error}")))?;
    }
    Ok(output)
}

fn collect_missing_warning(
    warnings: &mut Vec<String>,
    records: Option<&OptionalRecords>,
    text: &str,
) {
    if records.is_none() {
        warnings.push(text.to_owned());
    }
}

fn collect_missing_evidence_warnings(warnings: &mut Vec<String>, records: &[EvidenceRecord]) {
    for evidence_record in records {
        if evidence_record.value.is_some() {
            continue;
        }
        let source = source_kind_name(evidence_record.kind);
        warnings.push(format!("{source} observation bundle is not present"));
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fmt::Debug;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::{Value, json};

    use crate::derivation::timeline::{DeriveTimelineConfig, derive_timeline};

    type TestResult<T> = Result<T, Box<dyn Error>>;

    #[test]
    fn derives_timeline_with_semantic_rows_and_evidence_refs() {
        let root = unique_temp_dir("timeline-complete");
        let Some(()) = assert_ok(create_complete_fixture(&root), "create fixture") else {
            return;
        };

        let derive_result = derive_timeline(&DeriveTimelineConfig {
            recording_dir: root.clone(),
            ime_jsonl: None,
            touch_gestures_jsonl: None,
            dismissals_jsonl: None,
            output_dir: None,
        });
        let Some(_summary) = assert_ok(derive_result, "derive timeline") else {
            return;
        };

        let timeline_dir = root.join("derived").join("timeline");
        let Some(output) = assert_ok(
            read_jsonl(&timeline_dir.join("events.jsonl")),
            "read timeline events",
        ) else {
            return;
        };
        assert_eq!(output.len(), 7, "pointer_sample should be excluded");
        assert_eq!(
            output
                .first()
                .and_then(|record| record.get("record_kind"))
                .and_then(Value::as_str),
            Some("evidence_bundle"),
            "start evidence is anchored first"
        );
        assert!(
            output.iter().any(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("touch_gesture")
                    && record
                        .pointer("/source_ref/gesture_id")
                        .and_then(Value::as_str)
                        == Some("gesture-1")
                    && record.get("clock_domain").and_then(Value::as_str)
                        == Some("kernel_getevent_us")
            }),
            "touch gesture source refs and canonical clock domain should be present"
        );
        assert!(
            output.iter().any(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("dismissal_inference")
                    && record.get("clock_alignment_status").and_then(Value::as_str)
                        == Some("unsupported_clock_domain")
                    && record.get("time_delta_status").and_then(Value::as_str)
                        == Some("legacy_mixed_clock_heuristic")
                    && record
                        .pointer("/clock_alignment/status")
                        .and_then(Value::as_str)
                        == Some("unsupported_clock_domain")
            }),
            "dismissal timeline rows should preserve clock-safety metadata"
        );
        let Some(index) = assert_ok(read_json(&timeline_dir.join("index.json")), "read index")
        else {
            return;
        };
        assert_eq!(
            index.get("event_count").and_then(Value::as_u64),
            Some(7_u64),
            "index should count timeline rows"
        );
        assert_eq!(
            index
                .pointer("/ordering/clock_alignment_status")
                .and_then(Value::as_str),
            Some("not_estimated"),
            "clock alignment should be explicit"
        );
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
    }

    #[test]
    fn derives_timeline_with_missing_optional_sources() {
        let root = unique_temp_dir("timeline-missing-optional");
        let ime_dir = root.join("ime");
        let Some(()) = assert_ok(fs::create_dir_all(&ime_dir), "create ime dir") else {
            return;
        };
        let Some(()) = assert_ok(
            write_jsonl(
                &ime_dir.join("session-test.jsonl"),
                &[json!({
                    "event": "session_start",
                    "session_id": "session-test",
                    "t_uptime_ms": 100_i64,
                })],
            ),
            "write ime fixture",
        ) else {
            return;
        };

        let derive_result = derive_timeline(&DeriveTimelineConfig {
            recording_dir: root.clone(),
            ime_jsonl: None,
            touch_gestures_jsonl: None,
            dismissals_jsonl: None,
            output_dir: None,
        });
        let Some(summary) = assert_ok(derive_result, "derive timeline") else {
            return;
        };
        let warnings = summary
            .get("warnings")
            .and_then(Value::as_array)
            .map_or(0_usize, Vec::len);
        assert_eq!(
            warnings, 4_usize,
            "two derived files and two evidence bundles"
        );
        let Some(output) = assert_ok(
            read_jsonl(&root.join("derived").join("timeline").join("events.jsonl")),
            "read timeline events",
        ) else {
            return;
        };
        assert_eq!(output.len(), 1_usize, "IME row should still be written");
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
    }

    #[test]
    fn derives_timeline_with_video_markers() {
        let root = unique_temp_dir("timeline-video");
        let Some(()) = assert_ok(create_complete_fixture(&root), "create fixture") else {
            return;
        };
        let Some(()) = assert_ok(create_video_fixture(&root), "create video fixture") else {
            return;
        };

        let derive_result = derive_timeline(&DeriveTimelineConfig {
            recording_dir: root.clone(),
            ime_jsonl: None,
            touch_gestures_jsonl: None,
            dismissals_jsonl: None,
            output_dir: None,
        });
        let Some(_summary) = assert_ok(derive_result, "derive timeline") else {
            return;
        };

        let timeline_dir = root.join("derived").join("timeline");
        let Some(output) = assert_ok(
            read_jsonl(&timeline_dir.join("events.jsonl")),
            "read timeline events",
        ) else {
            return;
        };
        let video_phases = output
            .iter()
            .filter(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("video_marker")
            })
            .filter_map(|record| record.get("phase").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            video_phases,
            vec!["start", "stop"],
            "video start and stop markers should be present in timeline order"
        );
        assert!(
            output.iter().any(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("video_marker")
                    && record.get("clock_domain").and_then(Value::as_str) == Some("host_wall_ms")
                    && record.get("clock_alignment_status").and_then(Value::as_str)
                        == Some("legacy_wall_clock_bracketed")
            }),
            "video markers should expose canonical clock domain and legacy alignment status"
        );
        let Some(index) = assert_ok(read_json(&timeline_dir.join("index.json")), "read index")
        else {
            return;
        };
        let video_source_count = index
            .get("sources")
            .and_then(Value::as_array)
            .and_then(|sources| {
                sources.iter().find(|source| {
                    source.get("kind").and_then(Value::as_str) == Some("video_timing")
                })
            })
            .and_then(|source| source.get("record_count"))
            .and_then(Value::as_u64);
        assert_eq!(
            video_source_count,
            Some(2_u64),
            "timeline index should count video timing start and stop markers"
        );
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
    }

    fn create_complete_fixture(root: &Path) -> TestResult<()> {
        let ime_dir = root.join("ime");
        let derived_dir = root.join("derived");
        let evidence_start = root.join("evidence").join("start");
        let evidence_end = root.join("evidence").join("end");
        fs::create_dir_all(&ime_dir)?;
        fs::create_dir_all(&derived_dir)?;
        fs::create_dir_all(&evidence_start)?;
        fs::create_dir_all(&evidence_end)?;
        write_json(
            &root.join("manifest.json"),
            &json!({
                "external_run_id": "run-test",
                "package_name": "org.inputdynamics.ime.debug",
            }),
        )?;
        write_jsonl(&ime_dir.join("session-test.jsonl"), &ime_fixture())?;
        write_jsonl(
            &derived_dir.join("touch_gestures.jsonl"),
            &[touch_gesture_fixture()],
        )?;
        write_jsonl(
            &derived_dir.join("dismissal_inferences.jsonl"),
            &[dismissal_fixture()],
        )?;
        write_json(
            &evidence_start.join("index.json"),
            &evidence_fixture("start/screenshot.png", 900_i64),
        )?;
        write_json(
            &evidence_end.join("index.json"),
            &evidence_fixture("end/screenshot.png", 1_500_i64),
        )?;
        Ok(())
    }

    fn create_video_fixture(root: &Path) -> TestResult<()> {
        let video_dir = root.join("video");
        fs::create_dir_all(&video_dir)?;
        write_json(
            &video_dir.join("timing.json"),
            &json!({
                "schema": "input_dynamics_video_capture.v1",
                "enabled": true,
                "required": true,
                "remote_path": "/sdcard/Download/input-dynamics-run-test.mp4",
                "local_path": "video/screen.mp4",
                "start": {
                    "phase": "start",
                    "host_wall_ms_before_device_timestamp": 800_i64,
                    "device_epoch_ms": 801_i64,
                    "host_wall_ms_after_device_timestamp": 802_i64,
                },
                "stop": {
                    "phase": "stop",
                    "host_wall_ms_before_device_timestamp": 1_700_i64,
                    "device_epoch_ms": 1_701_i64,
                    "host_wall_ms_after_device_timestamp": 1_702_i64,
                },
                "ok": true,
            }),
        )
    }

    fn ime_fixture() -> Vec<Value> {
        vec![
            json!({
                "event": "session_start",
                "session_id": "session-test",
                "external_run_id": "run-test",
                "package_name": "org.inputdynamics.ime.debug",
                "t_uptime_ms": 100_i64,
                "t_wall_ms": 1_000_i64,
            }),
            json!({
                "event": "pointer_sample",
                "session_id": "session-test",
                "t_uptime_ms": 110_i64,
            }),
            json!({
                "event": "key_down",
                "session_id": "session-test",
                "external_run_id": "run-test",
                "target_package": "example.app",
                "t_uptime_ms": 120_i64,
                "press_id": 7_i64,
                "gesture_id": 8_i64,
                "key_code": 97_i64,
                "key_label": "a",
                "key_class": "letter",
            }),
            json!({
                "event": "ime_hide_window_called",
                "session_id": "session-test",
                "target_package": "example.app",
                "t_uptime_ms": 200_i64,
            }),
        ]
    }

    fn touch_gesture_fixture() -> Value {
        json!({
            "event": "touch_gesture",
            "gesture_id": "gesture-1",
            "external_run_id": "run-test",
            "session_id": "session-test",
            "package_name": "org.inputdynamics.ime.debug",
            "classification": "screen_edge_inward_swipe",
            "classification_confidence": {"decimal_ppm": 900_000_i64},
            "edge_side": "right",
            "start": {
                "line_index": 10_u64,
                "t_getevent_us": 150_000_i64,
                "t_getevent_ms": 150_i64,
                "x_px": 100_i64,
                "y_px": 200_i64
            },
            "end": {
                "line_index": 12_u64,
                "t_getevent_us": 190_000_i64,
                "t_getevent_ms": 190_i64,
                "x_px": 300_i64,
                "y_px": 200_i64
            },
            "delta": {"duration_ms": 40_i64},
        })
    }

    fn dismissal_fixture() -> Value {
        json!({
            "event": "dismissal_inference",
            "inference_id": "dismissal-1",
            "inferred_dismissal": "system_back_edge_gesture",
            "confidence": {"decimal_ppm": 900_000_i64},
            "time_delta_ms": 10_i64,
            "time_delta_status": "legacy_mixed_clock_heuristic",
            "clock_alignment_status": "unsupported_clock_domain",
            "clock_alignment": {
                "status": "unsupported_clock_domain",
                "ime_event_clock_domain": "android_uptime_ms",
                "getevent_gesture_clock_domain": "kernel_getevent_us",
                "reason": "fixture"
            },
            "target_package": "example.app",
            "evidence": [
                {"kind": "getevent_gesture", "gesture_id": "gesture-1"},
                {"kind": "ime_event", "line_index": 4_u64, "t_uptime_ms": 200_i64}
            ],
        })
    }

    fn evidence_fixture(screenshot: &str, captured_wall_ms: i64) -> Value {
        json!({
            "schema": "input_dynamics_observation_bundle.v1",
            "package_name": "org.inputdynamics.ime.debug",
            "captured_wall_ms": captured_wall_ms,
            "artifacts": {"screenshot_png": screenshot},
            "state": {"ok": true},
        })
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0_u128, |duration| duration.as_millis());
        std::env::temp_dir().join(format!("input-dynamics-{prefix}-{millis}"))
    }

    fn write_json(path: &Path, value: &Value) -> TestResult<()> {
        let text = serde_json::to_string(value)?;
        fs::write(path, text)?;
        Ok(())
    }

    fn write_jsonl(path: &Path, values: &[Value]) -> TestResult<()> {
        let mut text = String::new();
        for value in values {
            let line = serde_json::to_string(value)?;
            text.push_str(&line);
            text.push('\n');
        }
        fs::write(path, text)?;
        Ok(())
    }

    fn read_json(path: &Path) -> TestResult<Value> {
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    fn read_jsonl(path: &Path) -> TestResult<Vec<Value>> {
        let text = fs::read_to_string(path)?;
        text.lines()
            .map(|line| Ok(serde_json::from_str(line)?))
            .collect()
    }

    fn assert_ok<T, E>(result: Result<T, E>, label: &str) -> Option<T>
    where
        E: Debug,
    {
        let error = result.as_ref().err();
        assert!(error.is_none(), "{label} failed: {error:?}");
        result.ok()
    }
}
