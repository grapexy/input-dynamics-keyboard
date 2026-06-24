//! Encoded video frame and event-frame-map derivation.

use std::fmt::Write as FmtWrite;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::clock::{AlignmentStatus, ClockDomain, TimestampPrecision, TimestampSource};
use crate::derivation::jsonl::write_jsonl;
use crate::derivation::{
    DERIVATION_SUMMARY_SCHEMA, DeriveError, DeriveResult, EVENT_VIDEO_FRAME_MAP_SCHEMA,
    TIMELINE_INDEX_SCHEMA, VIDEO_ALIGNMENT_SCHEMA, VIDEO_FRAME_SCHEMA, VIDEO_MAP_INDEX_SCHEMA,
    path_text, read_manifest,
};

const SHA256_PREFIX: &str = "sha256:";
const ARTIFACT_STAGE_FRAME_INDEX: &str = "frame_index";
const ARTIFACT_STAGE_EVENT_FRAME_MAP: &str = "event_frame_map";
const UPTIME_TO_ELAPSED_TRANSFORM_ID: &str = "android_uptime_to_device_elapsed_offset:v1";
const ELAPSED_TO_MEDIA_TRANSFORM_ID: &str =
    "video_alignment:device_elapsed_realtime_ns_to_media_pts_ns:v1";
const LEGACY_DEVICE_WALL_TO_MEDIA_TRANSFORM_ID: &str = "legacy_device_wall_ms_to_media_pts:v1";
const FRAME_SELECTION_POLICY: &str = "interval_overlap_with_midpoint_nominal";
const NANOS_PER_MILLI: i64 = 1_000_000;

/// Captured process provenance for the `ffprobe` invocation.
#[derive(Clone, Debug)]
pub struct FfprobeInvocation {
    /// Executable name or path used by the caller.
    pub executable_path: String,
    /// First line of `ffprobe -version` output.
    pub version_first_line: String,
    /// Exact frame-probe arguments passed by the caller.
    pub args: Vec<String>,
    /// Probe process exit status.
    pub status_code: Option<i32>,
    /// Probe stderr, trimmed by the caller if desired.
    pub stderr: String,
}

/// Configuration for deriving an encoded video frame index.
#[derive(Clone, Debug)]
pub struct DeriveVideoMapConfig {
    /// Recording directory created by `input-dynamics record`.
    pub recording_dir: PathBuf,
    /// Video-map output directory. Defaults to `derived/video_map`.
    pub output_dir: Option<PathBuf>,
    /// JSON emitted by `ffprobe` for `video/screen.mp4`.
    pub ffprobe_json: String,
    /// Provenance for the `ffprobe` execution that produced `ffprobe_json`.
    pub ffprobe: FfprobeInvocation,
}

#[derive(Clone, Debug)]
struct VideoMapPaths {
    video_screen: PathBuf,
    video_timing: PathBuf,
    timeline_index: PathBuf,
    timeline_events: PathBuf,
    output_dir: PathBuf,
    index_output: PathBuf,
    frames_output: PathBuf,
    alignment_output: PathBuf,
    event_frames_output: PathBuf,
}

#[derive(Clone, Debug)]
struct FrameRecord {
    sequence: u64,
    pts_ns: i64,
    pts_time_seconds: Option<String>,
    pts_tick: i64,
    source_field: &'static str,
    time_base: TimeBase,
    rounding: RationalRounding,
    duration_ns: Option<i64>,
    duration_time_seconds: Option<String>,
    duration_tick: Option<i64>,
    pts_interval_ns: Option<i64>,
    is_key_frame: bool,
    pict_type: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    encoded_size_bytes: Option<i64>,
}

#[derive(Clone, Debug)]
struct ParsedVideoMap {
    stream: ProbeStream,
    frames: Vec<FrameRecord>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct TimelineRecord {
    line_index: u64,
    value: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimeInterval {
    start_ns: i64,
    end_ns: i64,
}

#[derive(Clone, Debug)]
struct AlignmentModel {
    status: AlignmentStatus,
    transform_id: &'static str,
    uptime_offset_interval: TimeInterval,
    media_origin_interval: TimeInterval,
    legacy_wall_media_origin_interval: Option<TimeInterval>,
    legacy_wall_status: Option<AlignmentStatus>,
    media_extent: TimeInterval,
    warnings: Vec<String>,
    reasons: Vec<String>,
    alignment_json: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeviceProbe {
    elapsed_realtime_ns: i64,
    uptime_ns: i64,
    wall_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VideoAnchors {
    start_before: DeviceProbe,
    start_after: DeviceProbe,
    stop_before: DeviceProbe,
    stop_after: DeviceProbe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LegacyWallAlignment {
    origin_interval: TimeInterval,
    status: AlignmentStatus,
    constraints_overlap: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameInterval {
    frame_sequence: u64,
    start_ns: i64,
    end_ns: i64,
}

#[derive(Clone, Debug)]
struct EventTimeInput {
    interval: TimeInterval,
    status: AlignmentStatus,
    clock_domain: &'static str,
    transform_id: &'static str,
    media_transform_id: &'static str,
    media_origin_interval: TimeInterval,
    source_time_status: String,
    reasons: Vec<String>,
}

#[derive(Clone, Debug)]
struct FrameWindow {
    start: u64,
    end: u64,
    nominal: u64,
}

#[derive(Clone, Debug)]
struct EventMappingResult {
    rows: Vec<Value>,
    status_counts: Value,
    mapped_event_count: u64,
    unmapped_event_count: u64,
    uncertainty_summary_ns: Value,
    warnings: Vec<String>,
}

struct IndexJsonInputs<'a> {
    config: &'a DeriveVideoMapConfig,
    paths: &'a VideoMapPaths,
    manifest: &'a Value,
    timeline_index: &'a Value,
    parsed: &'a ParsedVideoMap,
    frames: &'a [Value],
    alignment: &'a AlignmentModel,
    event_mapping: &'a EventMappingResult,
    timeline_event_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimeBase {
    numerator: i64,
    denominator: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RationalRounding {
    policy: &'static str,
    remainder_num: i128,
    denominator: i128,
}

#[derive(Clone, Debug, Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    frames: Vec<ProbeFrame>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProbeStream {
    index: Option<i64>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    duration: Option<String>,
    nb_frames: Option<String>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    time_base: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProbeFrame {
    media_type: Option<String>,
    key_frame: Option<Value>,
    pts: Option<Value>,
    pts_time: Option<String>,
    best_effort_timestamp: Option<Value>,
    best_effort_timestamp_time: Option<String>,
    duration: Option<Value>,
    duration_time: Option<String>,
    pkt_size: Option<Value>,
    width: Option<i64>,
    height: Option<i64>,
    pict_type: Option<String>,
}

impl VideoMapPaths {
    fn from_config(config: &DeriveVideoMapConfig) -> Self {
        let output_dir = config
            .output_dir
            .clone()
            .unwrap_or_else(|| config.recording_dir.join("derived").join("video_map"));
        Self {
            video_screen: config.recording_dir.join("video").join("screen.mp4"),
            video_timing: config.recording_dir.join("video").join("timing.json"),
            timeline_index: config
                .recording_dir
                .join("derived")
                .join("timeline")
                .join("index.json"),
            timeline_events: config
                .recording_dir
                .join("derived")
                .join("timeline")
                .join("events.jsonl"),
            index_output: output_dir.join("index.json"),
            frames_output: output_dir.join("frames.jsonl"),
            alignment_output: output_dir.join("alignment.json"),
            event_frames_output: output_dir.join("event_frames.jsonl"),
            output_dir,
        }
    }
}

/// Derive encoded video-frame metadata from a recording video.
pub fn derive_video_map(config: &DeriveVideoMapConfig) -> DeriveResult<Value> {
    let paths = VideoMapPaths::from_config(config);
    let manifest = read_manifest(&config.recording_dir)?;
    require_file(&paths.video_screen, "video file")?;
    require_file(&paths.video_timing, "video timing metadata")?;
    require_file(&paths.timeline_index, "timeline index")?;
    require_file(&paths.timeline_events, "timeline events")?;
    let parsed = parse_probe_output(&config.ffprobe_json)?;
    let video_timing = read_json_file(&paths.video_timing)?;
    let timeline_index = read_json_file(&paths.timeline_index)?;
    let timeline_events = read_jsonl_records(&paths.timeline_events)?;
    validate_timeline_bundle(
        &config.recording_dir,
        &timeline_index,
        timeline_events.len(),
    )?;
    fs::create_dir_all(&paths.output_dir)?;

    let source_video = source_ref(&config.recording_dir, "video_screen", &paths.video_screen)?;
    let frame_values = frame_json_values(&parsed.frames, &source_video);
    write_jsonl(&paths.frames_output, &frame_values)?;
    let alignment = build_alignment(&video_timing, &parsed.frames)?;
    write_json_file(&paths.alignment_output, &alignment.alignment_json)?;
    let event_mapping = map_timeline_events(&timeline_events, &parsed.frames, &alignment)?;
    write_jsonl(&paths.event_frames_output, &event_mapping.rows)?;
    let index = index_json(&IndexJsonInputs {
        config,
        paths: &paths,
        manifest: &manifest,
        timeline_index: &timeline_index,
        parsed: &parsed,
        frames: &frame_values,
        alignment: &alignment,
        event_mapping: &event_mapping,
        timeline_event_count: timeline_events.len(),
    })?;
    write_json_file(&paths.index_output, &index)?;
    let derivation_warnings = merge_warning_lists(
        &merge_warning_lists(&parsed.warnings, &alignment.warnings),
        &event_mapping.warnings,
    );

    Ok(json!({
        "ok": true,
        "schema": DERIVATION_SUMMARY_SCHEMA,
        "derivation": "video_map",
        "artifact_stage": ARTIFACT_STAGE_EVENT_FRAME_MAP,
        "recording_dir": path_text(&config.recording_dir),
        "output_dir": path_text(&paths.output_dir),
        "index_output": path_text(&paths.index_output),
        "frames_output": path_text(&paths.frames_output),
        "alignment_output": path_text(&paths.alignment_output),
        "event_frames_output": path_text(&paths.event_frames_output),
        "frame_count": frame_values.len(),
        "probe_status": "ok",
        "alignment_status": alignment.status.as_str(),
        "event_mapping": {
            "status": alignment.status.as_str(),
            "source_event_count": timeline_events.len(),
            "row_count": event_mapping.rows.len(),
            "mapped_event_count": event_mapping.mapped_event_count,
            "unmapped_event_count": event_mapping.unmapped_event_count,
            "status_counts": event_mapping.status_counts,
        },
        "warnings": derivation_warnings,
    }))
}

fn parse_probe_output(text: &str) -> DeriveResult<ParsedVideoMap> {
    let output: ProbeOutput = serde_json::from_str(text)?;
    let stream = select_video_stream(&output.streams)?;
    let time_base = stream_time_base(&stream)?;
    let mut warnings = Vec::new();
    let mut frames = Vec::new();
    for frame in output.frames {
        match frame.media_type.as_deref() {
            Some("video") => {}
            Some(_non_video) => continue,
            None => {
                return Err(DeriveError::new(
                    "ffprobe frame is missing media_type; expected video",
                ));
            }
        }
        let sequence = u64::try_from(frames.len())
            .map_err(|error| DeriveError::new(format!("frame sequence overflow: {error}")))?
            .checked_add(1)
            .ok_or_else(|| DeriveError::new("frame sequence overflow"))?;
        frames.push(parse_frame(
            sequence,
            &stream,
            time_base,
            &frame,
            &mut warnings,
        )?);
    }
    if frames.is_empty() {
        return Err(DeriveError::new("ffprobe output contains no video frames"));
    }
    apply_frame_intervals(&mut frames)?;
    Ok(ParsedVideoMap {
        stream,
        frames,
        warnings,
    })
}

fn select_video_stream(streams: &[ProbeStream]) -> DeriveResult<ProbeStream> {
    streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"))
        .cloned()
        .ok_or_else(|| DeriveError::new("ffprobe output contains no video stream"))
}

fn stream_time_base(stream: &ProbeStream) -> DeriveResult<TimeBase> {
    let Some(text) = stream.time_base.as_deref() else {
        return Err(DeriveError::new(
            "ffprobe video stream is missing time_base",
        ));
    };
    parse_time_base(text)
}

fn parse_frame(
    sequence: u64,
    stream: &ProbeStream,
    time_base: TimeBase,
    frame: &ProbeFrame,
    warnings: &mut Vec<String>,
) -> DeriveResult<FrameRecord> {
    let (pts_tick, source_field) = frame_timestamp_tick(frame, sequence, warnings)?;
    let (pts_ns, rounding) = ticks_to_nanos(pts_tick, time_base)?;
    let duration_time_seconds = frame.duration_time.clone();
    let duration_tick = value_to_i64(frame.duration.as_ref());
    let duration_ns = match duration_tick {
        Some(tick) => Some(ticks_to_nanos(tick, time_base)?.0),
        None => duration_time_seconds
            .as_deref()
            .map(|text| parse_seconds_to_nanos(text, "frame duration_time"))
            .transpose()?,
    };
    if duration_tick.is_none() && duration_time_seconds.is_none() {
        push_once(
            warnings,
            "one or more frames are missing ffprobe duration and duration_time; duration_ns is null",
        );
    }
    Ok(FrameRecord {
        sequence,
        pts_ns,
        pts_time_seconds: frame
            .pts_time
            .clone()
            .or_else(|| frame.best_effort_timestamp_time.clone()),
        pts_tick,
        source_field,
        time_base,
        rounding,
        duration_ns,
        duration_time_seconds,
        duration_tick,
        pts_interval_ns: None,
        is_key_frame: key_frame_bool(frame.key_frame.as_ref())?,
        pict_type: frame.pict_type.clone(),
        width: frame.width.or(stream.width),
        height: frame.height.or(stream.height),
        encoded_size_bytes: value_to_i64(frame.pkt_size.as_ref()),
    })
}

fn frame_timestamp_tick(
    frame: &ProbeFrame,
    sequence: u64,
    warnings: &mut Vec<String>,
) -> DeriveResult<(i64, &'static str)> {
    if let Some(pts_tick) = value_to_i64(frame.pts.as_ref()) {
        return Ok((pts_tick, "pts"));
    }
    if let Some(best_effort_tick) = value_to_i64(frame.best_effort_timestamp.as_ref()) {
        warnings.push(format!(
            "frame {sequence} used best_effort_timestamp because pts was missing"
        ));
        return Ok((best_effort_tick, "best_effort_timestamp"));
    }
    Err(DeriveError::new(format!(
        "frame {sequence} has no pts or best_effort_timestamp"
    )))
}

fn apply_frame_intervals(frames: &mut [FrameRecord]) -> DeriveResult<()> {
    let mut index = 0_usize;
    while index < frames.len() {
        let next_index = index
            .checked_add(1)
            .ok_or_else(|| DeriveError::new("frame index overflow"))?;
        if next_index < frames.len() {
            let current_pts = frames
                .get(index)
                .map(|frame| frame.pts_ns)
                .ok_or_else(|| DeriveError::new("frame index out of range"))?;
            let next_pts = frames
                .get(next_index)
                .map(|frame| frame.pts_ns)
                .ok_or_else(|| DeriveError::new("frame index out of range"))?;
            if next_pts < current_pts {
                return Err(DeriveError::new(format!(
                    "frame presentation timestamps are not monotonic at frame {}",
                    frames.get(next_index).map_or(0_u64, |frame| frame.sequence)
                )));
            }
            let interval = next_pts
                .checked_sub(current_pts)
                .ok_or_else(|| DeriveError::new("frame interval overflow"))?;
            let Some(frame) = frames.get_mut(index) else {
                return Err(DeriveError::new("frame index out of range"));
            };
            frame.pts_interval_ns = Some(interval);
        }
        index = next_index;
    }
    Ok(())
}

fn frame_json_values(frames: &[FrameRecord], source_video: &Value) -> Vec<Value> {
    frames
        .iter()
        .map(|frame| {
            json!({
                "schema": VIDEO_FRAME_SCHEMA,
                "event": "video_frame",
                "artifact_stage": ARTIFACT_STAGE_FRAME_INDEX,
                "frame_id": format!("frame:{:08}", frame.sequence),
                "frame_sequence": frame.sequence,
                "clock_domain": ClockDomain::MediaPtsNs.as_str(),
                "media_time": {
                    "clock_domain": ClockDomain::MediaPtsNs.as_str(),
                    "timestamp_source": TimestampSource::MediaProbe.as_str(),
                    "timestamp_precision": TimestampPrecision::Nanoseconds.as_str(),
                    "field": "pts_ns",
                    "source_field": frame.source_field,
                    "pts_ns": frame.pts_ns,
                    "pts_time_seconds": frame.pts_time_seconds,
                    "pts_tick": frame.pts_tick,
                    "source_time_base": frame.time_base.to_json(),
                    "rounding": frame.rounding.to_json(),
                },
                "duration_ns": frame.duration_ns,
                "duration_time_seconds": frame.duration_time_seconds,
                "duration_tick": frame.duration_tick,
                "pts_interval_ns": frame.pts_interval_ns,
                "is_key_frame": frame.is_key_frame,
                "pict_type": frame.pict_type,
                "width": frame.width,
                "height": frame.height,
                "encoded_size_bytes": frame.encoded_size_bytes,
                "source_video": source_video,
            })
        })
        .collect()
}

impl TimeInterval {
    fn new(start_ns: i64, end_ns: i64) -> DeriveResult<Self> {
        if end_ns < start_ns {
            return Err(DeriveError::new(format!(
                "time interval end is before start: {start_ns}..{end_ns}"
            )));
        }
        Ok(Self { start_ns, end_ns })
    }

    fn width_ns(self) -> DeriveResult<i64> {
        self.end_ns
            .checked_sub(self.start_ns)
            .ok_or_else(|| DeriveError::new("time interval width overflow"))
    }

    fn midpoint_ns(self) -> DeriveResult<i64> {
        let width = self.width_ns()?;
        self.start_ns
            .checked_add(
                width
                    .checked_div(2_i64)
                    .ok_or_else(|| DeriveError::new("time interval midpoint division failed"))?,
            )
            .ok_or_else(|| DeriveError::new("time interval midpoint overflow"))
    }

    fn to_json(self) -> Value {
        json!([self.start_ns, self.end_ns])
    }
}

#[allow(clippy::too_many_lines)]
fn build_alignment(timing: &Value, frames: &[FrameRecord]) -> DeriveResult<AlignmentModel> {
    let anchors = match video_anchors(timing) {
        Ok(value) => value,
        Err(error) => return build_legacy_wall_alignment(timing, frames, &error.to_string()),
    };
    let uptime_offset_interval = uptime_offset_interval(&anchors)?;
    let media_extent = media_extent_interval(frames)?;
    let legacy_wall_alignment = legacy_wall_alignment(&anchors, media_extent)?;
    let media_origin_from_start = TimeInterval::new(
        anchors.start_before.elapsed_realtime_ns,
        anchors.start_after.elapsed_realtime_ns,
    )?;
    let media_origin_from_stop = TimeInterval::new(
        anchors
            .stop_before
            .elapsed_realtime_ns
            .checked_sub(media_extent.end_ns)
            .ok_or_else(|| DeriveError::new("stop-before media-origin underflow"))?,
        anchors
            .stop_after
            .elapsed_realtime_ns
            .checked_sub(media_extent.end_ns)
            .ok_or_else(|| DeriveError::new("stop-after media-origin underflow"))?,
    )?;
    let start_max = media_origin_from_start
        .start_ns
        .max(media_origin_from_stop.start_ns);
    let end_min = media_origin_from_start
        .end_ns
        .min(media_origin_from_stop.end_ns);
    let mut warnings = Vec::new();
    let mut reasons = vec![
        String::from("screenrecord_start_bracket"),
        String::from("screenrecord_stop_bracket"),
        String::from("ffprobe_pts_index"),
    ];
    let (status, media_origin_interval) = if start_max <= end_min {
        (
            AlignmentStatus::Bracketed,
            TimeInterval::new(start_max, end_min)?,
        )
    } else {
        warnings.push(String::from(
            "screenrecord start and stop media-origin constraints do not overlap; using widened estimate",
        ));
        reasons.push(String::from("start_stop_origin_constraints_disagree"));
        (
            AlignmentStatus::Estimated,
            TimeInterval::new(
                media_origin_from_start
                    .start_ns
                    .min(media_origin_from_stop.start_ns),
                media_origin_from_start
                    .end_ns
                    .max(media_origin_from_stop.end_ns),
            )?,
        )
    };
    let alignment_json = json!({
        "schema": VIDEO_ALIGNMENT_SCHEMA,
        "alignment_id": ELAPSED_TO_MEDIA_TRANSFORM_ID,
        "status": status.as_str(),
        "source_clock_domain": ClockDomain::DeviceElapsedRealtimeNs.as_str(),
        "target_clock_domain": ClockDomain::MediaPtsNs.as_str(),
        "mapping_model": {
            "kind": "offset_interval",
            "formula": "media_pts_ns = source_time_ns - video_zero_time_ns",
            "video_zero_time_interval_ns": media_origin_interval.to_json(),
            "video_zero_time_interval_width_ns": media_origin_interval.width_ns()?,
        },
        "android_uptime_transform": {
            "transform_id": UPTIME_TO_ELAPSED_TRANSFORM_ID,
            "source_clock_domain": ClockDomain::AndroidUptimeMs.as_str(),
            "target_clock_domain": ClockDomain::DeviceElapsedRealtimeNs.as_str(),
            "offset_interval_ns": uptime_offset_interval.to_json(),
            "offset_spread_ns": uptime_offset_interval.width_ns()?,
        },
        "legacy_wall_transform": {
            "transform_id": LEGACY_DEVICE_WALL_TO_MEDIA_TRANSFORM_ID,
            "source_clock_domain": ClockDomain::DeviceWallMs.as_str(),
            "target_clock_domain": ClockDomain::MediaPtsNs.as_str(),
            "status": legacy_wall_alignment
                .map_or(AlignmentStatus::NotEstimated.as_str(), |value| value.status.as_str()),
            "video_zero_time_interval_ns": legacy_wall_alignment
                .map_or(Value::Null, |value| value.origin_interval.to_json()),
            "start_stop_constraints_overlap": legacy_wall_alignment
                .map_or(Value::Null, |value| json!(value.constraints_overlap)),
        },
        "media_pts_extent_ns": {
            "first_pts_ns": media_extent.start_ns,
            "last_frame_end_estimate_ns": media_extent.end_ns,
        },
        "anchors": {
            "video_start": {
                "source": "video/timing.json",
                "before": probe_json(anchors.start_before),
                "after": probe_json(anchors.start_after),
                "origin_interval_from_start_ns": media_origin_from_start.to_json(),
            },
            "video_stop": {
                "source": "video/timing.json",
                "before": probe_json(anchors.stop_before),
                "after": probe_json(anchors.stop_after),
                "origin_interval_from_stop_ns": media_origin_from_stop.to_json(),
            },
        },
        "duration_consistency": {
            "status": if status == AlignmentStatus::Bracketed { "ok" } else { "estimated" },
            "media_duration_ns": media_extent.width_ns()?,
            "start_stop_constraints_overlap": status == AlignmentStatus::Bracketed,
        },
        "uncertainty_budget_ns": {
            "video_origin_interval": media_origin_interval.width_ns()?,
            "uptime_offset_spread": uptime_offset_interval.width_ns()?,
            "media_pts_rounding": 1_i64,
            "total_policy": "interval_bounds",
        },
        "reasons": reasons,
        "warnings": warnings,
    });
    Ok(AlignmentModel {
        status,
        transform_id: ELAPSED_TO_MEDIA_TRANSFORM_ID,
        uptime_offset_interval,
        media_origin_interval,
        legacy_wall_media_origin_interval: legacy_wall_alignment.map(|value| value.origin_interval),
        legacy_wall_status: legacy_wall_alignment.map(|value| value.status),
        media_extent,
        warnings,
        reasons,
        alignment_json,
    })
}

#[allow(clippy::too_many_lines)]
fn build_legacy_wall_alignment(
    timing: &Value,
    frames: &[FrameRecord],
    canonical_error: &str,
) -> DeriveResult<AlignmentModel> {
    let media_extent = media_extent_interval(frames)?;
    let start_before_wall_ms = required_i64_at(timing, "/start/before/device_wall_ms")?;
    let start_after_wall_ms = required_i64_at(timing, "/start/after/device_wall_ms")?;
    let stop_before_wall_ms = required_i64_at(timing, "/stop/before/device_wall_ms")?;
    let stop_after_wall_ms = required_i64_at(timing, "/stop/after/device_wall_ms")?;
    let start_interval = TimeInterval::new(
        millis_to_nanos_checked(start_before_wall_ms)?,
        millis_to_nanos_checked(start_after_wall_ms)?,
    )?;
    let stop_interval = TimeInterval::new(
        millis_to_nanos_checked(stop_before_wall_ms)?
            .checked_sub(media_extent.end_ns)
            .ok_or_else(|| DeriveError::new("legacy stop-before media-origin underflow"))?,
        millis_to_nanos_checked(stop_after_wall_ms)?
            .checked_sub(media_extent.end_ns)
            .ok_or_else(|| DeriveError::new("legacy stop-after media-origin underflow"))?,
    )?;
    let constraints_overlap = start_interval.start_ns.max(stop_interval.start_ns)
        <= start_interval.end_ns.min(stop_interval.end_ns);
    let legacy_wall_media_origin_interval = if constraints_overlap {
        TimeInterval::new(
            start_interval.start_ns.max(stop_interval.start_ns),
            start_interval.end_ns.min(stop_interval.end_ns),
        )?
    } else {
        TimeInterval::new(
            start_interval.start_ns.min(stop_interval.start_ns),
            start_interval.end_ns.max(stop_interval.end_ns),
        )?
    };
    let mut warnings = vec![format!(
        "canonical device-elapsed video anchors unavailable; using legacy wall-clock anchors: {canonical_error}"
    )];
    let mut reasons = vec![
        String::from("legacy_wall_clock_video_anchors"),
        String::from("ffprobe_pts_index"),
    ];
    let status = if constraints_overlap {
        AlignmentStatus::LegacyWallClockBracketed
    } else {
        warnings.push(String::from(
            "legacy screenrecord start and stop media-origin constraints do not overlap; using widened estimate",
        ));
        reasons.push(String::from(
            "legacy_start_stop_origin_constraints_disagree",
        ));
        AlignmentStatus::Estimated
    };
    let alignment_json = json!({
        "schema": VIDEO_ALIGNMENT_SCHEMA,
        "alignment_id": LEGACY_DEVICE_WALL_TO_MEDIA_TRANSFORM_ID,
        "status": status.as_str(),
        "source_clock_domain": ClockDomain::DeviceWallMs.as_str(),
        "target_clock_domain": ClockDomain::MediaPtsNs.as_str(),
        "mapping_model": {
            "kind": "legacy_wall_offset_interval",
            "formula": "media_pts_ns = device_wall_ns - video_zero_time_ns",
            "video_zero_time_interval_ns": legacy_wall_media_origin_interval.to_json(),
            "video_zero_time_interval_width_ns": legacy_wall_media_origin_interval.width_ns()?,
        },
        "android_uptime_transform": {
            "transform_id": UPTIME_TO_ELAPSED_TRANSFORM_ID,
            "status": AlignmentStatus::NotEstimated.as_str(),
            "offset_interval_ns": Value::Null,
        },
        "legacy_wall_transform": {
            "transform_id": LEGACY_DEVICE_WALL_TO_MEDIA_TRANSFORM_ID,
            "source_clock_domain": ClockDomain::DeviceWallMs.as_str(),
            "target_clock_domain": ClockDomain::MediaPtsNs.as_str(),
            "status": status.as_str(),
            "video_zero_time_interval_ns": legacy_wall_media_origin_interval.to_json(),
        },
        "media_pts_extent_ns": {
            "first_pts_ns": media_extent.start_ns,
            "last_frame_end_estimate_ns": media_extent.end_ns,
        },
        "duration_consistency": {
            "status": status.as_str(),
            "media_duration_ns": media_extent.width_ns()?,
            "start_stop_constraints_overlap": constraints_overlap,
        },
        "uncertainty_budget_ns": {
            "legacy_wall_origin_interval": legacy_wall_media_origin_interval.width_ns()?,
            "media_pts_rounding": 1_i64,
            "total_policy": "legacy_interval_bounds",
        },
        "reasons": reasons,
        "warnings": warnings,
    });
    Ok(AlignmentModel {
        status,
        transform_id: LEGACY_DEVICE_WALL_TO_MEDIA_TRANSFORM_ID,
        uptime_offset_interval: TimeInterval::new(0_i64, 0_i64)?,
        media_origin_interval: legacy_wall_media_origin_interval,
        legacy_wall_media_origin_interval: Some(legacy_wall_media_origin_interval),
        legacy_wall_status: Some(status),
        media_extent,
        warnings,
        reasons,
        alignment_json,
    })
}

fn video_anchors(timing: &Value) -> DeriveResult<VideoAnchors> {
    Ok(VideoAnchors {
        start_before: device_probe(timing, "/start/before")?,
        start_after: device_probe(timing, "/start/after")?,
        stop_before: device_probe(timing, "/stop/before")?,
        stop_after: device_probe(timing, "/stop/after")?,
    })
}

fn device_probe(root: &Value, pointer: &str) -> DeriveResult<DeviceProbe> {
    let Some(value) = root.pointer(pointer) else {
        return Err(DeriveError::new(format!(
            "video timing is missing canonical probe: {pointer}"
        )));
    };
    Ok(DeviceProbe {
        elapsed_realtime_ns: required_i64_at(value, "/t_elapsed_realtime_ns")?,
        uptime_ns: required_i64_at(value, "/t_uptime_ns")?,
        wall_ms: value.get("device_wall_ms").and_then(Value::as_i64),
    })
}

fn uptime_offset_interval(anchors: &VideoAnchors) -> DeriveResult<TimeInterval> {
    let probes = [
        anchors.start_before,
        anchors.start_after,
        anchors.stop_before,
        anchors.stop_after,
    ];
    let mut min_offset = i64::MAX;
    let mut max_offset = i64::MIN;
    for probe in probes {
        let offset = probe
            .elapsed_realtime_ns
            .checked_sub(probe.uptime_ns)
            .ok_or_else(|| DeriveError::new("uptime-to-elapsed offset underflow"))?;
        min_offset = min_offset.min(offset);
        max_offset = max_offset.max(offset);
    }
    TimeInterval::new(min_offset, max_offset)
}

fn legacy_wall_alignment(
    anchors: &VideoAnchors,
    media_extent: TimeInterval,
) -> DeriveResult<Option<LegacyWallAlignment>> {
    let (
        Some(start_before_wall_ms),
        Some(start_after_wall_ms),
        Some(stop_before_wall_ms),
        Some(stop_after_wall_ms),
    ) = (
        anchors.start_before.wall_ms,
        anchors.start_after.wall_ms,
        anchors.stop_before.wall_ms,
        anchors.stop_after.wall_ms,
    )
    else {
        return Ok(None);
    };
    let start_interval = TimeInterval::new(
        millis_to_nanos_checked(start_before_wall_ms)?,
        millis_to_nanos_checked(start_after_wall_ms)?,
    )?;
    let stop_interval = TimeInterval::new(
        millis_to_nanos_checked(stop_before_wall_ms)?
            .checked_sub(media_extent.end_ns)
            .ok_or_else(|| DeriveError::new("legacy stop-before media-origin underflow"))?,
        millis_to_nanos_checked(stop_after_wall_ms)?
            .checked_sub(media_extent.end_ns)
            .ok_or_else(|| DeriveError::new("legacy stop-after media-origin underflow"))?,
    )?;
    if start_interval.start_ns.max(stop_interval.start_ns)
        <= start_interval.end_ns.min(stop_interval.end_ns)
    {
        return Ok(Some(LegacyWallAlignment {
            origin_interval: TimeInterval::new(
                start_interval.start_ns.max(stop_interval.start_ns),
                start_interval.end_ns.min(stop_interval.end_ns),
            )?,
            status: AlignmentStatus::LegacyWallClockBracketed,
            constraints_overlap: true,
        }));
    }
    Ok(Some(LegacyWallAlignment {
        origin_interval: TimeInterval::new(
            start_interval.start_ns.min(stop_interval.start_ns),
            start_interval.end_ns.max(stop_interval.end_ns),
        )?,
        status: AlignmentStatus::Estimated,
        constraints_overlap: false,
    }))
}

fn media_extent_interval(frames: &[FrameRecord]) -> DeriveResult<TimeInterval> {
    let Some(first) = frames.first() else {
        return Err(DeriveError::new("media extent requires at least one frame"));
    };
    let Some(last) = frames.last() else {
        return Err(DeriveError::new("media extent requires at least one frame"));
    };
    let last_end = frame_end_ns(last)?;
    TimeInterval::new(first.pts_ns, last_end)
}

fn frame_end_ns(frame: &FrameRecord) -> DeriveResult<i64> {
    let duration = frame
        .pts_interval_ns
        .or(frame.duration_ns)
        .ok_or_else(|| DeriveError::new("final frame has no interval or duration"))?;
    frame
        .pts_ns
        .checked_add(duration)
        .ok_or_else(|| DeriveError::new("frame end timestamp overflow"))
}

fn probe_json(probe: DeviceProbe) -> Value {
    json!({
        "clock_domain": ClockDomain::DeviceElapsedRealtimeNs.as_str(),
        "t_elapsed_realtime_ns": probe.elapsed_realtime_ns,
        "t_uptime_ns": probe.uptime_ns,
        "device_wall_ms": probe.wall_ms,
    })
}

fn map_timeline_events(
    timeline_events: &[TimelineRecord],
    frames: &[FrameRecord],
    alignment: &AlignmentModel,
) -> DeriveResult<EventMappingResult> {
    let frame_intervals = frame_intervals(frames)?;
    let mut rows = Vec::new();
    let mut status_counts = std::collections::BTreeMap::<String, u64>::new();
    let mut mapped_event_count = 0_u64;
    let mut unmapped_event_count = 0_u64;
    let mut uncertainties = Vec::new();
    let mut warnings = Vec::new();
    for record in timeline_events {
        let mapping = map_one_event(record, &frame_intervals, alignment)?;
        let status = mapping
            .get("mapping_status")
            .and_then(Value::as_str)
            .unwrap_or(AlignmentStatus::UnsupportedClockDomain.as_str())
            .to_owned();
        let current = status_counts.get(&status).copied().unwrap_or(0_u64);
        status_counts.insert(status, current.saturating_add(1_u64));
        if mapping
            .get("frame_window")
            .is_some_and(|value| !value.is_null())
        {
            mapped_event_count = mapped_event_count.saturating_add(1_u64);
            if let Some(uncertainty) = mapping
                .pointer("/video_time/uncertainty_ns")
                .and_then(Value::as_i64)
            {
                uncertainties.push(uncertainty);
            }
        } else {
            unmapped_event_count = unmapped_event_count.saturating_add(1_u64);
        }
        if let Some(row_warnings) = mapping.get("warnings").and_then(Value::as_array) {
            for warning in row_warnings {
                if let Some(text) = warning.as_str() {
                    push_once(&mut warnings, text);
                }
            }
        }
        rows.push(mapping);
    }
    Ok(EventMappingResult {
        rows,
        status_counts: json!(status_counts),
        mapped_event_count,
        unmapped_event_count,
        uncertainty_summary_ns: uncertainty_summary(&uncertainties),
        warnings,
    })
}

#[allow(clippy::too_many_lines)]
fn map_one_event(
    record: &TimelineRecord,
    frames: &[FrameInterval],
    alignment: &AlignmentModel,
) -> DeriveResult<Value> {
    let event = record
        .value
        .get("event")
        .cloned()
        .unwrap_or_else(|| Value::String(String::from("unknown")));
    let event_id = record
        .value
        .get("timeline_event_id")
        .cloned()
        .unwrap_or_else(|| Value::String(format!("timeline:{:06}", record.line_index)));
    let record_kind = record
        .value
        .get("record_kind")
        .cloned()
        .unwrap_or(Value::Null);
    let source_ref = record
        .value
        .get("source_ref")
        .cloned()
        .unwrap_or(Value::Null);
    let time_input = event_time_input(&record.value, alignment)?;
    let Some(input) = time_input else {
        return Ok(event_mapping_row(&EventMappingRow {
            record,
            event,
            event_id,
            record_kind,
            source_ref,
            mapping_status: AlignmentStatus::UnsupportedClockDomain,
            mapping_input_time: Value::Null,
            video_time: Value::Null,
            frame_window: Value::Null,
            reasons: vec![String::from("no_mappable_event_time")],
            warnings: Vec::new(),
        }));
    };
    let mut reasons = input.reasons.clone();
    reasons.extend(alignment.reasons.clone());
    let media_interval = event_interval_to_media(&input)?;
    let clipped = clip_to_media_extent(media_interval, alignment.media_extent)?;
    let Some(clipped_interval) = clipped else {
        return Ok(event_mapping_row(&EventMappingRow {
            record,
            event,
            event_id,
            record_kind,
            source_ref,
            mapping_status: AlignmentStatus::OutsideRange,
            mapping_input_time: mapping_input_time_json(&input),
            video_time: video_time_json(
                media_interval,
                input.media_origin_interval,
                input.media_transform_id,
                &Value::Null,
            )?,
            frame_window: Value::Null,
            reasons: merge_owned(reasons, vec![String::from("outside_video_pts_extent")]),
            warnings: Vec::new(),
        }));
    };
    let mut warnings = Vec::new();
    if clipped_interval != media_interval {
        warnings.push(String::from(
            "event media interval was clipped to video extent",
        ));
        reasons.push(String::from("clipped_to_video_range"));
    }
    let frame_window = frame_window_for_interval(frames, clipped_interval)?;
    if frame_window.is_null() {
        return Ok(event_mapping_row(&EventMappingRow {
            record,
            event,
            event_id,
            record_kind,
            source_ref,
            mapping_status: AlignmentStatus::OutsideRange,
            mapping_input_time: mapping_input_time_json(&input),
            video_time: video_time_json(
                clipped_interval,
                input.media_origin_interval,
                input.media_transform_id,
                &json!(media_interval.to_json()),
            )?,
            frame_window: Value::Null,
            reasons: merge_owned(reasons, vec![String::from("no_overlapping_video_frame")]),
            warnings,
        }));
    }
    let mapping_status = combined_mapping_status(input.status, alignment.status);
    Ok(event_mapping_row(&EventMappingRow {
        record,
        event,
        event_id,
        record_kind,
        source_ref,
        mapping_status,
        mapping_input_time: mapping_input_time_json(&input),
        video_time: video_time_json(
            clipped_interval,
            input.media_origin_interval,
            input.media_transform_id,
            &json!(media_interval.to_json()),
        )?,
        frame_window,
        reasons,
        warnings,
    }))
}

struct EventMappingRow<'a> {
    record: &'a TimelineRecord,
    event: Value,
    event_id: Value,
    record_kind: Value,
    source_ref: Value,
    mapping_status: AlignmentStatus,
    mapping_input_time: Value,
    video_time: Value,
    frame_window: Value,
    reasons: Vec<String>,
    warnings: Vec<String>,
}

fn event_mapping_row(row: &EventMappingRow<'_>) -> Value {
    json!({
        "schema": EVENT_VIDEO_FRAME_MAP_SCHEMA,
        "timeline_event_id": row.event_id.clone(),
        "timeline_ref": {
            "path": "derived/timeline/events.jsonl",
            "line_index": row.record.line_index,
        },
        "event": row.event.clone(),
        "record_kind": row.record_kind.clone(),
        "source_ref": row.source_ref.clone(),
        "mapping_status": row.mapping_status.as_str(),
        "mapping_input_time": row.mapping_input_time.clone(),
        "video_time": row.video_time.clone(),
        "frame_window": row.frame_window.clone(),
        "reasons": row.reasons.clone(),
        "warnings": row.warnings.clone(),
    })
}

fn event_time_input(
    event: &Value,
    alignment: &AlignmentModel,
) -> DeriveResult<Option<EventTimeInput>> {
    if let Some(input) = normalized_device_elapsed_input(event, alignment)? {
        return Ok(Some(input));
    }
    if let Some(input) = canonical_uptime_input(event, alignment)? {
        return Ok(Some(input));
    }
    legacy_wall_input(event, alignment)
}

fn normalized_device_elapsed_input(
    event: &Value,
    alignment: &AlignmentModel,
) -> DeriveResult<Option<EventTimeInput>> {
    let Some(normalized) = event.get("normalized_time") else {
        return Ok(None);
    };
    if normalized.get("clock_domain").and_then(Value::as_str)
        != Some(ClockDomain::DeviceElapsedRealtimeNs.as_str())
    {
        return Ok(None);
    }
    let interval = if let Some(interval_value) = normalized.get("time_interval_ns") {
        interval_from_json_array(interval_value)?
    } else if let Some(time_ns) = normalized.get("time_ns").and_then(Value::as_i64) {
        TimeInterval::new(
            time_ns,
            time_ns
                .checked_add(1_i64)
                .ok_or_else(|| DeriveError::new("normalized event timestamp overflow"))?,
        )?
    } else {
        return Ok(None);
    };
    let status = match normalized.get("status").and_then(Value::as_str) {
        Some("bracketed") => AlignmentStatus::Bracketed,
        Some("estimated") => AlignmentStatus::Estimated,
        Some(_) | None => return Ok(None),
    };
    Ok(Some(EventTimeInput {
        interval,
        status,
        clock_domain: ClockDomain::DeviceElapsedRealtimeNs.as_str(),
        transform_id: "timeline_normalized_time",
        media_transform_id: ELAPSED_TO_MEDIA_TRANSFORM_ID,
        media_origin_interval: alignment.media_origin_interval,
        source_time_status: String::from("timeline_normalized_time"),
        reasons: vec![String::from("timeline_normalized_device_elapsed_time")],
    }))
}

fn canonical_uptime_input(
    event: &Value,
    alignment: &AlignmentModel,
) -> DeriveResult<Option<EventTimeInput>> {
    if alignment.status == AlignmentStatus::LegacyWallClockBracketed {
        return Ok(None);
    }
    let Some(source_time) = event.get("source_time") else {
        return Ok(None);
    };
    if source_time
        .get("source_clock_domain")
        .and_then(Value::as_str)
        != Some(ClockDomain::AndroidUptimeMs.as_str())
    {
        return Ok(None);
    }
    if source_time
        .get("source_time_status")
        .and_then(Value::as_str)
        != Some("canonical_event_time_metadata")
    {
        return Ok(None);
    }
    let Some(source_time_ms) = source_time.get("source_time_ms").and_then(Value::as_i64) else {
        return Ok(None);
    };
    let start_ns = source_time_ms
        .checked_mul(NANOS_PER_MILLI)
        .ok_or_else(|| DeriveError::new("uptime millisecond timestamp overflow"))?;
    let end_ns = source_time_ms
        .checked_add(1_i64)
        .and_then(|value| value.checked_mul(NANOS_PER_MILLI))
        .ok_or_else(|| DeriveError::new("uptime millisecond interval overflow"))?;
    let source_interval = TimeInterval::new(start_ns, end_ns)?;
    let elapsed_interval = TimeInterval::new(
        source_interval
            .start_ns
            .checked_add(alignment.uptime_offset_interval.start_ns)
            .ok_or_else(|| DeriveError::new("uptime-to-elapsed interval start overflow"))?,
        source_interval
            .end_ns
            .checked_add(alignment.uptime_offset_interval.end_ns)
            .ok_or_else(|| DeriveError::new("uptime-to-elapsed interval end overflow"))?,
    )?;
    Ok(Some(EventTimeInput {
        interval: elapsed_interval,
        status: AlignmentStatus::Bracketed,
        clock_domain: ClockDomain::DeviceElapsedRealtimeNs.as_str(),
        transform_id: UPTIME_TO_ELAPSED_TRANSFORM_ID,
        media_transform_id: ELAPSED_TO_MEDIA_TRANSFORM_ID,
        media_origin_interval: alignment.media_origin_interval,
        source_time_status: String::from("canonical_event_time_metadata"),
        reasons: vec![
            String::from("canonical_android_uptime_event_time"),
            String::from("millisecond_precision_expanded_to_interval"),
        ],
    }))
}

fn legacy_wall_input(
    event: &Value,
    alignment: &AlignmentModel,
) -> DeriveResult<Option<EventTimeInput>> {
    if alignment.legacy_wall_media_origin_interval.is_none() {
        return Ok(None);
    }
    let time_ms = event.get("t_wall_ms").and_then(Value::as_i64);
    let Some(time_ms_value) = time_ms else {
        return Ok(None);
    };
    let start_ns = millis_to_nanos_checked(time_ms_value)?;
    let end_ns = time_ms_value
        .checked_add(1_i64)
        .map(millis_to_nanos_checked)
        .ok_or_else(|| DeriveError::new("legacy wall millisecond interval overflow"))??;
    Ok(Some(EventTimeInput {
        interval: TimeInterval::new(start_ns, end_ns)?,
        status: alignment
            .legacy_wall_status
            .unwrap_or(AlignmentStatus::UnsupportedClockDomain),
        clock_domain: ClockDomain::DeviceWallMs.as_str(),
        transform_id: LEGACY_DEVICE_WALL_TO_MEDIA_TRANSFORM_ID,
        media_transform_id: LEGACY_DEVICE_WALL_TO_MEDIA_TRANSFORM_ID,
        media_origin_interval: alignment
            .legacy_wall_media_origin_interval
            .ok_or_else(|| DeriveError::new("legacy wall transform is unavailable"))?,
        source_time_status: String::from("legacy_wall_time"),
        reasons: vec![String::from("legacy_wall_time_mapping")],
    }))
}

fn event_interval_to_media(input: &EventTimeInput) -> DeriveResult<TimeInterval> {
    TimeInterval::new(
        input
            .interval
            .start_ns
            .checked_sub(input.media_origin_interval.end_ns)
            .ok_or_else(|| DeriveError::new("media interval start underflow"))?,
        input
            .interval
            .end_ns
            .checked_sub(input.media_origin_interval.start_ns)
            .ok_or_else(|| DeriveError::new("media interval end underflow"))?,
    )
}

fn clip_to_media_extent(
    interval: TimeInterval,
    media_extent: TimeInterval,
) -> DeriveResult<Option<TimeInterval>> {
    if interval.end_ns <= media_extent.start_ns || interval.start_ns >= media_extent.end_ns {
        return Ok(None);
    }
    Ok(Some(TimeInterval::new(
        interval.start_ns.max(media_extent.start_ns),
        interval.end_ns.min(media_extent.end_ns),
    )?))
}

fn frame_intervals(frames: &[FrameRecord]) -> DeriveResult<Vec<FrameInterval>> {
    frames
        .iter()
        .map(|frame| {
            Ok(FrameInterval {
                frame_sequence: frame.sequence,
                start_ns: frame.pts_ns,
                end_ns: frame_end_ns(frame)?,
            })
        })
        .collect()
}

fn frame_window_for_interval(
    frames: &[FrameInterval],
    interval: TimeInterval,
) -> DeriveResult<Value> {
    let mut overlapping = frames
        .iter()
        .filter(|frame| frame.start_ns < interval.end_ns && frame.end_ns > interval.start_ns);
    let Some(first) = overlapping.next().copied() else {
        return Ok(Value::Null);
    };
    let mut last = first;
    for frame in overlapping {
        last = *frame;
    }
    let midpoint = interval.midpoint_ns()?;
    let nominal = frames
        .iter()
        .filter(|frame| frame.start_ns <= midpoint && midpoint < frame.end_ns)
        .map(|frame| frame.frame_sequence)
        .next()
        .unwrap_or(first.frame_sequence);
    Ok(frame_window_json(&FrameWindow {
        start: first.frame_sequence,
        end: last.frame_sequence,
        nominal,
    }))
}

fn frame_window_json(window: &FrameWindow) -> Value {
    json!({
        "start_frame_id": frame_id(window.start),
        "end_frame_id": frame_id(window.end),
        "nominal_frame_id": frame_id(window.nominal),
        "start_frame_sequence": window.start,
        "end_frame_sequence": window.end,
        "nominal_frame_sequence": window.nominal,
        "selection_policy": FRAME_SELECTION_POLICY,
    })
}

fn mapping_input_time_json(input: &EventTimeInput) -> Value {
    json!({
        "clock_domain": input.clock_domain,
        "timestamp_source": TimestampSource::DerivedTransform.as_str(),
        "timestamp_precision": TimestampPrecision::Nanoseconds.as_str(),
        "time_interval_ns": input.interval.to_json(),
        "source_time_status": input.source_time_status,
        "transform_id": input.transform_id,
    })
}

fn video_time_json(
    interval: TimeInterval,
    media_origin_interval: TimeInterval,
    transform_id: &'static str,
    unclipped_interval: &Value,
) -> DeriveResult<Value> {
    Ok(json!({
        "clock_domain": ClockDomain::MediaPtsNs.as_str(),
        "timestamp_source": TimestampSource::DerivedTransform.as_str(),
        "timestamp_precision": TimestampPrecision::Nanoseconds.as_str(),
        "time_interval_ns": interval.to_json(),
        "unclipped_time_interval_ns": unclipped_interval.clone(),
        "transform_id": transform_id,
        "uncertainty_ns": interval.width_ns()?.saturating_add(media_origin_interval.width_ns()?),
    }))
}

fn combined_mapping_status(
    event_status: AlignmentStatus,
    alignment_status: AlignmentStatus,
) -> AlignmentStatus {
    if matches!(
        event_status,
        AlignmentStatus::LegacyWallClockBracketed | AlignmentStatus::Estimated
    ) {
        return event_status;
    }
    if alignment_status == AlignmentStatus::Estimated || event_status == AlignmentStatus::Estimated
    {
        return AlignmentStatus::Estimated;
    }
    AlignmentStatus::Bracketed
}

fn interval_from_json_array(value: &Value) -> DeriveResult<TimeInterval> {
    let Some(values) = value.as_array() else {
        return Err(DeriveError::new("time interval is not an array"));
    };
    if values.len() != 2_usize {
        return Err(DeriveError::new("time interval must have two entries"));
    }
    let Some(start_value) = values.first().and_then(Value::as_i64) else {
        return Err(DeriveError::new("time interval start is missing"));
    };
    let Some(end_value) = values.get(1_usize).and_then(Value::as_i64) else {
        return Err(DeriveError::new("time interval end is missing"));
    };
    TimeInterval::new(start_value, end_value)
}

fn uncertainty_summary(values: &[i64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let min = sorted.first().copied().unwrap_or(0_i64);
    let max = sorted.last().copied().unwrap_or(0_i64);
    let mid_index = sorted.len().checked_div(2_usize).unwrap_or(0_usize);
    let p50 = sorted.get(mid_index).copied().unwrap_or(min);
    json!({
        "min": min,
        "max": max,
        "p50": p50,
    })
}

fn merge_owned(mut left: Vec<String>, right: Vec<String>) -> Vec<String> {
    for value in right {
        if !left.iter().any(|existing| existing == &value) {
            left.push(value);
        }
    }
    left
}

fn frame_id(sequence: u64) -> String {
    format!("frame:{sequence:08}")
}

#[allow(clippy::too_many_lines)]
fn index_json(inputs: &IndexJsonInputs<'_>) -> DeriveResult<Value> {
    let source_refs = source_refs(
        inputs.config,
        inputs.paths,
        inputs.timeline_index,
        inputs.timeline_event_count,
    )?;
    Ok(json!({
        "ok": true,
        "schema": VIDEO_MAP_INDEX_SCHEMA,
        "artifact_stage": ARTIFACT_STAGE_EVENT_FRAME_MAP,
        "cli_version": env!("CARGO_PKG_VERSION"),
        "recording_dir": path_text(&inputs.config.recording_dir),
        "external_run_id": inputs.manifest.get("external_run_id").cloned().unwrap_or(Value::Null),
        "package_name": inputs.manifest.get("package_name").cloned().unwrap_or(Value::Null),
        "session_id": inputs.manifest.get("session_id").cloned().unwrap_or(Value::Null),
        "sources": source_refs,
        "outputs": {
            "video_map_index": {
                "path": relative_path_text(&inputs.config.recording_dir, &inputs.paths.index_output),
                "schema": VIDEO_MAP_INDEX_SCHEMA,
                "record_count": Value::Null,
                "sensitive": true,
                "fingerprint": Value::Null,
                "fingerprint_status": "not_embedded_self_reference",
            },
            "video_map_frames": {
                "path": relative_path_text(&inputs.config.recording_dir, &inputs.paths.frames_output),
                "schema": VIDEO_FRAME_SCHEMA,
                "record_count": inputs.frames.len(),
                "sensitive": true,
                "fingerprint": file_fingerprint(&inputs.paths.frames_output)?,
            },
            "video_map_alignment": {
                "path": relative_path_text(&inputs.config.recording_dir, &inputs.paths.alignment_output),
                "schema": VIDEO_ALIGNMENT_SCHEMA,
                "record_count": 1,
                "sensitive": true,
                "fingerprint": file_fingerprint(&inputs.paths.alignment_output)?,
            },
            "video_map_event_frames": {
                "path": relative_path_text(&inputs.config.recording_dir, &inputs.paths.event_frames_output),
                "schema": EVENT_VIDEO_FRAME_MAP_SCHEMA,
                "record_count": inputs.event_mapping.rows.len(),
                "sensitive": true,
                "fingerprint": file_fingerprint(&inputs.paths.event_frames_output)?,
            },
        },
        "ffprobe": {
            "executable_path": inputs.config.ffprobe.executable_path,
            "version_first_line": inputs.config.ffprobe.version_first_line,
            "args": inputs.config.ffprobe.args,
            "status_code": inputs.config.ffprobe.status_code,
            "stderr": inputs.config.ffprobe.stderr,
            "selected_stream_index": inputs.parsed.stream.index,
        },
        "video_stream": video_stream_json(&inputs.parsed.stream),
        "frame_count": inputs.frames.len(),
        "frame_pts": frame_pts_summary(&inputs.parsed.frames)?,
        "clock_domain": ClockDomain::MediaPtsNs.as_str(),
        "probe_status": "ok",
        "alignment_status": inputs.alignment.status.as_str(),
        "alignment": {
            "schema": VIDEO_ALIGNMENT_SCHEMA,
            "path": relative_path_text(&inputs.config.recording_dir, &inputs.paths.alignment_output),
            "status": inputs.alignment.status.as_str(),
            "transform_id": inputs.alignment.transform_id,
        },
        "event_mapping": {
            "schema": EVENT_VIDEO_FRAME_MAP_SCHEMA,
            "path": relative_path_text(&inputs.config.recording_dir, &inputs.paths.event_frames_output),
            "status": inputs.alignment.status.as_str(),
            "source_event_count": inputs.event_mapping.rows.len(),
            "row_count": inputs.event_mapping.rows.len(),
            "mapped_event_count": inputs.event_mapping.mapped_event_count,
            "unmapped_event_count": inputs.event_mapping.unmapped_event_count,
            "status_counts": inputs.event_mapping.status_counts,
            "uncertainty_summary_ns": inputs.event_mapping.uncertainty_summary_ns,
        },
        "warnings": merge_warning_lists(
            &merge_warning_lists(&inputs.parsed.warnings, &inputs.alignment.warnings),
            &inputs.event_mapping.warnings,
        ),
    }))
}

fn source_refs(
    config: &DeriveVideoMapConfig,
    paths: &VideoMapPaths,
    _timeline_index: &Value,
    timeline_event_count: usize,
) -> DeriveResult<Value> {
    Ok(json!([
        source_ref(
            &config.recording_dir,
            "manifest",
            &config.recording_dir.join("manifest.json")
        )?,
        source_ref(&config.recording_dir, "video_screen", &paths.video_screen)?,
        source_ref(&config.recording_dir, "video_timing", &paths.video_timing)?,
        source_ref(
            &config.recording_dir,
            "timeline_index",
            &paths.timeline_index
        )?,
        source_ref_with_count(
            &config.recording_dir,
            "timeline_events",
            &paths.timeline_events,
            Some(
                u64::try_from(timeline_event_count).map_err(|error| DeriveError::new(format!(
                    "timeline event count overflow: {error}"
                )))?,
            )
        )?,
    ]))
}

fn source_ref(recording_dir: &Path, kind: &str, path: &Path) -> DeriveResult<Value> {
    source_ref_with_count(recording_dir, kind, path, None)
}

fn source_ref_with_count(
    recording_dir: &Path,
    kind: &str,
    path: &Path,
    record_count: Option<u64>,
) -> DeriveResult<Value> {
    let exists = path.exists();
    Ok(json!({
        "kind": kind,
        "path": relative_path_text(recording_dir, path),
        "exists": exists,
        "required": true,
        "record_count": record_count,
        "fingerprint": if exists { file_fingerprint(path)? } else { Value::Null },
    }))
}

fn video_stream_json(stream: &ProbeStream) -> Value {
    json!({
        "index": stream.index,
        "codec_type": stream.codec_type,
        "codec_name": stream.codec_name,
        "width": stream.width,
        "height": stream.height,
        "duration_seconds": stream.duration,
        "nb_frames_reported": stream.nb_frames,
        "avg_frame_rate": stream.avg_frame_rate,
        "r_frame_rate": stream.r_frame_rate,
        "time_base": stream.time_base,
    })
}

fn frame_pts_summary(frames: &[FrameRecord]) -> DeriveResult<Value> {
    let first = frames
        .first()
        .map(|frame| frame.pts_ns)
        .ok_or_else(|| DeriveError::new("frame summary requires at least one frame"))?;
    let last = frames
        .last()
        .map(|frame| frame.pts_ns)
        .ok_or_else(|| DeriveError::new("frame summary requires at least one frame"))?;
    let mut min_duration_ns = None;
    let mut max_duration_ns = None;
    for duration in frames.iter().filter_map(|frame| frame.duration_ns) {
        min_duration_ns =
            Some(min_duration_ns.map_or(duration, |current: i64| current.min(duration)));
        max_duration_ns =
            Some(max_duration_ns.map_or(duration, |current: i64| current.max(duration)));
    }
    Ok(json!({
        "first_pts_ns": first,
        "last_pts_ns": last,
        "min_duration_ns": min_duration_ns,
        "max_duration_ns": max_duration_ns,
    }))
}

fn require_file(path: &Path, description: &str) -> DeriveResult<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(DeriveError::new(format!(
            "missing {description}: {}",
            path.display()
        )))
    }
}

fn read_json_file(path: &Path) -> DeriveResult<Value> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn read_jsonl_records(path: &Path) -> DeriveResult<Vec<TimelineRecord>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for (index, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }
        let line_index = u64::try_from(index)
            .map_err(|error| DeriveError::new(format!("timeline line index overflow: {error}")))?
            .checked_add(1_u64)
            .ok_or_else(|| DeriveError::new("timeline line index overflow"))?;
        records.push(TimelineRecord {
            line_index,
            value: serde_json::from_str(&line)?,
        });
    }
    if records.is_empty() {
        return Err(DeriveError::new("timeline events JSONL is empty"));
    }
    Ok(records)
}

fn validate_timeline_bundle(
    recording_dir: &Path,
    timeline_index: &Value,
    timeline_event_count: usize,
) -> DeriveResult<()> {
    if timeline_index.get("schema").and_then(Value::as_str) != Some(TIMELINE_INDEX_SCHEMA) {
        return Err(DeriveError::new(
            "stale timeline input for video map; run derive timeline: timeline index schema is unsupported",
        ));
    }
    let recorded_event_count =
        timeline_index
            .get("event_count")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                DeriveError::new(
                    "stale timeline input for video map; run derive timeline: timeline index has no event_count",
                )
            })?;
    let actual_event_count = u64::try_from(timeline_event_count)
        .map_err(|error| DeriveError::new(format!("timeline event count overflow: {error}")))?;
    if recorded_event_count != actual_event_count {
        return Err(DeriveError::new(format!(
            "stale timeline input for video map; run derive timeline: timeline event_count changed from {recorded_event_count} to {actual_event_count}",
        )));
    }
    let Some(sources) = timeline_index.get("sources").and_then(Value::as_array) else {
        return Err(DeriveError::new(
            "stale timeline input for video map; run derive timeline: timeline index has no sources array",
        ));
    };
    if sources.is_empty() {
        return Err(DeriveError::new(
            "stale timeline input for video map; run derive timeline: timeline index has no source lineage",
        ));
    }
    let mut has_required_ime_source = false;
    let mut stale_reasons = Vec::new();
    for source in sources {
        if source.get("kind").and_then(Value::as_str) == Some("ime_jsonl")
            && source
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        {
            has_required_ime_source = true;
        }
        if let Some(reason) = timeline_source_stale_reason(recording_dir, source)? {
            stale_reasons.push(reason);
        }
    }
    if !has_required_ime_source {
        stale_reasons.push(String::from(
            "timeline index has no required ime_jsonl source",
        ));
    }
    if stale_reasons.is_empty() {
        Ok(())
    } else {
        Err(DeriveError::new(format!(
            "stale timeline input for video map; run derive timeline: {}",
            stale_reasons.join("; ")
        )))
    }
}

fn timeline_source_stale_reason(
    recording_dir: &Path,
    source: &Value,
) -> DeriveResult<Option<String>> {
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
        return Ok(None);
    }
    let Some(path_text_value) = source.get("path").and_then(Value::as_str) else {
        return Ok(Some(format!("{kind} has no source path")));
    };
    let path = source_path(recording_dir, path_text_value);
    if !path.exists() {
        return Ok(Some(format!("{kind} source is missing: {path_text_value}")));
    }
    let Some(recorded_sha) = source
        .pointer("/fingerprint/sha256")
        .and_then(Value::as_str)
    else {
        return Ok(Some(format!(
            "{kind} source has no fingerprint: {path_text_value}"
        )));
    };
    let current = file_fingerprint(&path)?;
    let current_sha = current.get("sha256").and_then(Value::as_str);
    if Some(recorded_sha) != current_sha {
        return Ok(Some(format!(
            "{kind} source fingerprint changed: {path_text_value}"
        )));
    }
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        && let Some(recorded_count) = source.get("record_count").and_then(Value::as_u64)
    {
        let current_count = count_nonempty_lines(&path)?;
        if recorded_count != current_count {
            return Ok(Some(format!(
                "{kind} source record count changed: {path_text_value}"
            )));
        }
    }
    Ok(None)
}

fn source_path(recording_dir: &Path, path_text_value: &str) -> PathBuf {
    let path = PathBuf::from(path_text_value);
    if path.is_absolute() {
        path
    } else {
        recording_dir.join(path)
    }
}

fn count_nonempty_lines(path: &Path) -> DeriveResult<u64> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut count = 0_u64;
    for line_result in reader.lines() {
        let line = line_result?;
        if !line.trim().is_empty() {
            count = count
                .checked_add(1_u64)
                .ok_or_else(|| DeriveError::new("line count overflow"))?;
        }
    }
    Ok(count)
}

fn required_i64_at(value: &Value, pointer: &str) -> DeriveResult<i64> {
    value
        .pointer(pointer)
        .and_then(Value::as_i64)
        .ok_or_else(|| DeriveError::new(format!("missing integer field: {pointer}")))
}

fn millis_to_nanos_checked(value: i64) -> DeriveResult<i64> {
    value
        .checked_mul(NANOS_PER_MILLI)
        .ok_or_else(|| DeriveError::new("millisecond-to-nanosecond conversion overflow"))
}

fn key_frame_bool(value: Option<&Value>) -> DeriveResult<bool> {
    let Some(json_value) = value else {
        return Err(DeriveError::new("frame is missing key_frame"));
    };
    if let Some(bool_value) = json_value.as_bool() {
        return Ok(bool_value);
    }
    if let Some(number_value) = json_value.as_i64() {
        return Ok(number_value != 0_i64);
    }
    if let Some(text) = json_value.as_str() {
        return match text {
            "0" => Ok(false),
            "1" => Ok(true),
            other => Err(DeriveError::new(format!(
                "unsupported key_frame value: {other}"
            ))),
        };
    }
    Err(DeriveError::new("unsupported key_frame value type"))
}

fn value_to_i64(value: Option<&Value>) -> Option<i64> {
    let json_value = value?;
    if let Some(number_value) = json_value.as_i64() {
        return Some(number_value);
    }
    json_value
        .as_str()
        .and_then(|text| text.parse::<i64>().ok())
}

impl TimeBase {
    fn to_json(self) -> Value {
        json!({
            "numerator": self.numerator,
            "denominator": self.denominator,
            "text": format!("{}/{}", self.numerator, self.denominator),
        })
    }
}

impl RationalRounding {
    fn to_json(self) -> Value {
        json!({
            "policy": self.policy,
            "remainder_num": self.remainder_num,
            "denominator": self.denominator,
            "exact": self.remainder_num == 0_i128,
        })
    }
}

fn parse_time_base(text: &str) -> DeriveResult<TimeBase> {
    let mut parts = text.split('/');
    let Some(numerator_text) = parts.next() else {
        return Err(DeriveError::new("time_base is empty"));
    };
    let Some(denominator_text) = parts.next() else {
        return Err(DeriveError::new(format!(
            "time_base has no denominator: {text}"
        )));
    };
    if parts.next().is_some() {
        return Err(DeriveError::new(format!(
            "time_base has too many parts: {text}"
        )));
    }
    let numerator = parse_positive_i64(numerator_text, "time_base numerator")?;
    let denominator = parse_positive_i64(denominator_text, "time_base denominator")?;
    Ok(TimeBase {
        numerator,
        denominator,
    })
}

fn parse_positive_i64(text: &str, field_name: &str) -> DeriveResult<i64> {
    let value = parse_digit_text(text, field_name)?;
    if value <= 0_i64 {
        return Err(DeriveError::new(format!(
            "{field_name} must be positive: {text}"
        )));
    }
    Ok(value)
}

fn ticks_to_nanos(ticks: i64, time_base: TimeBase) -> DeriveResult<(i64, RationalRounding)> {
    if ticks < 0_i64 {
        return Err(DeriveError::new(format!(
            "media timestamp tick must be non-negative: {ticks}"
        )));
    }
    let numerator = i128::from(ticks)
        .checked_mul(i128::from(time_base.numerator))
        .and_then(|value| value.checked_mul(1_000_000_000_i128))
        .ok_or_else(|| DeriveError::new("media timestamp numerator overflow"))?;
    let denominator = i128::from(time_base.denominator);
    let quotient = numerator
        .checked_div(denominator)
        .ok_or_else(|| DeriveError::new("media timestamp denominator is zero"))?;
    let remainder = numerator
        .checked_rem(denominator)
        .ok_or_else(|| DeriveError::new("media timestamp denominator is zero"))?;
    let doubled_remainder = remainder
        .checked_mul(2_i128)
        .ok_or_else(|| DeriveError::new("media timestamp remainder overflow"))?;
    let rounded = if doubled_remainder >= denominator {
        quotient
            .checked_add(1_i128)
            .ok_or_else(|| DeriveError::new("media timestamp rounding overflow"))?
    } else {
        quotient
    };
    let pts_ns = i64::try_from(rounded)
        .map_err(|error| DeriveError::new(format!("media timestamp overflow: {error}")))?;
    Ok((
        pts_ns,
        RationalRounding {
            policy: "nearest_integer_ns_half_up",
            remainder_num: remainder,
            denominator,
        },
    ))
}

fn parse_seconds_to_nanos(text: &str, field_name: &str) -> DeriveResult<i64> {
    if text.starts_with('-') {
        return Err(DeriveError::new(format!(
            "{field_name} must be non-negative: {text}"
        )));
    }
    let mut parts = text.split('.');
    let seconds_text = parts
        .next()
        .ok_or_else(|| DeriveError::new(format!("{field_name} is empty")))?;
    let fraction_text = parts.next();
    if parts.next().is_some() {
        return Err(DeriveError::new(format!(
            "{field_name} has multiple decimal points: {text}"
        )));
    }
    let seconds = parse_digit_text(seconds_text, field_name)?;
    let mut nanos = seconds
        .checked_mul(1_000_000_000_i64)
        .ok_or_else(|| DeriveError::new(format!("{field_name} nanoseconds overflow")))?;
    if let Some(fraction) = fraction_text {
        let fraction_nanos = parse_fraction_to_nanos(fraction, field_name)?;
        nanos = nanos
            .checked_add(fraction_nanos)
            .ok_or_else(|| DeriveError::new(format!("{field_name} nanoseconds overflow")))?;
    }
    Ok(nanos)
}

fn parse_digit_text(text: &str, field_name: &str) -> DeriveResult<i64> {
    if text.is_empty() {
        return Err(DeriveError::new(format!("{field_name} has empty digits")));
    }
    let mut value = 0_i64;
    for character in text.chars() {
        let Some(digit) = character.to_digit(10) else {
            return Err(DeriveError::new(format!(
                "{field_name} contains a non-digit character: {text}"
            )));
        };
        value = value
            .checked_mul(10_i64)
            .and_then(|next| next.checked_add(i64::from(digit)))
            .ok_or_else(|| DeriveError::new(format!("{field_name} digit overflow")))?;
    }
    Ok(value)
}

fn parse_fraction_to_nanos(text: &str, field_name: &str) -> DeriveResult<i64> {
    if text.is_empty() {
        return Ok(0_i64);
    }
    let mut value = parse_digit_text(text, field_name)?;
    let mut digit_count = text.len();
    if digit_count > 9_usize {
        return Err(DeriveError::new(format!(
            "{field_name} has sub-nanosecond precision: {text}"
        )));
    }
    while digit_count < 9_usize {
        value = value
            .checked_mul(10_i64)
            .ok_or_else(|| DeriveError::new(format!("{field_name} fraction overflow")))?;
        digit_count = digit_count
            .checked_add(1_usize)
            .ok_or_else(|| DeriveError::new("fraction digit count overflow"))?;
    }
    Ok(value)
}

fn push_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn merge_warning_lists(left: &[String], right: &[String]) -> Vec<String> {
    let mut warnings = left.to_vec();
    for warning in right {
        push_once(&mut warnings, warning);
    }
    warnings
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

fn file_fingerprint(path: &Path) -> DeriveResult<Value> {
    let metadata = fs::metadata(path)?;
    Ok(json!({
        "byte_count": metadata.len(),
        "modified_wall_ms": modified_wall_ms(&metadata)?,
        "sha256": format!("{SHA256_PREFIX}{}", sha256_file(path)?),
    }))
}

fn modified_wall_ms(metadata: &fs::Metadata) -> DeriveResult<Option<u64>> {
    let modified_time = metadata.modified()?;
    let modified_duration = match modified_time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration,
        Err(_time_error) => return Ok(None),
    };
    Ok(Some(u64::try_from(modified_duration.as_millis()).map_err(
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

fn relative_path_text(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .map_or_else(|_strip_error| path_text(path), path_text)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use proptest::test_runner::TestCaseError;
    use serde_json::{Value, json};

    use crate::derivation::video_map::{
        DeriveVideoMapConfig, FfprobeInvocation, FrameInterval, TimeInterval, VIDEO_FRAME_SCHEMA,
        derive_video_map, frame_window_for_interval, parse_probe_output, parse_seconds_to_nanos,
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0_u64);

    #[test]
    fn parses_probe_frames_with_integer_media_timestamps() {
        let parsed_result = parse_probe_output(probe_fixture());
        assert!(parsed_result.is_ok(), "probe fixture should parse");
        let Ok(parsed) = parsed_result else {
            return;
        };

        assert_eq!(parsed.frames.len(), 3_usize, "frame count should match");
        assert_eq!(
            parsed.frames.first().map(|frame| frame.pts_ns),
            Some(0_i64),
            "first frame pts should be zero"
        );
        assert_eq!(
            parsed.frames.get(1_usize).map(|frame| frame.pts_ns),
            Some(33_333_333_i64),
            "second frame pts should preserve nanoseconds"
        );
        assert_eq!(
            parsed
                .frames
                .first()
                .and_then(|frame| frame.pts_interval_ns),
            Some(33_333_333_i64),
            "first interval should use next frame pts"
        );
        assert_eq!(
            parsed
                .frames
                .get(2_usize)
                .and_then(|frame| frame.duration_ns),
            Some(33_333_333_i64),
            "duration should be derived from integer ticks and time_base"
        );
        assert_eq!(
            parsed
                .frames
                .first()
                .and_then(|frame| frame.encoded_size_bytes),
            Some(123_i64),
            "encoded packet size should be retained"
        );
    }

    #[test]
    fn rejects_non_monotonic_frame_timestamps() {
        let text = probe_fixture().replace("\"pts\": 6000", "\"pts\": 1000");
        let parsed_result = parse_probe_output(&text);

        assert!(
            parsed_result.is_err(),
            "non-monotonic frame timestamps should fail"
        );
    }

    #[test]
    fn rejects_probe_without_video_stream() {
        let text =
            probe_fixture().replace("\"codec_type\": \"video\"", "\"codec_type\": \"audio\"");
        let parsed_result = parse_probe_output(&text);

        assert!(
            parsed_result.is_err(),
            "probe output without a video stream should fail"
        );
    }

    #[test]
    fn rejects_frames_missing_media_type() {
        let text = probe_fixture().replace("\"media_type\": \"video\",\n", "");
        let parsed_result = parse_probe_output(&text);

        assert!(
            parsed_result.is_err(),
            "video frame rows without media_type should fail"
        );
    }

    #[test]
    fn rejects_subnanosecond_timestamp_precision() {
        let parse_result = parse_seconds_to_nanos("0.1234567891", "test field");

        assert!(
            parse_result.is_err(),
            "sub-nanosecond decimal precision should be rejected"
        );
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    fn derives_video_map_files_from_probe_json() {
        let root = temp_recording_dir();
        let setup_result = create_recording_fixture(&root);
        assert!(setup_result.is_ok(), "fixture setup should succeed");
        let config = DeriveVideoMapConfig {
            recording_dir: root.clone(),
            output_dir: None,
            ffprobe_json: probe_fixture().to_owned(),
            ffprobe: FfprobeInvocation {
                executable_path: "ffprobe".to_owned(),
                version_first_line: "ffprobe version test".to_owned(),
                args: vec!["-show_frames".to_owned()],
                status_code: Some(0_i32),
                stderr: String::new(),
            },
        };

        let summary_result = derive_video_map(&config);
        assert!(summary_result.is_ok(), "derive should succeed");
        let Ok(summary) = summary_result else {
            cleanup_recording_dir(&root);
            return;
        };
        assert_eq!(
            summary.get("frame_count").and_then(Value::as_u64),
            Some(3_u64),
            "summary should report parsed frames"
        );
        let index = read_json(&root.join("derived").join("video_map").join("index.json"));
        let frames =
            fs::read_to_string(root.join("derived").join("video_map").join("frames.jsonl"));
        let event_frames = fs::read_to_string(
            root.join("derived")
                .join("video_map")
                .join("event_frames.jsonl"),
        );
        let alignment = read_json(
            &root
                .join("derived")
                .join("video_map")
                .join("alignment.json"),
        );
        assert!(index.is_ok(), "index should be readable");
        assert!(frames.is_ok(), "frames JSONL should be readable");
        assert!(
            event_frames.is_ok(),
            "event frames JSONL should be readable"
        );
        assert!(alignment.is_ok(), "alignment JSON should be readable");
        let Ok(index_value) = index else {
            cleanup_recording_dir(&root);
            return;
        };
        assert_eq!(
            index_value.get("artifact_stage").and_then(Value::as_str),
            Some("event_frame_map"),
            "index should declare event-frame-map stage"
        );
        assert_eq!(
            index_value
                .pointer("/event_mapping/status")
                .and_then(Value::as_str),
            Some("bracketed"),
            "event mapping should use canonical timing fixture"
        );
        let Ok(frame_text) = frames else {
            cleanup_recording_dir(&root);
            return;
        };
        let first_line = frame_text.lines().next();
        assert!(
            first_line.is_some(),
            "frames JSONL should contain a first line"
        );
        let Some(first_line_text) = first_line else {
            cleanup_recording_dir(&root);
            return;
        };
        let first_frame: Result<Value, serde_json::Error> = serde_json::from_str(first_line_text);
        assert!(first_frame.is_ok(), "first frame should parse");
        let Ok(first_frame_value) = first_frame else {
            cleanup_recording_dir(&root);
            return;
        };
        assert_eq!(
            first_frame_value.get("schema").and_then(Value::as_str),
            Some(VIDEO_FRAME_SCHEMA),
            "frame rows should use the video-frame schema"
        );
        assert_eq!(
            first_frame_value
                .pointer("/media_time/pts_ns")
                .and_then(Value::as_i64),
            Some(0_i64),
            "frame row should carry integer media PTS"
        );
        let Ok(event_frame_text) = event_frames else {
            cleanup_recording_dir(&root);
            return;
        };
        let event_rows: Vec<Value> = event_frame_text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(event_rows.len(), 2_usize, "one row per timeline event");
        assert_eq!(
            event_rows
                .first()
                .and_then(|row| row.get("mapping_status"))
                .and_then(Value::as_str),
            Some("bracketed"),
            "canonical event should map"
        );
        assert!(
            event_rows
                .first()
                .and_then(|row| row.get("frame_window"))
                .is_some_and(|value| !value.is_null()),
            "canonical event should include a frame window"
        );
        assert_eq!(
            event_rows
                .get(1_usize)
                .and_then(|row| row.get("mapping_status"))
                .and_then(Value::as_str),
            Some("unsupported_clock_domain"),
            "raw touch event should stay unsupported"
        );
        cleanup_recording_dir(&root);
    }

    #[test]
    fn maps_outside_video_events_as_outside_range() {
        let root = temp_recording_dir();
        let setup_result = create_recording_fixture(&root);
        assert!(setup_result.is_ok(), "fixture setup should succeed");
        let write_result = fs::write(
            root.join("derived").join("timeline").join("events.jsonl"),
            "{\"schema\":\"input_dynamics_timeline_event.v1\",\"timeline_event_id\":\"timeline:000001\",\"event\":\"key_down\",\"record_kind\":\"ime_event\",\"clock_domain\":\"android_uptime_ms\",\"source_time\":{\"source_clock_domain\":\"android_uptime_ms\",\"source_time_status\":\"canonical_event_time_metadata\",\"source_time_ms\":500}}\n",
        );
        assert!(write_result.is_ok(), "timeline override should succeed");
        let index_result = write_timeline_index(&root, 1_u64);
        assert!(
            index_result.is_ok(),
            "timeline index override should succeed"
        );
        let config = video_map_test_config(&root);

        let derive_result = derive_video_map(&config);
        assert!(derive_result.is_ok(), "derive should succeed");
        let rows_result = fs::read_to_string(
            root.join("derived")
                .join("video_map")
                .join("event_frames.jsonl"),
        );
        assert!(rows_result.is_ok(), "event frames should be readable");
        let Ok(rows_text) = rows_result else {
            cleanup_recording_dir(&root);
            return;
        };
        let row: Result<Value, serde_json::Error> = serde_json::from_str(
            rows_text
                .lines()
                .next()
                .unwrap_or("{\"mapping_status\":\"missing\"}"),
        );
        assert!(row.is_ok(), "event row should parse");
        let Ok(row_value) = row else {
            cleanup_recording_dir(&root);
            return;
        };
        assert_eq!(
            row_value.get("mapping_status").and_then(Value::as_str),
            Some("outside_range"),
            "outside event should not map to frames"
        );
        assert!(
            row_value.get("frame_window").is_some_and(Value::is_null),
            "outside event should have no frame window"
        );
        assert!(
            row_value
                .get("reasons")
                .and_then(Value::as_array)
                .is_some_and(|reasons| reasons
                    .iter()
                    .any(|reason| reason.as_str() == Some("outside_video_pts_extent"))),
            "outside event should explain the range failure"
        );
        cleanup_recording_dir(&root);
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    fn maps_legacy_wall_clock_events_as_legacy() {
        let root = temp_recording_dir();
        let setup_result = create_recording_fixture(&root);
        assert!(setup_result.is_ok(), "fixture setup should succeed");
        let timing_result = fs::write(
            root.join("video").join("timing.json"),
            r#"{
                "schema":"input_dynamics_video_capture.v1",
                "start":{
                    "before":{"device_wall_ms":10000},
                    "after":{"device_wall_ms":10000}
                },
                "stop":{
                    "before":{"device_wall_ms":10100},
                    "after":{"device_wall_ms":10100}
                }
            }"#,
        );
        assert!(
            timing_result.is_ok(),
            "legacy timing override should succeed"
        );
        let write_result = fs::write(
            root.join("derived").join("timeline").join("events.jsonl"),
            "{\"schema\":\"input_dynamics_timeline_event.v1\",\"timeline_event_id\":\"timeline:000001\",\"event\":\"field_enter\",\"record_kind\":\"ime_event\",\"clock_domain\":\"android_uptime_ms\",\"t_wall_ms\":10050}\n",
        );
        assert!(
            write_result.is_ok(),
            "legacy timeline override should succeed"
        );
        let timeline_index_result = write_timeline_index(&root, 1_u64);
        assert!(
            timeline_index_result.is_ok(),
            "timeline index override should succeed"
        );
        let config = video_map_test_config(&root);

        let derive_result = derive_video_map(&config);
        assert!(derive_result.is_ok(), "legacy derive should succeed");
        let map_index_result =
            read_json(&root.join("derived").join("video_map").join("index.json"));
        assert!(map_index_result.is_ok(), "index should be readable");
        let Ok(index) = map_index_result else {
            cleanup_recording_dir(&root);
            return;
        };
        assert_eq!(
            index.get("alignment_status").and_then(Value::as_str),
            Some("legacy_wall_clock_bracketed"),
            "legacy timing should be labeled"
        );
        let rows_result = fs::read_to_string(
            root.join("derived")
                .join("video_map")
                .join("event_frames.jsonl"),
        );
        assert!(rows_result.is_ok(), "event frames should be readable");
        let Ok(rows_text) = rows_result else {
            cleanup_recording_dir(&root);
            return;
        };
        let row: Result<Value, serde_json::Error> = serde_json::from_str(
            rows_text
                .lines()
                .next()
                .unwrap_or("{\"mapping_status\":\"missing\"}"),
        );
        assert!(row.is_ok(), "event row should parse");
        let Ok(row_value) = row else {
            cleanup_recording_dir(&root);
            return;
        };
        assert_eq!(
            row_value.get("mapping_status").and_then(Value::as_str),
            Some("legacy_wall_clock_bracketed"),
            "legacy event should keep legacy mapping status"
        );
        assert_eq!(
            row_value
                .pointer("/video_time/transform_id")
                .and_then(Value::as_str),
            Some(super::LEGACY_DEVICE_WALL_TO_MEDIA_TRANSFORM_ID),
            "legacy event should not claim the canonical elapsed-realtime transform"
        );
        assert!(
            row_value
                .get("frame_window")
                .is_some_and(|value| !value.is_null()),
            "legacy event should still map to a frame window when timing permits"
        );
        cleanup_recording_dir(&root);
    }

    #[test]
    fn does_not_map_host_captured_wall_time_as_device_wall_time() {
        let root = temp_recording_dir();
        let setup_result = create_recording_fixture(&root);
        assert!(setup_result.is_ok(), "fixture setup should succeed");
        let timing_result = fs::write(
            root.join("video").join("timing.json"),
            r#"{
                "schema":"input_dynamics_video_capture.v1",
                "start":{
                    "before":{"device_wall_ms":10000},
                    "after":{"device_wall_ms":10000}
                },
                "stop":{
                    "before":{"device_wall_ms":10100},
                    "after":{"device_wall_ms":10100}
                }
            }"#,
        );
        assert!(
            timing_result.is_ok(),
            "legacy timing override should succeed"
        );
        let write_result = fs::write(
            root.join("derived").join("timeline").join("events.jsonl"),
            "{\"schema\":\"input_dynamics_timeline_event.v1\",\"timeline_event_id\":\"timeline:000001\",\"event\":\"touch_gesture\",\"record_kind\":\"touch_gesture\",\"clock_domain\":\"host_wall_ms\",\"captured_wall_ms\":10050}\n",
        );
        assert!(
            write_result.is_ok(),
            "captured-wall timeline override should succeed"
        );
        let index_result = write_timeline_index(&root, 1_u64);
        assert!(
            index_result.is_ok(),
            "timeline index override should succeed"
        );
        let config = video_map_test_config(&root);

        let derive_result = derive_video_map(&config);
        assert!(derive_result.is_ok(), "derive should succeed");
        let row_value = first_event_frame_row(&root);
        assert_eq!(
            row_value.get("mapping_status").and_then(Value::as_str),
            Some("unsupported_clock_domain"),
            "host captured wall time must not map through the device-wall transform"
        );
        cleanup_recording_dir(&root);
    }

    #[test]
    fn unusable_normalized_time_status_stays_unsupported() {
        let root = temp_recording_dir();
        let setup_result = create_recording_fixture(&root);
        assert!(setup_result.is_ok(), "fixture setup should succeed");
        let write_result = fs::write(
            root.join("derived").join("timeline").join("events.jsonl"),
            "{\"schema\":\"input_dynamics_timeline_event.v1\",\"timeline_event_id\":\"timeline:000001\",\"event\":\"key_down\",\"record_kind\":\"ime_event\",\"normalized_time\":{\"status\":\"unsupported_clock_domain\",\"clock_domain\":\"device_elapsed_realtime_ns\",\"time_ns\":1050000000}}\n",
        );
        assert!(
            write_result.is_ok(),
            "normalized timeline override should succeed"
        );
        let index_result = write_timeline_index(&root, 1_u64);
        assert!(
            index_result.is_ok(),
            "timeline index override should succeed"
        );
        let config = video_map_test_config(&root);

        let derive_result = derive_video_map(&config);
        assert!(derive_result.is_ok(), "derive should succeed");
        let row_value = first_event_frame_row(&root);
        assert_eq!(
            row_value.get("mapping_status").and_then(Value::as_str),
            Some("unsupported_clock_domain"),
            "unusable normalized statuses must not become bracketed mappings"
        );
        cleanup_recording_dir(&root);
    }

    #[test]
    fn stale_timeline_event_count_fails_before_mapping() {
        let root = temp_recording_dir();
        let setup_result = create_recording_fixture(&root);
        assert!(setup_result.is_ok(), "fixture setup should succeed");
        let write_result = fs::write(
            root.join("derived").join("timeline").join("events.jsonl"),
            "{\"schema\":\"input_dynamics_timeline_event.v1\",\"timeline_event_id\":\"timeline:000001\",\"event\":\"key_down\"}\n",
        );
        assert!(write_result.is_ok(), "timeline override should succeed");
        let config = video_map_test_config(&root);

        let derive_result = derive_video_map(&config);
        assert!(
            derive_result
                .err()
                .is_some_and(|error| error.to_string().contains("timeline event_count changed")),
            "stale timeline count should fail with remediation"
        );
        cleanup_recording_dir(&root);
    }

    proptest::proptest! {
        #[test]
        fn widened_event_interval_does_not_shrink_frame_window(
            start in 0_i64..900_i64,
            width in 1_i64..100_i64,
            expand_before in 0_i64..100_i64,
            expand_after in 0_i64..100_i64,
        ) {
            let frames = synthetic_frame_intervals();
            let inner_start = start;
            let inner_end = start.saturating_add(width);
            let outer_start = inner_start.saturating_sub(expand_before);
            let outer_end = inner_end.saturating_add(expand_after).min(1_000_i64);
            let inner = time_interval_for_prop(inner_start, inner_end)?;
            let outer = time_interval_for_prop(outer_start, outer_end)?;
            let inner_window = frame_window_for_interval(&frames, inner)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let outer_window = frame_window_for_interval(&frames, outer)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let inner_start_frame = frame_sequence_pointer(&inner_window, "/start_frame_sequence")?;
            let inner_end_frame = frame_sequence_pointer(&inner_window, "/end_frame_sequence")?;
            let outer_start_frame = frame_sequence_pointer(&outer_window, "/start_frame_sequence")?;
            let outer_end_frame = frame_sequence_pointer(&outer_window, "/end_frame_sequence")?;

            proptest::prop_assert!(
                outer_start_frame <= inner_start_frame,
                "widened interval should not move start frame later"
            );
            proptest::prop_assert!(
                outer_end_frame >= inner_end_frame,
                "widened interval should not move end frame earlier"
            );
        }
    }

    fn probe_fixture() -> &'static str {
        r#"{
            "streams": [
                {
                    "index": 0,
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 1080,
                    "height": 2400,
                    "duration": "0.100000000",
                    "nb_frames": "3",
                    "avg_frame_rate": "30/1",
                    "r_frame_rate": "30/1",
                    "time_base": "1/90000"
                }
            ],
            "frames": [
                {
                    "media_type": "video",
                    "key_frame": 1,
                    "pts": 0,
                    "pts_time": "0.000000000",
                    "duration": 3000,
                    "duration_time": "0.033333333",
                    "pkt_size": "123",
                    "width": 1080,
                    "height": 2400,
                    "pict_type": "I"
                },
                {
                    "media_type": "video",
                    "key_frame": 0,
                    "pts": 3000,
                    "pts_time": "0.033333333",
                    "duration": 3000,
                    "duration_time": "0.033333333",
                    "pkt_size": "456",
                    "width": 1080,
                    "height": 2400,
                    "pict_type": "P"
                },
                {
                    "media_type": "video",
                    "key_frame": 0,
                    "pts": 6000,
                    "pts_time": "0.066666667",
                    "duration": 3000,
                    "duration_time": "0.033333334",
                    "pkt_size": "789",
                    "width": 1080,
                    "height": 2400,
                    "pict_type": "P"
                }
            ]
        }"#
    }

    fn temp_recording_dir() -> PathBuf {
        let counter = TEMP_COUNTER.fetch_add(1_u64, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "input-dynamics-video-map-test-{}-{counter}",
            std::process::id()
        ))
    }

    fn create_recording_fixture(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(root.join("ime"))?;
        fs::create_dir_all(root.join("video"))?;
        fs::create_dir_all(root.join("derived").join("timeline"))?;
        let ime_path = root.join("ime").join("session-test.jsonl");
        fs::write(
            &ime_path,
            "{\"schema\":\"input_dynamics_event.v1\",\"event\":\"session_start\"}\n",
        )?;
        fs::write(root.join("video").join("screen.mp4"), b"synthetic-video")?;
        fs::write(
            root.join("video").join("timing.json"),
            r#"{
                "schema":"input_dynamics_video_capture.v1",
                "start":{
                    "before":{"t_elapsed_realtime_ns":1000000000,"t_uptime_ns":100000000,"device_wall_ms":10000},
                    "after":{"t_elapsed_realtime_ns":1000000000,"t_uptime_ns":100000000,"device_wall_ms":10000}
                },
                "stop":{
                    "before":{"t_elapsed_realtime_ns":1100000000,"t_uptime_ns":200000000,"device_wall_ms":10100},
                    "after":{"t_elapsed_realtime_ns":1100000000,"t_uptime_ns":200000000,"device_wall_ms":10100}
                }
            }"#,
        )?;
        fs::write(
            root.join("manifest.json"),
            r#"{"schema":"input_dynamics_record_manifest.v1","external_run_id":"run-test","package_name":"org.inputdynamics.ime.debug","session_id":"session-test"}"#,
        )?;
        write_timeline_index(root, 2_u64)?;
        fs::write(
            root.join("derived").join("timeline").join("events.jsonl"),
            concat!(
                "{\"schema\":\"input_dynamics_timeline_event.v1\",\"timeline_event_id\":\"timeline:000001\",\"event\":\"key_down\",\"record_kind\":\"ime_event\",\"clock_domain\":\"android_uptime_ms\",\"source_ref\":{\"path\":\"ime/session.jsonl\",\"line_index\":1},\"source_time\":{\"source_clock_domain\":\"android_uptime_ms\",\"source_time_status\":\"canonical_event_time_metadata\",\"source_time_ms\":150}}\n",
                "{\"schema\":\"input_dynamics_timeline_event.v1\",\"timeline_event_id\":\"timeline:000002\",\"event\":\"touch_gesture\",\"record_kind\":\"touch_gesture\",\"clock_domain\":\"kernel_getevent_us\",\"source_ref\":{\"path\":\"derived/touch_gestures.jsonl\",\"line_index\":1}}\n",
            ),
        )?;
        Ok(())
    }

    fn write_timeline_index(
        root: &Path,
        event_count: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ime_path = root.join("ime").join("session-test.jsonl");
        fs::write(
            root.join("derived").join("timeline").join("index.json"),
            serde_json::to_string(&json!({
                "schema": "input_dynamics_timeline_index.v1",
                "event_count": event_count,
                "sources": [
                    {
                        "kind": "ime_jsonl",
                        "path": "ime/session-test.jsonl",
                        "exists": true,
                        "required": true,
                        "record_count": 1_u64,
                        "fingerprint": super::file_fingerprint(&ime_path)?,
                    }
                ]
            }))?,
        )?;
        Ok(())
    }

    fn video_map_test_config(root: &Path) -> DeriveVideoMapConfig {
        DeriveVideoMapConfig {
            recording_dir: root.to_path_buf(),
            output_dir: None,
            ffprobe_json: probe_fixture().to_owned(),
            ffprobe: FfprobeInvocation {
                executable_path: "ffprobe".to_owned(),
                version_first_line: "ffprobe version test".to_owned(),
                args: vec!["-show_frames".to_owned()],
                status_code: Some(0_i32),
                stderr: String::new(),
            },
        }
    }

    fn read_json(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    fn first_event_frame_row(root: &Path) -> Value {
        let text = fs::read_to_string(
            root.join("derived")
                .join("video_map")
                .join("event_frames.jsonl"),
        )
        .unwrap_or_default();
        text.lines()
            .next()
            .and_then(|line| serde_json::from_str(line).ok())
            .unwrap_or_else(|| json!({"mapping_status": "missing"}))
    }

    fn cleanup_recording_dir(root: &Path) {
        let _ignored = fs::remove_dir_all(root);
    }

    fn synthetic_frame_intervals() -> Vec<FrameInterval> {
        vec![
            FrameInterval {
                frame_sequence: 1,
                start_ns: 0,
                end_ns: 100,
            },
            FrameInterval {
                frame_sequence: 2,
                start_ns: 100,
                end_ns: 250,
            },
            FrameInterval {
                frame_sequence: 3,
                start_ns: 250,
                end_ns: 600,
            },
            FrameInterval {
                frame_sequence: 4,
                start_ns: 600,
                end_ns: 1_000,
            },
        ]
    }

    fn time_interval_for_prop(start_ns: i64, end_ns: i64) -> Result<TimeInterval, TestCaseError> {
        TimeInterval::new(start_ns, end_ns).map_err(|error| TestCaseError::fail(error.to_string()))
    }

    fn frame_sequence_pointer(value: &Value, pointer: &str) -> Result<u64, TestCaseError> {
        value
            .pointer(pointer)
            .and_then(Value::as_u64)
            .ok_or_else(|| TestCaseError::fail(format!("missing frame sequence: {pointer}")))
    }
}
