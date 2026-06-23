//! Encoded video frame-index derivation.

use std::fmt::Write as FmtWrite;
use std::fs::{self, File};
use std::io::{BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::clock::{AlignmentStatus, ClockDomain, TimestampPrecision, TimestampSource};
use crate::derivation::jsonl::write_jsonl;
use crate::derivation::{
    DERIVATION_SUMMARY_SCHEMA, DeriveError, DeriveResult, VIDEO_FRAME_SCHEMA,
    VIDEO_MAP_INDEX_SCHEMA, path_text, read_manifest,
};

const SHA256_PREFIX: &str = "sha256:";
const ARTIFACT_STAGE_FRAME_INDEX: &str = "frame_index";
const EVENT_MAPPING_STATUS: &str = AlignmentStatus::NotEstimated.as_str();

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
    output_dir: PathBuf,
    index_output: PathBuf,
    frames_output: PathBuf,
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
            index_output: output_dir.join("index.json"),
            frames_output: output_dir.join("frames.jsonl"),
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
    let parsed = parse_probe_output(&config.ffprobe_json)?;
    fs::create_dir_all(&paths.output_dir)?;

    let source_video = source_ref(&config.recording_dir, "video_screen", &paths.video_screen)?;
    let frame_values = frame_json_values(&parsed.frames, &source_video);
    write_jsonl(&paths.frames_output, &frame_values)?;
    let index = index_json(config, &paths, &manifest, &parsed, &frame_values)?;
    write_json_file(&paths.index_output, &index)?;

    Ok(json!({
        "ok": true,
        "schema": DERIVATION_SUMMARY_SCHEMA,
        "derivation": "video_map",
        "artifact_stage": ARTIFACT_STAGE_FRAME_INDEX,
        "recording_dir": path_text(&config.recording_dir),
        "output_dir": path_text(&paths.output_dir),
        "index_output": path_text(&paths.index_output),
        "frames_output": path_text(&paths.frames_output),
        "frame_count": frame_values.len(),
        "probe_status": "ok",
        "alignment_status": AlignmentStatus::NotEstimated.as_str(),
        "event_mapping": {
            "status": EVENT_MAPPING_STATUS,
            "mapped_event_count": Value::Null,
            "unmapped_event_count": Value::Null,
        },
        "warnings": parsed.warnings,
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

fn index_json(
    config: &DeriveVideoMapConfig,
    paths: &VideoMapPaths,
    manifest: &Value,
    parsed: &ParsedVideoMap,
    frames: &[Value],
) -> DeriveResult<Value> {
    let source_refs = source_refs(config, paths)?;
    Ok(json!({
        "ok": true,
        "schema": VIDEO_MAP_INDEX_SCHEMA,
        "artifact_stage": ARTIFACT_STAGE_FRAME_INDEX,
        "cli_version": env!("CARGO_PKG_VERSION"),
        "recording_dir": path_text(&config.recording_dir),
        "external_run_id": manifest.get("external_run_id").cloned().unwrap_or(Value::Null),
        "package_name": manifest.get("package_name").cloned().unwrap_or(Value::Null),
        "session_id": manifest.get("session_id").cloned().unwrap_or(Value::Null),
        "sources": source_refs,
        "outputs": {
            "video_map_index": {
                "path": relative_path_text(&config.recording_dir, &paths.index_output),
                "schema": VIDEO_MAP_INDEX_SCHEMA,
                "record_count": Value::Null,
                "sensitive": true,
                "fingerprint": Value::Null,
                "fingerprint_status": "not_embedded_self_reference",
            },
            "video_map_frames": {
                "path": relative_path_text(&config.recording_dir, &paths.frames_output),
                "schema": VIDEO_FRAME_SCHEMA,
                "record_count": frames.len(),
                "sensitive": true,
                "fingerprint": file_fingerprint(&paths.frames_output)?,
            },
        },
        "ffprobe": {
            "executable_path": config.ffprobe.executable_path,
            "version_first_line": config.ffprobe.version_first_line,
            "args": config.ffprobe.args,
            "status_code": config.ffprobe.status_code,
            "stderr": config.ffprobe.stderr,
            "selected_stream_index": parsed.stream.index,
        },
        "video_stream": video_stream_json(&parsed.stream),
        "frame_count": frames.len(),
        "frame_pts": frame_pts_summary(&parsed.frames)?,
        "clock_domain": ClockDomain::MediaPtsNs.as_str(),
        "probe_status": "ok",
        "alignment_status": AlignmentStatus::NotEstimated.as_str(),
        "event_mapping": {
            "status": EVENT_MAPPING_STATUS,
            "mapped_event_count": Value::Null,
            "unmapped_event_count": Value::Null,
        },
        "warnings": parsed.warnings,
    }))
}

fn source_refs(config: &DeriveVideoMapConfig, paths: &VideoMapPaths) -> DeriveResult<Value> {
    Ok(json!([
        source_ref(
            &config.recording_dir,
            "manifest",
            &config.recording_dir.join("manifest.json")
        )?,
        source_ref(&config.recording_dir, "video_screen", &paths.video_screen)?,
        source_ref(&config.recording_dir, "video_timing", &paths.video_timing)?,
    ]))
}

fn source_ref(recording_dir: &Path, kind: &str, path: &Path) -> DeriveResult<Value> {
    let exists = path.exists();
    Ok(json!({
        "kind": kind,
        "path": relative_path_text(recording_dir, path),
        "exists": exists,
        "required": true,
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

    use serde_json::Value;

    use crate::derivation::video_map::{
        DeriveVideoMapConfig, FfprobeInvocation, VIDEO_FRAME_SCHEMA, derive_video_map,
        parse_probe_output, parse_seconds_to_nanos,
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
        assert!(index.is_ok(), "index should be readable");
        assert!(frames.is_ok(), "frames JSONL should be readable");
        let Ok(index_value) = index else {
            cleanup_recording_dir(&root);
            return;
        };
        assert_eq!(
            index_value.get("artifact_stage").and_then(Value::as_str),
            Some("frame_index"),
            "index should declare frame-index stage"
        );
        assert_eq!(
            index_value
                .pointer("/event_mapping/status")
                .and_then(Value::as_str),
            Some("not_estimated"),
            "event mapping should not be implied by frame indexing"
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
        cleanup_recording_dir(&root);
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
        fs::create_dir_all(root.join("video"))?;
        fs::create_dir_all(root.join("derived").join("timeline"))?;
        fs::write(root.join("video").join("screen.mp4"), b"synthetic-video")?;
        fs::write(
            root.join("video").join("timing.json"),
            r#"{"schema":"input_dynamics_video_capture.v1"}"#,
        )?;
        fs::write(
            root.join("manifest.json"),
            r#"{"schema":"input_dynamics_record_manifest.v1","external_run_id":"run-test","package_name":"org.inputdynamics.ime.debug","session_id":"session-test"}"#,
        )?;
        fs::write(
            root.join("derived").join("timeline").join("index.json"),
            r#"{"schema":"input_dynamics_timeline_index.v1","sources":[]}"#,
        )?;
        fs::write(
            root.join("derived").join("timeline").join("events.jsonl"),
            "{\"schema\":\"input_dynamics_timeline_event.v1\"}\n",
        )?;
        Ok(())
    }

    fn read_json(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    fn cleanup_recording_dir(root: &Path) {
        let _ignored = fs::remove_dir_all(root);
    }
}
