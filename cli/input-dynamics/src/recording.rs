//! Read-only inspection for local recording directories.

use std::ffi::OsStr;
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use input_dynamics_analysis::clock::{AlignmentStatus, ClockDomain};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::clock_probe::{validate_probe_marker, validate_probe_order};
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
}

struct ClockInspection {
    value: Value,
    canonical_clock_ready: bool,
    has_legacy_timing: bool,
    needs_canonical_recording: bool,
    warnings: Vec<String>,
}

struct FlagInputs<'a> {
    manifest: Option<&'a Value>,
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
    let video = inspect_video(dir, manifest_ref, &artifacts)?;
    let video_map = inspect_video_map(dir)?;
    let clock = inspect_clock(dir, manifest_ref, &timeline, &video)?;
    let note_flags = note_flags(dir)?;
    let flag_inputs = FlagInputs {
        manifest: manifest_ref,
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
    let next_actions = next_actions(dir, external_run_id.as_deref(), &flags)?;
    let warnings = warnings(&flag_inputs);
    Ok(inspection_json(&InspectionJson {
        dir,
        external_run_id,
        manifest_ref,
        artifacts,
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
        "recording_dir": path_text(input.dir),
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
        "session_jsonl": session_json(input.dir, input.session),
        "validation": {
            "stored_present": input.validation.stored_present,
            "stored": input.validation.stored,
            "current": input.validation.current,
            "stale_reasons": input.validation.stale_reasons,
        },
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

fn inspect_video_map(dir: &Path) -> CliResult<VideoMapInspection> {
    let index_path = dir.join("derived").join("video_map").join("index.json");
    let frames_path = dir.join("derived").join("video_map").join("frames.jsonl");
    let index_exists = index_path.exists();
    let frames_exists = frames_path.exists();
    if !index_exists && !frames_exists {
        return Ok(VideoMapInspection {
            exists: false,
            frame_index_exists: false,
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
            ..VideoMapInspection::default()
        });
    };
    if index.get("schema").and_then(Value::as_str) != Some("input_dynamics_video_map_index.v1") {
        stale_reasons.push(String::from("video map index schema is unsupported"));
    }
    if index.get("artifact_stage").and_then(Value::as_str) != Some("frame_index") {
        stale_reasons.push(String::from(
            "video map index artifact stage is not frame_index",
        ));
    }
    if let Some(sources) = index.get("sources").and_then(Value::as_array) {
        for source in sources {
            stale_reasons.extend(video_map_source_stale_reasons(dir, source)?);
        }
    } else {
        stale_reasons.push(String::from("video map index has no sources array"));
    }
    stale_reasons.extend(video_map_output_stale_reasons(dir, &index, &frames_path)?);
    let frame_index_exists = index_exists && frames_exists && stale_reasons.is_empty();
    Ok(VideoMapInspection {
        stage: index.get("artifact_stage").cloned().unwrap_or(Value::Null),
        frame_count: index.get("frame_count").cloned().unwrap_or(Value::Null),
        probe_status: index.get("probe_status").cloned().unwrap_or(Value::Null),
        event_mapping: index.get("event_mapping").cloned().unwrap_or(Value::Null),
        stale_reasons,
        exists: index_exists && frames_exists,
        frame_index_exists,
    })
}

fn video_map_output_stale_reasons(
    dir: &Path,
    index: &Value,
    frames_path: &Path,
) -> CliResult<Vec<String>> {
    let mut reasons = Vec::new();
    let Some(output) = index.pointer("/outputs/video_map_frames") else {
        reasons.push(String::from(
            "video map index has no frames output metadata",
        ));
        return Ok(reasons);
    };
    if output.get("schema").and_then(Value::as_str) != Some("input_dynamics_video_frame.v1") {
        reasons.push(String::from(
            "video map frames output schema is unsupported",
        ));
    }
    let Some(path_text_value) = output.get("path").and_then(Value::as_str) else {
        reasons.push(String::from("video map frames output has no path"));
        return Ok(reasons);
    };
    let recorded_output_path = source_path(dir, path_text_value);
    if recorded_output_path != frames_path {
        reasons.push(String::from("video map frames output path changed"));
    }
    let Some(recorded_count) = output.get("record_count").and_then(Value::as_u64) else {
        reasons.push(String::from("video map frames output has no record count"));
        return Ok(reasons);
    };
    let Some(recorded_sha) = output
        .pointer("/fingerprint/sha256")
        .and_then(Value::as_str)
    else {
        reasons.push(String::from("video map frames output has no fingerprint"));
        return Ok(reasons);
    };
    if !frames_path.exists() {
        return Ok(reasons);
    }
    let current = file_fingerprint(frames_path)?;
    let current_sha = current.get("sha256").and_then(Value::as_str);
    if Some(recorded_sha) != current_sha {
        reasons.push(String::from("video map frames fingerprint changed"));
    }
    let current_count = count_nonempty_lines(frames_path)?;
    if recorded_count != current_count {
        reasons.push(String::from("video map frames record count changed"));
    }
    Ok(reasons)
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

fn derived_artifact_specs(dir: &Path) -> [ArtifactSpec; 8] {
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
    let video_frame_index_ready =
        has_video && inputs.clock.canonical_clock_ready && !inputs.clock.needs_canonical_recording;
    let has_video_frame_index = video_frame_index_ready && inputs.video_map.frame_index_exists;
    let needs_video_frame_index = video_frame_index_ready && !inputs.video_map.frame_index_exists;
    let has_evidence = artifact_exists(inputs.artifacts, "evidence_start_index")
        || artifact_exists(inputs.artifacts, "evidence_end_index");
    let needs_derivation = !has_touch_gestures || !has_dismissals;
    let needs_run_summary =
        !inputs.run_summary.exists || !inputs.run_summary.stale_reasons.is_empty();
    let needs_timeline = !inputs.timeline.exists || !inputs.timeline.stale_reasons.is_empty();
    let needs_canonical_video = bool_at(&inputs.clock.value, "/video/required")
        && !bool_at(&inputs.clock.value, "/video/canonical");
    let needs_canonical_evidence = bool_at(&inputs.clock.value, "/evidence/requested")
        && !bool_at(&inputs.clock.value, "/evidence/canonical");
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
            && inputs.validation.current_ok
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
        "has_video_map": false,
        "has_sensitive_evidence": has_evidence || has_video,
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
    add_record_next_action(&mut actions, flags);
    add_derivation_next_actions(&mut actions, dir, flags)?;
    Ok(Value::Array(actions))
}

fn add_record_next_action(actions: &mut Vec<Value>, flags: &Value) {
    if bool_at(flags, "/needs_video") || bool_at(flags, "/needs_canonical_recording") {
        let include_evidence = bool_at(flags, "/needs_canonical_evidence");
        let kind = if bool_at(flags, "/needs_video") {
            "record_with_video"
        } else {
            "record_with_canonical_clocks"
        };
        let mut command =
            String::from("input-dynamics record --run-id <new-run-id> --out <new-run-dir>");
        if include_evidence {
            command.push_str(" --with-evidence");
        }
        let reason = match (include_evidence, bool_at(flags, "/needs_video")) {
            (true, true) => {
                "rerun with video and evidence to refresh request-correlated device clock anchors"
            }
            (true, false) => {
                "rerun with evidence to refresh request-correlated device clock anchors"
            }
            (false, true) => "rerun with video to refresh request-correlated device clock anchors",
            (false, false) => {
                "rerun through the canonical recorder to refresh device clock anchors"
            }
        };
        actions.push(json!({
            "kind": kind,
            "command": command,
            "reason": reason,
        }));
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
    if bool_at(flags, "/needs_video_frame_index") {
        actions.push(json!({
            "kind": "derive_video_map",
            "command": format!(
                "input-dynamics derive video-map --recording-dir {}",
                shellish(dir)?
            ),
            "reason": "derive or refresh the encoded video frame index",
        }));
    }
    Ok(())
}

fn warnings(inputs: &FlagInputs<'_>) -> Vec<String> {
    let mut warnings = inputs.session.warnings.clone();
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
