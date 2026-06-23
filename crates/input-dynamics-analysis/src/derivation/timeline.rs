//! Cross-source timeline derivation for a recorded input-dynamics run.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::clock::{
    AlignmentStatus, ClockDomain, TimestampPrecision, micros_to_nanos, millis_to_nanos,
};
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
    capture: Option<Value>,
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
    events: &'a [Value],
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
            .then_with(|| self.source_rank.cmp(&other.source_rank))
            .then_with(|| optional_time_order(self.time_ms, other.time_ms))
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
    let evidence_records = read_evidence_records(&paths, &manifest)?;
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
        events: &timeline_values,
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
            "source_time": ime_source_time_json(&record.value),
            "normalized_time": normalized_time_json(AlignmentStatus::NotEstimated.as_str()),
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
                time_ms: ime_order_time_ms(&record.value),
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
        "clock_domain": record
            .pointer("/time/source_clock_domain")
            .and_then(Value::as_str)
            .unwrap_or(ClockDomain::KernelGeteventUs.as_str()),
        "source_time": touch_source_time_json(record),
        "clock_alignment_status": AlignmentStatus::UnsupportedClockDomain.as_str(),
        "normalized_time": normalized_time_json(AlignmentStatus::UnsupportedClockDomain.as_str()),
        "ordering": ordering_json(ORDER_METHOD, AlignmentStatus::UnsupportedClockDomain.as_str()),
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
        "source_clock_domains": dismissal_source_clock_domains(record),
        "source_time": dismissal_source_time_json(record),
        "normalized_time": dismissal_normalized_time_json(record),
        "ordering": ordering_json(
            ORDER_METHOD,
            record
                .get("clock_alignment_status")
                .and_then(Value::as_str)
                .unwrap_or(AlignmentStatus::UnsupportedClockDomain.as_str()),
        ),
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
        let marker_clock = video_marker_clock(phase_record);
        let value = if let Some(clock) = marker_clock {
            json!({
                "schema": TIMELINE_EVENT_SCHEMA,
                "record_kind": "video_marker",
                "event": format!("video_{phase}"),
                "source_layer": "video",
                "source": "video_timing",
                "source_ref": {
                    "path": relative_path_text(recording_dir, path),
                    "phase": phase,
                },
                "clock_domain": ClockDomain::DeviceElapsedRealtimeNs.as_str(),
                "clock_alignment_status": AlignmentStatus::NotEstimated.as_str(),
                "source_time": device_elapsed_source_time_json(clock.elapsed_realtime_ns, Some(clock.bracket)),
                "normalized_time": normalized_time_json(AlignmentStatus::NotEstimated.as_str()),
                "ordering": ordering_json(ORDER_METHOD, AlignmentStatus::NotEstimated.as_str()),
                "phase": phase,
                "t_elapsed_realtime_ns": clock.elapsed_realtime_ns,
                "remote_path": timing_record.get("remote_path").cloned().unwrap_or(Value::Null),
                "local_path": timing_record.get("local_path").cloned().unwrap_or(Value::Null),
                "clock_bracket": clock.bracket,
                "marker": phase_record,
                "file": timing_record.get("file").cloned().unwrap_or(Value::Null),
            })
        } else {
            json!({
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
            "source_time": legacy_host_wall_source_time_json(
                "host_wall_ms_before_device_timestamp",
                legacy_video_phase_wall_ms(phase_record),
            ),
            "normalized_time": normalized_time_json(AlignmentStatus::LegacyWallClockBracketed.as_str()),
            "ordering": ordering_json(ORDER_METHOD, AlignmentStatus::LegacyWallClockBracketed.as_str()),
            "phase": phase,
            "remote_path": timing_record.get("remote_path").cloned().unwrap_or(Value::Null),
            "local_path": timing_record.get("local_path").cloned().unwrap_or(Value::Null),
            "marker": phase_record,
            "file": timing_record.get("file").cloned().unwrap_or(Value::Null),
            })
        };
        timeline.push(TimelineRecord {
            order: TimelineOrder {
                group: video_order_group(phase),
                time_ms: marker_clock
                    .and_then(|clock| elapsed_realtime_ms(clock.elapsed_realtime_ns))
                    .or_else(|| legacy_video_phase_wall_ms(phase_record)),
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
        let capture_clock = evidence_capture_clock(evidence_record.capture.as_ref());
        let (
            clock_domain,
            clock_alignment_status,
            source_time,
            normalized_time,
            marker,
            order_time_ms,
        ) = if let Some(clock) = capture_clock {
            (
                ClockDomain::DeviceElapsedRealtimeNs.as_str(),
                AlignmentStatus::Bracketed.as_str(),
                device_elapsed_interval_source_time_json(
                    clock.before_ns,
                    clock.after_ns,
                    Some(clock.capture),
                ),
                normalized_interval_time_json(
                    AlignmentStatus::Bracketed.as_str(),
                    ClockDomain::DeviceElapsedRealtimeNs.as_str(),
                    clock.before_ns,
                    clock.after_ns,
                ),
                evidence_record.capture.clone().unwrap_or(Value::Null),
                elapsed_realtime_ms(clock.before_ns),
            )
        } else {
            (
                ClockDomain::HostWallMs.as_str(),
                AlignmentStatus::LegacyWallClockBracketed.as_str(),
                legacy_host_wall_source_time_json(
                    "captured_wall_ms",
                    record.get("captured_wall_ms").and_then(Value::as_i64),
                ),
                normalized_time_json(AlignmentStatus::LegacyWallClockBracketed.as_str()),
                evidence_record.capture.clone().unwrap_or(Value::Null),
                record.get("captured_wall_ms").and_then(Value::as_i64),
            )
        };
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
            "clock_domain": clock_domain,
            "clock_alignment_status": clock_alignment_status,
            "source_time": source_time,
            "normalized_time": normalized_time,
            "ordering": ordering_json(ORDER_METHOD, clock_alignment_status),
            "captured_wall_ms": record.get("captured_wall_ms").cloned().unwrap_or(Value::Null),
            "phase": phase,
            "capture": marker,
            "package_name": record.get("package_name").cloned().unwrap_or(Value::Null),
            "artifacts": record.get("artifacts").cloned().unwrap_or(Value::Null),
            "state_ok": record.pointer("/state/ok").cloned().unwrap_or(Value::Null),
        });
        timeline.push(TimelineRecord {
            order: TimelineOrder {
                group: evidence_order_group(kind)?,
                time_ms: order_time_ms,
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
        "clock_domain_counts": count_string_field(inputs.events, "clock_domain"),
        "normalized_status_counts": count_string_pointer(inputs.events, "/normalized_time/status"),
        "artifact_diagnostics": artifact_clock_diagnostics(inputs.events),
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

fn count_string_field(records: &[Value], field: &str) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for record in records {
        let Some(value) = record.get(field).and_then(Value::as_str) else {
            continue;
        };
        let current = counts.get(value).copied().unwrap_or(0_u64);
        counts.insert(value.to_owned(), current.saturating_add(1));
    }
    counts
}

fn count_string_pointer(records: &[Value], pointer: &str) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for record in records {
        let Some(value) = record.pointer(pointer).and_then(Value::as_str) else {
            continue;
        };
        let current = counts.get(value).copied().unwrap_or(0_u64);
        counts.insert(value.to_owned(), current.saturating_add(1));
    }
    counts
}

#[derive(Default)]
struct ArtifactClockDiagnostics {
    missing_domains: u64,
    invalid_domains: u64,
    mixed_domain_claims: u64,
    mixed_domain_claims_without_alignment: u64,
    normalized_claims_without_domain: u64,
    unit_mismatches: u64,
}

impl ArtifactClockDiagnostics {
    const fn increment_missing_clock_domain(&mut self) {
        self.missing_domains = self.missing_domains.saturating_add(1);
    }

    const fn increment_invalid_clock_domain(&mut self) {
        self.invalid_domains = self.invalid_domains.saturating_add(1);
    }

    const fn increment_mixed_clock_domain_claim(&mut self) {
        self.mixed_domain_claims = self.mixed_domain_claims.saturating_add(1);
    }

    const fn increment_mixed_clock_domain_without_alignment(&mut self) {
        self.mixed_domain_claims_without_alignment =
            self.mixed_domain_claims_without_alignment.saturating_add(1);
    }

    const fn increment_normalized_claim_without_domain(&mut self) {
        self.normalized_claims_without_domain =
            self.normalized_claims_without_domain.saturating_add(1);
    }

    const fn increment_unit_mismatch(&mut self) {
        self.unit_mismatches = self.unit_mismatches.saturating_add(1);
    }

    fn to_json(&self) -> Value {
        json!({
            "missing_clock_domain_count": self.missing_domains,
            "invalid_clock_domain_count": self.invalid_domains,
            "mixed_clock_domain_claim_count": self.mixed_domain_claims,
            "mixed_clock_domain_without_alignment_count": self.mixed_domain_claims_without_alignment,
            "normalized_claim_without_domain_count": self.normalized_claims_without_domain,
            "unit_mismatch_count": self.unit_mismatches,
        })
    }
}

#[derive(Clone, Copy)]
enum DomainRequirement {
    Required,
}

fn artifact_clock_diagnostics(records: &[Value]) -> Value {
    let mut diagnostics = ArtifactClockDiagnostics::default();
    for record in records {
        inspect_artifact_clock_record(&mut diagnostics, record);
    }
    diagnostics.to_json()
}

fn inspect_artifact_clock_record(diagnostics: &mut ArtifactClockDiagnostics, record: &Value) {
    inspect_clock_domain_field(
        diagnostics,
        record.get("clock_domain"),
        DomainRequirement::Required,
    );
    if source_time_has_value(record.pointer("/source_time")) {
        inspect_clock_domain_field(
            diagnostics,
            record.pointer("/source_time/source_clock_domain"),
            DomainRequirement::Required,
        );
    }
    inspect_normalized_clock_claim(diagnostics, record.pointer("/normalized_time"));
    inspect_source_clock_domains(diagnostics, record);
    inspect_time_unit_consistency(diagnostics, record.pointer("/source_time"));
}

fn inspect_clock_domain_field(
    diagnostics: &mut ArtifactClockDiagnostics,
    value: Option<&Value>,
    requirement: DomainRequirement,
) {
    match value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        Some(text) if text.parse::<ClockDomain>().is_ok() => {}
        Some(_text) => diagnostics.increment_invalid_clock_domain(),
        None => match requirement {
            DomainRequirement::Required => diagnostics.increment_missing_clock_domain(),
        },
    }
}

fn inspect_normalized_clock_claim(
    diagnostics: &mut ArtifactClockDiagnostics,
    normalized_time: Option<&Value>,
) {
    let Some(time) = normalized_time else {
        return;
    };
    let status = time.get("status").and_then(Value::as_str);
    let claim_like = matches!(status, Some("bracketed" | "estimated"))
        || has_non_null_pointer(time, "/time_ns")
        || has_non_null_pointer(time, "/time_interval_ns");
    if !claim_like {
        return;
    }
    let before_missing = diagnostics.missing_domains;
    let before_invalid = diagnostics.invalid_domains;
    inspect_clock_domain_field(
        diagnostics,
        time.get("clock_domain"),
        DomainRequirement::Required,
    );
    if diagnostics.missing_domains > before_missing || diagnostics.invalid_domains > before_invalid
    {
        diagnostics.increment_normalized_claim_without_domain();
    }
}

fn inspect_source_clock_domains(diagnostics: &mut ArtifactClockDiagnostics, record: &Value) {
    let Some(domains) = record.get("source_clock_domains").and_then(Value::as_array) else {
        return;
    };
    let mut unique_domains = Vec::new();
    for domain in domains {
        let Some(text) = domain.as_str().filter(|value| !value.is_empty()) else {
            diagnostics.increment_invalid_clock_domain();
            continue;
        };
        if text.parse::<ClockDomain>().is_err() {
            diagnostics.increment_invalid_clock_domain();
            continue;
        }
        if !unique_domains.iter().any(|existing| existing == text) {
            unique_domains.push(text.to_owned());
        }
    }
    if unique_domains.len() <= 1 || !record_has_cross_domain_claim(record) {
        return;
    }
    diagnostics.increment_mixed_clock_domain_claim();
    if !record_has_supported_alignment(record) {
        diagnostics.increment_mixed_clock_domain_without_alignment();
    }
}

fn record_has_cross_domain_claim(record: &Value) -> bool {
    record
        .get("time_delta_ms")
        .is_some_and(|value| !value.is_null())
        || record
            .pointer("/normalized_time/status")
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "bracketed" | "estimated"))
        || has_non_null_pointer(record, "/normalized_time/time_ns")
        || has_non_null_pointer(record, "/normalized_time/time_interval_ns")
}

fn record_has_supported_alignment(record: &Value) -> bool {
    record
        .get("clock_alignment_status")
        .or_else(|| record.pointer("/clock_alignment/status"))
        .or_else(|| record.pointer("/ordering/clock_alignment_status"))
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "bracketed" | "estimated"))
}

fn inspect_time_unit_consistency(
    diagnostics: &mut ArtifactClockDiagnostics,
    source_time: Option<&Value>,
) {
    let Some(time) = source_time else {
        return;
    };
    if let (Some(ms), Some(ns)) = (
        time.get("source_time_ms").and_then(Value::as_i64),
        time.get("source_time_ns").and_then(Value::as_i64),
    ) && millis_to_nanos(ms) != Some(ns)
    {
        diagnostics.increment_unit_mismatch();
    }
    if let (Some(us), Some(ns)) = (
        time.get("source_time_us").and_then(Value::as_i64),
        time.get("source_time_ns").and_then(Value::as_i64),
    ) && micros_to_nanos(us) != Some(ns)
    {
        diagnostics.increment_unit_mismatch();
    }
    if let (Some((start_us, end_us)), Some((start_ns, end_ns))) = (
        i64_pair(time.get("source_time_interval_us")),
        i64_pair(time.get("source_time_interval_ns")),
    ) && (micros_to_nanos(start_us) != Some(start_ns) || micros_to_nanos(end_us) != Some(end_ns))
    {
        diagnostics.increment_unit_mismatch();
    }
}

fn source_time_has_value(source_time: Option<&Value>) -> bool {
    source_time.is_some_and(|time| {
        has_non_null_pointer(time, "/source_time_ms")
            || has_non_null_pointer(time, "/source_time_ns")
            || has_non_null_pointer(time, "/source_time_us")
            || has_non_null_pointer(time, "/source_time_interval_us")
            || has_non_null_pointer(time, "/source_time_interval_ns")
    })
}

fn has_non_null_pointer(value: &Value, pointer: &str) -> bool {
    value.pointer(pointer).is_some_and(|field| !field.is_null())
}

fn i64_pair(value: Option<&Value>) -> Option<(i64, i64)> {
    let mut values = value?.as_array()?.iter();
    let first = values.next()?.as_i64()?;
    let second = values.next()?.as_i64()?;
    if values.next().is_some() {
        return None;
    }
    Some((first, second))
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

fn read_evidence_records(
    paths: &TimelinePaths,
    manifest: &Value,
) -> DeriveResult<Vec<EvidenceRecord>> {
    Ok(vec![
        EvidenceRecord {
            kind: SourceKind::EvidenceStart,
            value: read_optional_json(&paths.evidence_start_index)?,
            capture: manifest.pointer("/evidence/start").cloned(),
        },
        EvidenceRecord {
            kind: SourceKind::EvidenceEnd,
            value: read_optional_json(&paths.evidence_end_index)?,
            capture: manifest.pointer("/evidence/end").cloned(),
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

fn legacy_video_phase_wall_ms(record: &Value) -> Option<i64> {
    record
        .get("host_wall_ms_before_device_timestamp")
        .and_then(Value::as_i64)
        .or_else(|| {
            record
                .pointer("/before/host_wall_ms_before_device_timestamp")
                .and_then(Value::as_i64)
        })
        .or_else(|| {
            record
                .pointer("/before/host_wall_ms")
                .and_then(Value::as_i64)
        })
}

#[derive(Clone, Copy)]
struct VideoMarkerClock<'a> {
    elapsed_realtime_ns: i64,
    bracket: &'a Value,
}

#[derive(Clone, Copy)]
struct EvidenceCaptureClock<'a> {
    before_ns: i64,
    after_ns: i64,
    capture: &'a Value,
}

struct MillisecondSourceTimeInput<'a> {
    clock_domain: &'a str,
    source_field: &'a str,
    value_ms: Option<i64>,
    source_time_status: &'a str,
    metadata: Option<&'a Value>,
    alignment_status: &'a str,
}

fn ime_source_time_json(record: &Value) -> Value {
    let event_time = record.get("event_time");
    let t_event_uptime_ms = record.get("t_event_uptime_ms").and_then(Value::as_i64);
    if event_time
        .and_then(|metadata| metadata.get("clock_domain"))
        .and_then(Value::as_str)
        == Some(ClockDomain::AndroidUptimeMs.as_str())
        && event_time
            .and_then(|metadata| metadata.get("timestamp_precision"))
            .and_then(Value::as_str)
            == Some(TimestampPrecision::Milliseconds.as_str())
        && event_time
            .and_then(|metadata| metadata.get("field"))
            .and_then(Value::as_str)
            == Some("t_event_uptime_ms")
        && let Some(value_ms) = t_event_uptime_ms
    {
        return source_millisecond_time_json(&MillisecondSourceTimeInput {
            clock_domain: ClockDomain::AndroidUptimeMs.as_str(),
            source_field: "t_event_uptime_ms",
            value_ms: Some(value_ms),
            source_time_status: "canonical_event_time_metadata",
            metadata: event_time,
            alignment_status: AlignmentStatus::NotEstimated.as_str(),
        });
    }
    if let Some(value_ms) = t_event_uptime_ms {
        return source_millisecond_time_json(&MillisecondSourceTimeInput {
            clock_domain: ClockDomain::AndroidUptimeMs.as_str(),
            source_field: "t_event_uptime_ms",
            value_ms: Some(value_ms),
            source_time_status: "legacy_t_event_uptime_ms",
            metadata: event_time,
            alignment_status: AlignmentStatus::NotEstimated.as_str(),
        });
    }
    source_millisecond_time_json(&MillisecondSourceTimeInput {
        clock_domain: ClockDomain::AndroidUptimeMs.as_str(),
        source_field: "t_uptime_ms",
        value_ms: record.get("t_uptime_ms").and_then(Value::as_i64),
        source_time_status: "legacy_t_uptime_ms_fallback",
        metadata: event_time,
        alignment_status: AlignmentStatus::NotEstimated.as_str(),
    })
}

fn ime_order_time_ms(record: &Value) -> Option<i64> {
    let event_time = record.get("event_time");
    let t_event_uptime_ms = record.get("t_event_uptime_ms").and_then(Value::as_i64);
    if event_time
        .and_then(|metadata| metadata.get("clock_domain"))
        .and_then(Value::as_str)
        == Some(ClockDomain::AndroidUptimeMs.as_str())
        && event_time
            .and_then(|metadata| metadata.get("timestamp_precision"))
            .and_then(Value::as_str)
            == Some(TimestampPrecision::Milliseconds.as_str())
        && event_time
            .and_then(|metadata| metadata.get("field"))
            .and_then(Value::as_str)
            == Some("t_event_uptime_ms")
    {
        return t_event_uptime_ms;
    }
    t_event_uptime_ms.or_else(|| record.get("t_uptime_ms").and_then(Value::as_i64))
}

fn touch_source_time_json(record: &Value) -> Value {
    if let Some(time) = record.get("time") {
        return time.clone();
    }
    let start_us = record
        .pointer("/start/t_getevent_us")
        .and_then(Value::as_i64);
    let end_us = record.pointer("/end/t_getevent_us").and_then(Value::as_i64);
    json!({
        "source_clock_domain": ClockDomain::KernelGeteventUs.as_str(),
        "source_timestamp_precision": TimestampPrecision::Microseconds.as_str(),
        "source_time_interval_us": [start_us, end_us],
        "source_time_interval_ns": [
            start_us.and_then(micros_to_nanos),
            end_us.and_then(micros_to_nanos),
        ],
        "source_time_status": "derived_getevent_time",
        "normalized_clock_domain": Value::Null,
        "normalized_time_interval_ns": Value::Null,
        "alignment_status": AlignmentStatus::UnsupportedClockDomain.as_str(),
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
    })
}

fn dismissal_source_time_json(record: &Value) -> Value {
    if let Some(time) = record.get("source_time") {
        return time.clone();
    }
    if let Some(time) = record
        .get("evidence")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("kind").and_then(Value::as_str) == Some("ime_event"))
        })
        .and_then(|item| item.get("time"))
    {
        return time.clone();
    }
    source_millisecond_time_json(&MillisecondSourceTimeInput {
        clock_domain: ClockDomain::AndroidUptimeMs.as_str(),
        source_field: "evidence.ime_event.t_uptime_ms",
        value_ms: dismissal_order_ms(record),
        source_time_status: "legacy_t_uptime_ms_fallback",
        metadata: None,
        alignment_status: AlignmentStatus::NotEstimated.as_str(),
    })
}

fn dismissal_normalized_time_json(record: &Value) -> Value {
    if let Some(time) = record.get("normalized_time") {
        return time.clone();
    }
    normalized_time_json(
        record
            .get("clock_alignment_status")
            .and_then(Value::as_str)
            .unwrap_or(AlignmentStatus::UnsupportedClockDomain.as_str()),
    )
}

fn dismissal_source_clock_domains(record: &Value) -> Value {
    record
        .pointer("/clock_alignment/source_clock_domains")
        .cloned()
        .unwrap_or_else(|| {
            json!([
                record
                    .pointer("/clock_alignment/ime_event_clock_domain")
                    .cloned()
                    .unwrap_or_else(|| json!(ClockDomain::AndroidUptimeMs.as_str())),
                record
                    .pointer("/clock_alignment/getevent_gesture_clock_domain")
                    .cloned()
                    .unwrap_or_else(|| json!(ClockDomain::KernelGeteventUs.as_str())),
            ])
        })
}

fn source_millisecond_time_json(input: &MillisecondSourceTimeInput<'_>) -> Value {
    let status = if input.value_ms.is_some() {
        input.source_time_status
    } else {
        "missing"
    };
    json!({
        "source_clock_domain": input.value_ms.map(|_value| input.clock_domain),
        "source_timestamp_precision": input.value_ms.map(|_value| TimestampPrecision::Milliseconds.as_str()),
        "source_time_ms": input.value_ms,
        "source_time_ns": input.value_ms.and_then(millis_to_nanos),
        "source_field": input.source_field,
        "source_time_status": status,
        "timestamp_role_metadata": input.metadata.cloned(),
        "normalized_clock_domain": Value::Null,
        "normalized_time_ns": Value::Null,
        "alignment_status": input.alignment_status,
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
    })
}

fn device_elapsed_source_time_json(value_ns: i64, bracket: Option<&Value>) -> Value {
    json!({
        "source_clock_domain": ClockDomain::DeviceElapsedRealtimeNs.as_str(),
        "source_timestamp_precision": TimestampPrecision::Nanoseconds.as_str(),
        "source_time_ns": value_ns,
        "source_field": "t_elapsed_realtime_ns",
        "source_time_status": "device_clock_probe",
        "clock_bracket": bracket.cloned().unwrap_or(Value::Null),
        "normalized_clock_domain": Value::Null,
        "normalized_time_ns": Value::Null,
        "alignment_status": AlignmentStatus::NotEstimated.as_str(),
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
    })
}

fn device_elapsed_interval_source_time_json(
    before_ns: i64,
    after_ns: i64,
    capture: Option<&Value>,
) -> Value {
    json!({
        "source_clock_domain": ClockDomain::DeviceElapsedRealtimeNs.as_str(),
        "source_timestamp_precision": TimestampPrecision::Nanoseconds.as_str(),
        "source_time_interval_ns": [before_ns, after_ns],
        "source_time_status": "device_clock_probe_bracket",
        "clock_bracket": capture.cloned().unwrap_or(Value::Null),
        "normalized_clock_domain": Value::Null,
        "normalized_time_interval_ns": Value::Null,
        "alignment_status": AlignmentStatus::Bracketed.as_str(),
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
    })
}

fn legacy_host_wall_source_time_json(source_field: &str, value_ms: Option<i64>) -> Value {
    source_millisecond_time_json(&MillisecondSourceTimeInput {
        clock_domain: ClockDomain::HostWallMs.as_str(),
        source_field,
        value_ms,
        source_time_status: "legacy_wall_clock",
        metadata: None,
        alignment_status: AlignmentStatus::LegacyWallClockBracketed.as_str(),
    })
}

fn normalized_time_json(status: &str) -> Value {
    json!({
        "status": status,
        "clock_domain": Value::Null,
        "time_ns": Value::Null,
        "time_interval_ns": Value::Null,
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
    })
}

fn normalized_interval_time_json(
    status: &str,
    clock_domain: &str,
    before_ns: i64,
    after_ns: i64,
) -> Value {
    json!({
        "status": status,
        "clock_domain": clock_domain,
        "time_ns": Value::Null,
        "time_interval_ns": [before_ns, after_ns],
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
    })
}

fn video_marker_clock(record: &Value) -> Option<VideoMarkerClock<'_>> {
    let before = record.get("before")?;
    let after = record.get("after")?;
    let elapsed_realtime_ns = probe_marker_elapsed_realtime_ns(before)?;
    let after_elapsed_realtime_ns = probe_marker_elapsed_realtime_ns(after)?;
    if after_elapsed_realtime_ns < elapsed_realtime_ns {
        return None;
    }
    Some(VideoMarkerClock {
        elapsed_realtime_ns,
        bracket: record,
    })
}

fn evidence_capture_clock(record: Option<&Value>) -> Option<EvidenceCaptureClock<'_>> {
    let capture = record?;
    if capture.get("clock_domain").and_then(Value::as_str)
        != Some(ClockDomain::DeviceElapsedRealtimeNs.as_str())
    {
        return None;
    }
    if capture
        .get("clock_alignment_status")
        .and_then(Value::as_str)
        != Some(AlignmentStatus::Bracketed.as_str())
    {
        return None;
    }
    let before = capture.get("before")?;
    let after = capture.get("after")?;
    let before_ns = probe_marker_elapsed_realtime_ns(before)?;
    let after_ns = probe_marker_elapsed_realtime_ns(after)?;
    if after_ns < before_ns {
        return None;
    }
    Some(EvidenceCaptureClock {
        before_ns,
        after_ns,
        capture,
    })
}

fn probe_marker_elapsed_realtime_ns(marker: &Value) -> Option<i64> {
    let probe = marker.get("device_clock_probe")?;
    if probe.get("schema").and_then(Value::as_str) != Some("input_dynamics_device_clock_probe.v1") {
        return None;
    }
    if probe.get("canonical_clock_domain").and_then(Value::as_str)
        != Some(ClockDomain::DeviceElapsedRealtimeNs.as_str())
    {
        return None;
    }
    let probe_elapsed_realtime_ns = probe.get("t_elapsed_realtime_ns").and_then(Value::as_i64)?;
    if let Some(marker_elapsed_realtime_ns) =
        marker.get("t_elapsed_realtime_ns").and_then(Value::as_i64)
        && marker_elapsed_realtime_ns != probe_elapsed_realtime_ns
    {
        return None;
    }
    Some(probe_elapsed_realtime_ns)
}

const fn elapsed_realtime_ms(elapsed_realtime_ns: i64) -> Option<i64> {
    elapsed_realtime_ns.checked_div(1_000_000)
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
        "used": "source_time_for_inspection",
        "canonical_cross_source_order": false,
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
        assert_current_evidence_rows(&output);
        assert_ime_source_time_rows(&output);
        assert_touch_and_dismissal_rows(&output);
        let Some(index) = assert_ok(read_json(&timeline_dir.join("index.json")), "read index")
        else {
            return;
        };
        assert_timeline_index_counts(&index);
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
    }

    fn assert_current_evidence_rows(output: &[Value]) {
        assert_eq!(
            output
                .first()
                .and_then(|record| record.get("record_kind"))
                .and_then(Value::as_str),
            Some("evidence_bundle"),
            "start evidence is anchored first"
        );
        let evidence_rows = output
            .iter()
            .filter(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("evidence_bundle")
            })
            .collect::<Vec<_>>();
        assert_eq!(evidence_rows.len(), 2_usize, "start/end evidence rows");
        assert!(
            evidence_rows.iter().all(|record| {
                record.get("clock_domain").and_then(Value::as_str)
                    == Some("device_elapsed_realtime_ns")
                    && record.get("clock_alignment_status").and_then(Value::as_str)
                        == Some("bracketed")
                    && record
                        .pointer("/source_time/source_time_status")
                        .and_then(Value::as_str)
                        == Some("device_clock_probe_bracket")
                    && record
                        .pointer("/normalized_time/time_interval_ns/0")
                        .is_some()
                    && record
                        .pointer("/ordering/canonical_cross_source_order")
                        .and_then(Value::as_bool)
                        == Some(false)
            }),
            "manifest-backed evidence brackets should expose elapsed-realtime source and normalized intervals"
        );
    }

    fn assert_ime_source_time_rows(output: &[Value]) {
        assert!(
            output.iter().any(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("ime_event")
                    && record.get("event").and_then(Value::as_str) == Some("key_down")
                    && record
                        .pointer("/source_time/source_time_ms")
                        .and_then(Value::as_i64)
                        == Some(125_i64)
                    && record
                        .pointer("/source_time/source_time_status")
                        .and_then(Value::as_str)
                        == Some("canonical_event_time_metadata")
            }),
            "IME timeline rows should prefer event_time metadata over writer uptime"
        );
    }

    fn assert_touch_and_dismissal_rows(output: &[Value]) {
        assert!(
            output.iter().any(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("touch_gesture")
                    && record
                        .pointer("/source_ref/gesture_id")
                        .and_then(Value::as_str)
                        == Some("gesture-1")
                    && record.get("clock_domain").and_then(Value::as_str)
                        == Some("kernel_getevent_us")
                    && record
                        .pointer("/normalized_time/status")
                        .and_then(Value::as_str)
                        == Some("unsupported_clock_domain")
                    && record
                        .pointer("/ordering/clock_alignment_status")
                        .and_then(Value::as_str)
                        == Some("unsupported_clock_domain")
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
                    && record
                        .pointer("/ordering/clock_alignment_status")
                        .and_then(Value::as_str)
                        == Some("unsupported_clock_domain")
            }),
            "dismissal timeline rows should preserve clock-safety metadata"
        );
    }

    fn assert_timeline_index_counts(index: &Value) {
        assert_eq!(
            index.get("event_count").and_then(Value::as_u64),
            Some(7_u64),
            "index should count timeline rows"
        );
        assert_eq!(
            index
                .pointer("/clock_domain_counts/device_elapsed_realtime_ns")
                .and_then(Value::as_u64),
            Some(2_u64),
            "index should count current evidence elapsed-realtime rows"
        );
        assert_eq!(
            index
                .pointer("/ordering/clock_alignment_status")
                .and_then(Value::as_str),
            Some("not_estimated"),
            "clock alignment should be explicit"
        );
        assert_eq!(
            index
                .pointer("/artifact_diagnostics/invalid_clock_domain_count")
                .and_then(Value::as_u64),
            Some(0_u64),
            "valid generated timeline rows should not have invalid clock domains"
        );
        assert_eq!(
            index
                .pointer("/artifact_diagnostics/unit_mismatch_count")
                .and_then(Value::as_u64),
            Some(0_u64),
            "valid generated timeline rows should not have unit mismatches"
        );
        assert_mixed_domain_diagnostics(index, 1_u64, 1_u64);
    }

    #[test]
    fn derives_timeline_with_legacy_evidence_index_markers() {
        let root = unique_temp_dir("timeline-legacy-evidence");
        let Some(()) = assert_ok(create_legacy_evidence_fixture(&root), "create fixture") else {
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

        let Some(output) = assert_ok(
            read_jsonl(&root.join("derived").join("timeline").join("events.jsonl")),
            "read timeline events",
        ) else {
            return;
        };
        let evidence_rows = output
            .iter()
            .filter(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("evidence_bundle")
            })
            .collect::<Vec<_>>();
        assert_eq!(evidence_rows.len(), 2_usize, "start/end evidence rows");
        assert!(
            evidence_rows.iter().all(|record| {
                record.get("clock_domain").and_then(Value::as_str) == Some("host_wall_ms")
                    && record.get("clock_alignment_status").and_then(Value::as_str)
                        == Some("legacy_wall_clock_bracketed")
                    && record
                        .pointer("/source_time/source_time_status")
                        .and_then(Value::as_str)
                        == Some("legacy_wall_clock")
            }),
            "index-only evidence timing should be explicit legacy host-wall provenance"
        );
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
    }

    #[test]
    fn derives_timeline_with_mapped_unsupported_and_legacy_time_claims() {
        let root = unique_temp_dir("timeline-derived-time-claims");
        let Some(()) = assert_ok(create_dismissal_time_claim_fixture(&root), "create fixture")
        else {
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
            "read events",
        ) else {
            return;
        };
        assert!(
            output.iter().any(|record| {
                record.get("inference_id").and_then(Value::as_str) == Some("dismissal-mapped")
                    && record
                        .pointer("/normalized_time/status")
                        .and_then(Value::as_str)
                        == Some("bracketed")
                    && record
                        .pointer("/normalized_time/time_interval_ns/0")
                        .and_then(Value::as_i64)
                        .is_some()
            }),
            "mapped fixture should preserve its normalized time claim"
        );
        assert!(
            output.iter().any(|record| {
                record.get("inference_id").and_then(Value::as_str) == Some("dismissal-unsupported")
                    && record.get("time_delta_ms") == Some(&Value::Null)
                    && record
                        .pointer("/normalized_time/status")
                        .and_then(Value::as_str)
                        == Some("unsupported_clock_domain")
            }),
            "unsupported fixture should remain null-delta and non-normalized"
        );
        assert!(
            output.iter().any(|record| {
                record.get("inference_id").and_then(Value::as_str) == Some("dismissal-legacy")
                    && record.get("time_delta_status").and_then(Value::as_str)
                        == Some("legacy_mixed_clock_heuristic")
                    && record
                        .pointer("/normalized_time/status")
                        .and_then(Value::as_str)
                        == Some("unsupported_clock_domain")
            }),
            "legacy fixture should remain readable without becoming mapped"
        );
        let Some(index) = assert_ok(read_json(&timeline_dir.join("index.json")), "read index")
        else {
            return;
        };
        assert_eq!(
            index
                .pointer("/normalized_status_counts/bracketed")
                .and_then(Value::as_u64),
            Some(1_u64),
            "index should count mapped normalized rows separately"
        );
        assert_eq!(
            index
                .pointer("/normalized_status_counts/unsupported_clock_domain")
                .and_then(Value::as_u64),
            Some(2_u64),
            "index should count unsupported and legacy rows separately from mapped rows"
        );
        assert_mixed_domain_diagnostics(&index, 2_u64, 1_u64);
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
    }

    #[test]
    fn artifact_diagnostics_reject_scalar_microsecond_nanosecond_mismatch() {
        let mut diagnostics = super::ArtifactClockDiagnostics::default();
        super::inspect_artifact_clock_record(
            &mut diagnostics,
            &json!({
                "clock_domain": "kernel_getevent_us",
                "source_time": {
                    "source_clock_domain": "kernel_getevent_us",
                    "source_time_us": 2_i64,
                    "source_time_ns": 2_001_i64
                }
            }),
        );

        assert_eq!(
            diagnostics
                .to_json()
                .pointer("/unit_mismatch_count")
                .and_then(Value::as_u64),
            Some(1_u64),
            "scalar microsecond/nanosecond mismatch should be counted"
        );
    }

    fn assert_mixed_domain_diagnostics(
        index: &Value,
        expected_claim_count: u64,
        expected_without_alignment_count: u64,
    ) {
        assert_eq!(
            index
                .pointer("/artifact_diagnostics/mixed_clock_domain_claim_count")
                .and_then(Value::as_u64),
            Some(expected_claim_count),
            "mixed-domain claims should be counted"
        );
        assert_eq!(
            index
                .pointer("/artifact_diagnostics/mixed_clock_domain_without_alignment_count")
                .and_then(Value::as_u64),
            Some(expected_without_alignment_count),
            "unsupported mixed-domain claims should be counted separately"
        );
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

    #[test]
    fn derives_timeline_with_nested_legacy_video_markers() {
        let root = unique_temp_dir("timeline-nested-legacy-video");
        let Some(()) = assert_ok(create_complete_fixture(&root), "create fixture") else {
            return;
        };
        let Some(()) = assert_ok(
            create_nested_legacy_video_fixture(&root),
            "create nested legacy video fixture",
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
        let video_records = output
            .iter()
            .filter(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("video_marker")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            video_records.len(),
            2_usize,
            "nested legacy video fixture should emit start and stop rows"
        );
        assert!(
            video_records.iter().all(|record| {
                record.get("clock_domain").and_then(Value::as_str) == Some("host_wall_ms")
                    && record.get("clock_alignment_status").and_then(Value::as_str)
                        == Some("legacy_wall_clock_bracketed")
                    && record
                        .pointer("/source_time/source_time_status")
                        .and_then(Value::as_str)
                        == Some("legacy_wall_clock")
                    && record
                        .pointer("/source_time/source_time_ms")
                        .and_then(Value::as_i64)
                        .is_some()
            }),
            "nested legacy video timing should preserve non-null host-wall source time"
        );
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
    }

    #[test]
    fn derives_timeline_with_current_video_probe_markers() {
        let root = unique_temp_dir("timeline-current-video");
        let Some(()) = assert_ok(create_complete_fixture(&root), "create fixture") else {
            return;
        };
        let Some(()) = assert_ok(
            create_current_video_fixture(&root),
            "create current video fixture",
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
        let video_records = output
            .iter()
            .filter(|record| {
                record.get("record_kind").and_then(Value::as_str) == Some("video_marker")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            video_records.len(),
            2_usize,
            "current video fixture should emit start and stop rows"
        );
        assert!(
            video_records.iter().all(|record| {
                record.get("clock_domain").and_then(Value::as_str)
                    == Some("device_elapsed_realtime_ns")
                    && record.get("clock_alignment_status").and_then(Value::as_str)
                        == Some("not_estimated")
                    && record
                        .get("t_elapsed_realtime_ns")
                        .and_then(Value::as_i64)
                        .is_some()
                    && record
                        .pointer("/clock_bracket/before/device_clock_probe/schema")
                        .and_then(Value::as_str)
                        == Some("input_dynamics_device_clock_probe.v1")
            }),
            "current video rows should expose STATUS-backed elapsed realtime markers"
        );
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
    }

    #[test]
    fn derives_timeline_rejects_mismatched_probe_marker_elapsed_time() {
        let root = unique_temp_dir("timeline-mismatched-video-probe");
        let Some(()) = assert_ok(create_complete_fixture(&root), "create fixture") else {
            return;
        };
        let Some(()) = assert_ok(
            create_mismatched_current_video_fixture(&root),
            "create mismatched video fixture",
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
        let Some(_summary) = assert_ok(derive_result, "derive timeline") else {
            return;
        };

        let Some(output) = assert_ok(
            read_jsonl(&root.join("derived").join("timeline").join("events.jsonl")),
            "read timeline events",
        ) else {
            return;
        };
        assert!(
            output
                .iter()
                .filter(|record| {
                    record.get("record_kind").and_then(Value::as_str) == Some("video_marker")
                        && record.get("clock_domain").and_then(Value::as_str)
                            == Some("device_elapsed_realtime_ns")
                })
                .count()
                == 0_usize,
            "mismatched probe wrappers should not emit canonical video markers"
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
                "evidence": {
                    "enabled": true,
                    "policy": "start_end",
                    "start": current_evidence_capture("start", 900_000_i64, 910_000_i64),
                    "end": current_evidence_capture("end", 1_500_000_i64, 1_510_000_i64),
                },
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

    fn create_legacy_evidence_fixture(root: &Path) -> TestResult<()> {
        let ime_dir = root.join("ime");
        let evidence_start = root.join("evidence").join("start");
        let evidence_end = root.join("evidence").join("end");
        fs::create_dir_all(&ime_dir)?;
        fs::create_dir_all(&evidence_start)?;
        fs::create_dir_all(&evidence_end)?;
        write_json(
            &root.join("manifest.json"),
            &json!({
                "external_run_id": "run-test",
                "package_name": "org.inputdynamics.ime.debug",
            }),
        )?;
        write_jsonl(
            &ime_dir.join("session-test.jsonl"),
            &[session_start_fixture()],
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

    fn create_dismissal_time_claim_fixture(root: &Path) -> TestResult<()> {
        let ime_dir = root.join("ime");
        let derived_dir = root.join("derived");
        fs::create_dir_all(&ime_dir)?;
        fs::create_dir_all(&derived_dir)?;
        write_json(
            &root.join("manifest.json"),
            &json!({
                "external_run_id": "run-test",
                "package_name": "org.inputdynamics.ime.debug",
            }),
        )?;
        write_jsonl(
            &ime_dir.join("session-test.jsonl"),
            &[session_start_fixture()],
        )?;
        write_jsonl(
            &derived_dir.join("dismissal_inferences.jsonl"),
            &[
                mapped_dismissal_fixture(),
                unsupported_dismissal_fixture(),
                legacy_dismissal_fixture(),
            ],
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

    fn create_nested_legacy_video_fixture(root: &Path) -> TestResult<()> {
        let video_dir = root.join("video");
        fs::create_dir_all(&video_dir)?;
        write_json(
            &video_dir.join("timing.json"),
            &json!({
                "enabled": true,
                "required": true,
                "remote_path": "/sdcard/Download/input-dynamics-run-test.mp4",
                "local_path": "video/screen.mp4",
                "start": {
                    "phase": "start",
                    "before": {
                        "host_wall_ms_before_device_timestamp": 800_i64,
                        "device_wall_ms": 801_i64,
                        "host_wall_ms_after_device_timestamp": 802_i64
                    },
                    "after": {
                        "host_wall_ms_before_device_timestamp": 803_i64,
                        "device_wall_ms": 804_i64,
                        "host_wall_ms_after_device_timestamp": 805_i64
                    }
                },
                "stop": {
                    "phase": "stop",
                    "before": {
                        "host_wall_ms_before_device_timestamp": 1_700_i64,
                        "device_wall_ms": 1_701_i64,
                        "host_wall_ms_after_device_timestamp": 1_702_i64
                    },
                    "after": {
                        "host_wall_ms_before_device_timestamp": 1_703_i64,
                        "device_wall_ms": 1_704_i64,
                        "host_wall_ms_after_device_timestamp": 1_705_i64
                    }
                },
                "ok": true,
            }),
        )
    }

    fn create_current_video_fixture(root: &Path) -> TestResult<()> {
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
                    "before": video_probe_marker("before_screenrecord_start", 10_000_i64),
                    "after": video_probe_marker("after_screenrecord_start", 11_000_i64),
                },
                "stop": {
                    "before": video_probe_marker("before_screenrecord_stop", 20_000_i64),
                    "after": video_probe_marker("after_screenrecord_stop", 21_000_i64),
                },
                "ok": true,
            }),
        )
    }

    fn create_mismatched_current_video_fixture(root: &Path) -> TestResult<()> {
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
                    "before": mismatched_video_probe_marker("before_screenrecord_start", 10_000_i64),
                    "after": video_probe_marker("after_screenrecord_start", 11_000_i64),
                },
                "stop": {
                    "before": mismatched_video_probe_marker("before_screenrecord_stop", 20_000_i64),
                    "after": video_probe_marker("after_screenrecord_stop", 21_000_i64),
                },
                "ok": true,
            }),
        )
    }

    fn video_probe_marker(phase: &str, elapsed_realtime_ns: i64) -> Value {
        json!({
            "schema": "input_dynamics_device_clock_probe.v1",
            "phase": phase,
            "clock_domain": "device_elapsed_realtime_ns",
            "clock_alignment_status": "not_estimated",
            "t_elapsed_realtime_ns": elapsed_realtime_ns,
            "device_clock_probe": {
                "schema": "input_dynamics_device_clock_probe.v1",
                "canonical_clock_domain": "device_elapsed_realtime_ns",
                "t_elapsed_realtime_ns": elapsed_realtime_ns,
            },
        })
    }

    fn mismatched_video_probe_marker(phase: &str, elapsed_realtime_ns: i64) -> Value {
        let mut marker = video_probe_marker(phase, elapsed_realtime_ns);
        if let Some(object) = marker.as_object_mut() {
            object.insert(
                String::from("t_elapsed_realtime_ns"),
                json!(elapsed_realtime_ns.saturating_add(1_i64)),
            );
        }
        marker
    }

    fn current_evidence_capture(phase: &str, before_ns: i64, after_ns: i64) -> Value {
        json!({
            "schema": "input_dynamics_record_evidence_capture.v1",
            "enabled": true,
            "requested": true,
            "phase": phase,
            "policy": "start_end",
            "clock_domain": "device_elapsed_realtime_ns",
            "clock_alignment_status": "bracketed",
            "before": video_probe_marker(&format!("before_evidence_{phase}"), before_ns),
            "after": video_probe_marker(&format!("after_evidence_{phase}"), after_ns),
            "bundle": {
                "schema": "input_dynamics_observation_bundle.v1",
                "index": format!("evidence/{phase}/index.json")
            },
        })
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
                "t_uptime_ms": 9_000_i64,
                "t_event_uptime_ms": 125_i64,
                "event_time": ime_event_time_metadata(),
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

    fn ime_event_time_metadata() -> Value {
        json!({
            "clock_domain": "android_uptime_ms",
            "timestamp_source": "motion_event",
            "timestamp_precision": "milliseconds",
            "field": "t_event_uptime_ms",
            "field_ns": "t_event_uptime_ns",
            "field_ns_precision": "milliseconds_converted_to_nanoseconds",
        })
    }

    fn session_start_fixture() -> Value {
        ime_fixture()
            .into_iter()
            .next()
            .unwrap_or_else(|| json!({"event": "session_start", "t_uptime_ms": 100_i64}))
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

    fn mapped_dismissal_fixture() -> Value {
        json!({
            "event": "dismissal_inference",
            "inference_id": "dismissal-mapped",
            "inferred_dismissal": "focus_or_app_hide_unknown",
            "confidence": {"decimal_ppm": 500_000_i64},
            "time_delta_ms": 12_i64,
            "clock_alignment_status": "bracketed",
            "source_time": {
                "source_clock_domain": "android_uptime_ms",
                "source_timestamp_precision": "milliseconds",
                "source_time_ms": 200_i64,
                "source_time_ns": 200_000_000_i64,
                "source_field": "evidence.ime_event.t_uptime_ms",
                "source_time_status": "synthetic_mapped_fixture",
                "alignment_status": "bracketed"
            },
            "normalized_time": {
                "status": "bracketed",
                "clock_domain": "device_elapsed_realtime_ns",
                "time_interval_ns": [1_000_000_000_i64, 1_001_000_000_i64],
                "transform_id": "fixture-transform",
                "uncertainty_ns": 1_000_000_i64
            },
            "clock_alignment": {
                "status": "bracketed",
                "source_clock_domains": ["android_uptime_ms", "device_elapsed_realtime_ns"],
                "transform_id": "fixture-transform"
            },
            "evidence": [
                {"kind": "ime_event", "line_index": 1_u64, "t_uptime_ms": 200_i64}
            ],
        })
    }

    fn unsupported_dismissal_fixture() -> Value {
        json!({
            "event": "dismissal_inference",
            "inference_id": "dismissal-unsupported",
            "inferred_dismissal": "focus_or_app_hide_unknown",
            "confidence": {"decimal_ppm": 250_000_i64},
            "time_delta_ms": Value::Null,
            "clock_alignment_status": "unsupported_clock_domain",
            "clock_alignment": {
                "status": "unsupported_clock_domain",
                "source_clock_domains": ["android_uptime_ms", "kernel_getevent_us"]
            },
            "evidence": [
                {"kind": "ime_event", "line_index": 1_u64, "t_uptime_ms": 210_i64}
            ],
        })
    }

    fn legacy_dismissal_fixture() -> Value {
        let mut fixture = dismissal_fixture();
        if let Some(object) = fixture.as_object_mut() {
            object.insert(String::from("inference_id"), json!("dismissal-legacy"));
        }
        fixture
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
