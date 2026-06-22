//! Run-level summary derivation from per-press summaries.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File};
use std::io::{BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::derivation::jsonl::read_jsonl;
use crate::derivation::{
    DERIVATION_SUMMARY_SCHEMA, DeriveError, DeriveResult, RUN_SUMMARY_SCHEMA, path_text,
    read_manifest, string_at, string_field,
};

const SHA256_PREFIX: &str = "sha256:";
const CLOCK_ALIGNMENT_NOT_ESTIMATED: &str = "not_estimated";

/// Configuration for deriving a run-level summary from press summaries.
#[derive(Clone, Debug)]
pub struct DeriveRunSummaryConfig {
    /// Recording directory created by `input-dynamics record`.
    pub recording_dir: PathBuf,
    /// Derived press summary JSONL path. Defaults under `recording_dir`.
    pub press_summaries_jsonl: Option<PathBuf>,
    /// Output path for the run summary JSON. Defaults under `recording_dir`.
    pub output: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct SummaryPaths {
    press_summaries_jsonl: PathBuf,
    output: PathBuf,
}

#[derive(Default)]
struct RunSummaryBuilder {
    first_external_run_id: Option<String>,
    first_package_name: Option<String>,
    first_session_id: Option<String>,
    counts: Counts,
    timing: TimingStats,
    pointer: PointerStats,
    spatial: SpatialStats,
    target_packages: BTreeMap<String, u64>,
    password_record_count: u64,
    clock_alignment_status: Option<String>,
}

#[derive(Default)]
struct Counts {
    presses: u64,
    commits: u64,
    letters: u64,
    spaces: u64,
    enters: u64,
    deletes: u64,
    repeats: u64,
    long_presses: u64,
    cancels: u64,
    with_pointer_samples: u64,
    with_key_down: u64,
    with_key_up: u64,
    with_key_commit: u64,
}

#[derive(Default)]
struct TimingStats {
    hold_ms: I64Stats,
    flight_since_previous_commit_ms: I64Stats,
    down_to_commit_ms: I64Stats,
    pointer_duration_ms: I64Stats,
    pause_buckets: PauseBuckets,
}

#[derive(Default)]
struct PauseBuckets {
    negative_ms: u64,
    under_100_ms: u64,
    ms_100_to_250: u64,
    ms_250_to_1000: u64,
    ms_1000_or_more: u64,
}

#[derive(Default)]
struct PointerStats {
    sample_count: I64Stats,
    current_sample_count: I64Stats,
    historical_sample_count: I64Stats,
    path_length_px: I64Stats,
    max_distance_from_start_px: I64Stats,
    pressure: F64Stats,
    size: F64Stats,
    touch_major_px: I64Stats,
    touch_minor_px: I64Stats,
}

#[derive(Default)]
struct SpatialStats {
    center_offset_x_px: I64Stats,
    center_offset_y_px: I64Stats,
    touch_x_ratio: F64Stats,
    touch_y_ratio: F64Stats,
}

#[derive(Default)]
struct I64Stats {
    count: u64,
    min: Option<i64>,
    max: Option<i64>,
    sum: i64,
}

#[derive(Default)]
struct F64Stats {
    count: u64,
    first: Option<f64>,
    last: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
}

impl SummaryPaths {
    fn from_config(config: &DeriveRunSummaryConfig) -> Self {
        let derived_dir = config.recording_dir.join("derived");
        let press_summaries_jsonl = config
            .press_summaries_jsonl
            .clone()
            .unwrap_or_else(|| derived_dir.join("press_summaries.jsonl"));
        let output = config
            .output
            .clone()
            .unwrap_or_else(|| derived_dir.join("run_summary.json"));
        Self {
            press_summaries_jsonl,
            output,
        }
    }
}

/// Derive a run-level summary from `derived/press_summaries.jsonl`.
pub fn derive_run_summary(config: &DeriveRunSummaryConfig) -> DeriveResult<Value> {
    let paths = SummaryPaths::from_config(config);
    let manifest = read_manifest(&config.recording_dir)?;
    let press_records = read_jsonl(&paths.press_summaries_jsonl)?;
    let summary = run_summary_json(config, &paths, &manifest, &press_records)?;
    write_json_file(&paths.output, &summary)?;
    Ok(json!({
        "ok": true,
        "schema": DERIVATION_SUMMARY_SCHEMA,
        "derivation": "run_summary",
        "recording_dir": path_text(&config.recording_dir),
        "press_summaries_jsonl": path_text(&paths.press_summaries_jsonl),
        "output": path_text(&paths.output),
        "press_summary_count": press_records.len(),
    }))
}

fn run_summary_json(
    config: &DeriveRunSummaryConfig,
    paths: &SummaryPaths,
    manifest: &Value,
    press_records: &[Value],
) -> DeriveResult<Value> {
    let mut builder = RunSummaryBuilder::default();
    for record in press_records {
        builder.ingest(record)?;
    }
    let source_record_count = u64::try_from(press_records.len())
        .map_err(|error| DeriveError::new(format!("record count overflow: {error}")))?;
    let external_run_id =
        string_at(manifest, "/external_run_id").or_else(|| builder.first_external_run_id.clone());
    let package_name =
        string_at(manifest, "/package_name").or_else(|| builder.first_package_name.clone());
    let session_id =
        string_at(manifest, "/session_id").or_else(|| builder.first_session_id.clone());
    let clock_alignment_status = builder
        .clock_alignment_status
        .clone()
        .unwrap_or_else(|| String::from(CLOCK_ALIGNMENT_NOT_ESTIMATED));
    let target_packages = builder.target_packages.clone();
    Ok(json!({
        "ok": true,
        "schema": RUN_SUMMARY_SCHEMA,
        "event": "run_summary",
        "external_run_id": external_run_id,
        "package_name": package_name,
        "session_id": session_id,
        "recording_dir": path_text(&config.recording_dir),
        "source": "derived_press_summaries",
        "source_ref": {
            "path": relative_path_text(&config.recording_dir, &paths.press_summaries_jsonl),
            "record_count": source_record_count,
            "fingerprint": file_fingerprint(&paths.press_summaries_jsonl)?,
        },
        "counts": builder.counts.to_json(),
        "timing": builder.timing.to_json(),
        "pointer": builder.pointer.to_json(),
        "spatial": builder.spatial.to_json(),
        "target_packages": target_packages,
        "provenance": provenance_json(manifest),
        "readiness": {
            "source_present": paths.press_summaries_jsonl.exists(),
            "password_record_count": builder.password_record_count,
            "clock_alignment_status": clock_alignment_status,
        },
    }))
}

impl RunSummaryBuilder {
    fn ingest(&mut self, record: &Value) -> DeriveResult<()> {
        if self.first_external_run_id.is_none() {
            self.first_external_run_id = string_field(record, "external_run_id");
        }
        if self.first_package_name.is_none() {
            self.first_package_name = string_field(record, "package_name");
        }
        if self.first_session_id.is_none() {
            self.first_session_id = string_field(record, "session_id");
        }
        if self.clock_alignment_status.is_none() {
            self.clock_alignment_status = string_at(record, "/clock_alignment/getevent");
        }
        if let Some(target_package) = string_field(record, "target_package") {
            increment_map_value(&mut self.target_packages, target_package)?;
        }
        if record
            .get("password_field")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.password_record_count = checked_increment(self.password_record_count)?;
        }
        self.counts.ingest(record)?;
        self.timing.ingest(record)?;
        self.pointer.ingest(record)?;
        self.spatial.ingest(record)?;
        Ok(())
    }
}

impl Counts {
    fn ingest(&mut self, record: &Value) -> DeriveResult<()> {
        self.presses = checked_increment(self.presses)?;
        if record
            .pointer("/quality/has_key_commit")
            .and_then(Value::as_bool)
            == Some(true)
        {
            self.commits = checked_increment(self.commits)?;
        }
        if record
            .pointer("/quality/has_pointer_samples")
            .and_then(Value::as_bool)
            == Some(true)
        {
            self.with_pointer_samples = checked_increment(self.with_pointer_samples)?;
        }
        if record
            .pointer("/quality/has_key_down")
            .and_then(Value::as_bool)
            == Some(true)
        {
            self.with_key_down = checked_increment(self.with_key_down)?;
        }
        if record
            .pointer("/quality/has_key_up")
            .and_then(Value::as_bool)
            == Some(true)
        {
            self.with_key_up = checked_increment(self.with_key_up)?;
        }
        if record
            .pointer("/quality/has_key_commit")
            .and_then(Value::as_bool)
            == Some(true)
        {
            self.with_key_commit = checked_increment(self.with_key_commit)?;
        }
        self.repeats = checked_add_optional_i64(
            self.repeats,
            record
                .pointer("/key_events/repeat_count")
                .and_then(Value::as_i64),
        )?;
        self.long_presses = checked_add_optional_i64(
            self.long_presses,
            record
                .pointer("/key_events/long_press_count")
                .and_then(Value::as_i64),
        )?;
        self.cancels = checked_add_optional_i64(
            self.cancels,
            record
                .pointer("/key_events/cancel_count")
                .and_then(Value::as_i64),
        )?;
        self.ingest_key_class(record)?;
        Ok(())
    }

    fn ingest_key_class(&mut self, record: &Value) -> DeriveResult<()> {
        let Some(key_class) = record.pointer("/key/class").and_then(Value::as_str) else {
            return Ok(());
        };
        match key_class {
            "letter" => {
                self.letters = checked_increment(self.letters)?;
            }
            "space" => {
                self.spaces = checked_increment(self.spaces)?;
            }
            "enter" => {
                self.enters = checked_increment(self.enters)?;
            }
            "delete" => {
                self.deletes = checked_increment(self.deletes)?;
            }
            _other => {}
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "presses": self.presses,
            "commits": self.commits,
            "letters": self.letters,
            "spaces": self.spaces,
            "enters": self.enters,
            "deletes": self.deletes,
            "repeats": self.repeats,
            "long_presses": self.long_presses,
            "cancels": self.cancels,
            "with_pointer_samples": self.with_pointer_samples,
            "with_key_down": self.with_key_down,
            "with_key_up": self.with_key_up,
            "with_key_commit": self.with_key_commit,
        })
    }
}

impl TimingStats {
    fn ingest(&mut self, record: &Value) -> DeriveResult<()> {
        self.hold_ms
            .push_optional(i64_at(record, "/timing/hold_ms"))?;
        let flight = i64_at(record, "/timing/flight_since_previous_commit_ms");
        self.flight_since_previous_commit_ms.push_optional(flight)?;
        if let Some(flight_ms) = flight {
            self.pause_buckets.ingest(flight_ms)?;
        }
        self.down_to_commit_ms
            .push_optional(i64_at(record, "/timing/down_to_commit_ms"))?;
        self.pointer_duration_ms
            .push_optional(i64_at(record, "/timing/pointer_duration_ms"))?;
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "hold_ms": self.hold_ms.to_json(),
            "flight_since_previous_commit_ms": self.flight_since_previous_commit_ms.to_json(),
            "down_to_commit_ms": self.down_to_commit_ms.to_json(),
            "pointer_duration_ms": self.pointer_duration_ms.to_json(),
            "pause_buckets": self.pause_buckets.to_json(),
        })
    }
}

impl PauseBuckets {
    fn ingest(&mut self, value: i64) -> DeriveResult<()> {
        if value < 0_i64 {
            self.negative_ms = checked_increment(self.negative_ms)?;
        } else if value < 100_i64 {
            self.under_100_ms = checked_increment(self.under_100_ms)?;
        } else if value < 250_i64 {
            self.ms_100_to_250 = checked_increment(self.ms_100_to_250)?;
        } else if value < 1_000_i64 {
            self.ms_250_to_1000 = checked_increment(self.ms_250_to_1000)?;
        } else {
            self.ms_1000_or_more = checked_increment(self.ms_1000_or_more)?;
        }
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "negative_ms": self.negative_ms,
            "under_100_ms": self.under_100_ms,
            "100_to_250_ms": self.ms_100_to_250,
            "250_to_1000_ms": self.ms_250_to_1000,
            "1000_ms_or_more": self.ms_1000_or_more,
        })
    }
}

impl PointerStats {
    fn ingest(&mut self, record: &Value) -> DeriveResult<()> {
        self.sample_count
            .push_optional(usize_at(record, "/pointer/sample_count")?)?;
        self.current_sample_count
            .push_optional(usize_at(record, "/pointer/current_sample_count")?)?;
        self.historical_sample_count
            .push_optional(usize_at(record, "/pointer/historical_sample_count")?)?;
        self.path_length_px
            .push_optional(i64_at(record, "/pointer/movement/path_length_px"))?;
        self.max_distance_from_start_px.push_optional(i64_at(
            record,
            "/pointer/movement/max_distance_from_start_px",
        ))?;
        self.pressure
            .push_stats(record.pointer("/pointer/pressure"))?;
        self.size.push_stats(record.pointer("/pointer/size"))?;
        self.touch_major_px
            .push_stats(record.pointer("/pointer/touch_major_px"))?;
        self.touch_minor_px
            .push_stats(record.pointer("/pointer/touch_minor_px"))?;
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "sample_count": self.sample_count.to_json(),
            "current_sample_count": self.current_sample_count.to_json(),
            "historical_sample_count": self.historical_sample_count.to_json(),
            "path_length_px": self.path_length_px.to_json(),
            "max_distance_from_start_px": self.max_distance_from_start_px.to_json(),
            "pressure": self.pressure.to_json(),
            "size": self.size.to_json(),
            "touch_major_px": self.touch_major_px.to_json(),
            "touch_minor_px": self.touch_minor_px.to_json(),
        })
    }
}

impl SpatialStats {
    fn ingest(&mut self, record: &Value) -> DeriveResult<()> {
        self.center_offset_x_px
            .push_optional(i64_at(record, "/key/landing/key_center_offset_x_px"))?;
        self.center_offset_y_px
            .push_optional(i64_at(record, "/key/landing/key_center_offset_y_px"))?;
        self.touch_x_ratio
            .push_optional(f64_at(record, "/key/landing/key_touch_x_ratio"))?;
        self.touch_y_ratio
            .push_optional(f64_at(record, "/key/landing/key_touch_y_ratio"))?;
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "key_center_offset_x_px": self.center_offset_x_px.to_json(),
            "key_center_offset_y_px": self.center_offset_y_px.to_json(),
            "key_touch_x_ratio": self.touch_x_ratio.to_json(),
            "key_touch_y_ratio": self.touch_y_ratio.to_json(),
        })
    }
}

impl I64Stats {
    fn push_optional(&mut self, value: Option<i64>) -> DeriveResult<()> {
        if let Some(actual_value) = value {
            self.push(actual_value)?;
        }
        Ok(())
    }

    fn push_stats(&mut self, value: Option<&Value>) -> DeriveResult<()> {
        let Some(stats) = value else {
            return Ok(());
        };
        if let Some(count) = stats.get("count").and_then(Value::as_u64) {
            self.count = self
                .count
                .checked_add(count)
                .ok_or_else(|| DeriveError::new("integer statistic count overflow"))?;
        }
        self.min = optional_min_i64(self.min, stats.get("min").and_then(Value::as_i64));
        self.max = optional_max_i64(self.max, stats.get("max").and_then(Value::as_i64));
        let sum_value = stats
            .get("sum")
            .and_then(Value::as_i64)
            .or_else(|| stats.pointer("/mean_fraction/sum").and_then(Value::as_i64));
        if let Some(actual_sum) = sum_value {
            self.sum = self
                .sum
                .checked_add(actual_sum)
                .ok_or_else(|| DeriveError::new("integer statistic sum overflow"))?;
        }
        Ok(())
    }

    fn push(&mut self, value: i64) -> DeriveResult<()> {
        self.count = checked_increment(self.count)?;
        self.sum = self
            .sum
            .checked_add(value)
            .ok_or_else(|| DeriveError::new("integer statistic sum overflow"))?;
        self.min = optional_min_i64(self.min, Some(value));
        self.max = optional_max_i64(self.max, Some(value));
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "count": self.count,
            "min": self.min,
            "max": self.max,
            "sum": self.sum,
            "mean_fraction": {
                "sum": self.sum,
                "count": self.count,
            },
        })
    }
}

impl F64Stats {
    fn push_optional(&mut self, value: Option<f64>) -> DeriveResult<()> {
        if let Some(actual_value) = value {
            self.push(actual_value)?;
        }
        Ok(())
    }

    fn push_stats(&mut self, value: Option<&Value>) -> DeriveResult<()> {
        let Some(stats) = value else {
            return Ok(());
        };
        if let Some(count) = stats.get("count").and_then(Value::as_u64) {
            self.count = self
                .count
                .checked_add(count)
                .ok_or_else(|| DeriveError::new("float statistic count overflow"))?;
        }
        if self.first.is_none() {
            self.first = finite_f64_field(stats, "first");
        }
        if let Some(last_value) = finite_f64_field(stats, "last") {
            self.last = Some(last_value);
        }
        self.min = optional_min_f64(self.min, finite_f64_field(stats, "min"));
        self.max = optional_max_f64(self.max, finite_f64_field(stats, "max"));
        Ok(())
    }

    fn push(&mut self, value: f64) -> DeriveResult<()> {
        if !value.is_finite() {
            return Ok(());
        }
        self.count = checked_increment(self.count)?;
        if self.first.is_none() {
            self.first = Some(value);
        }
        self.last = Some(value);
        self.min = optional_min_f64(self.min, Some(value));
        self.max = optional_max_f64(self.max, Some(value));
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "count": self.count,
            "first": self.first,
            "last": self.last,
            "min": self.min,
            "max": self.max,
        })
    }
}

fn provenance_json(manifest: &Value) -> Value {
    json!({
        "input_actor": string_at(manifest, "/input_actor"),
        "input_controller": value_at(manifest, "/input_controller"),
        "input_backend": value_at(manifest, "/input_backend"),
        "input_cadence_policy": string_at(manifest, "/input_cadence_policy"),
        "input_profile": value_at(manifest, "/input_controller_runtime/summary/input_profile"),
        "input_controller_runtime": value_at(manifest, "/input_controller_runtime"),
        "host_start_wall_ms": value_at(manifest, "/host_start_wall_ms"),
        "host_stop_wall_ms": value_at(manifest, "/host_stop_wall_ms"),
    })
}

fn value_at(record: &Value, pointer: &str) -> Value {
    record.pointer(pointer).cloned().unwrap_or(Value::Null)
}

fn i64_at(record: &Value, pointer: &str) -> Option<i64> {
    record.pointer(pointer).and_then(Value::as_i64)
}

fn usize_at(record: &Value, pointer: &str) -> DeriveResult<Option<i64>> {
    let Some(value) = record.pointer(pointer).and_then(Value::as_u64) else {
        return Ok(None);
    };
    Ok(Some(i64::try_from(value).map_err(|error| {
        DeriveError::new(format!("unsigned statistic overflow: {error}"))
    })?))
}

fn f64_at(record: &Value, pointer: &str) -> Option<f64> {
    record
        .pointer(pointer)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn finite_f64_field(record: &Value, field: &str) -> Option<f64> {
    record
        .get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn optional_min_i64(current: Option<i64>, candidate: Option<i64>) -> Option<i64> {
    match (current, candidate) {
        (Some(current_value), Some(candidate_value)) => Some(current_value.min(candidate_value)),
        (Some(current_value), None) => Some(current_value),
        (None, Some(candidate_value)) => Some(candidate_value),
        (None, None) => None,
    }
}

fn optional_max_i64(current: Option<i64>, candidate: Option<i64>) -> Option<i64> {
    match (current, candidate) {
        (Some(current_value), Some(candidate_value)) => Some(current_value.max(candidate_value)),
        (Some(current_value), None) => Some(current_value),
        (None, Some(candidate_value)) => Some(candidate_value),
        (None, None) => None,
    }
}

fn optional_min_f64(current: Option<f64>, candidate: Option<f64>) -> Option<f64> {
    match (current, candidate) {
        (Some(current_value), Some(candidate_value)) => {
            if candidate_value.total_cmp(&current_value) == Ordering::Less {
                Some(candidate_value)
            } else {
                Some(current_value)
            }
        }
        (Some(current_value), None) => Some(current_value),
        (None, Some(candidate_value)) => Some(candidate_value),
        (None, None) => None,
    }
}

fn optional_max_f64(current: Option<f64>, candidate: Option<f64>) -> Option<f64> {
    match (current, candidate) {
        (Some(current_value), Some(candidate_value)) => {
            if candidate_value.total_cmp(&current_value) == Ordering::Greater {
                Some(candidate_value)
            } else {
                Some(current_value)
            }
        }
        (Some(current_value), None) => Some(current_value),
        (None, Some(candidate_value)) => Some(candidate_value),
        (None, None) => None,
    }
}

fn checked_add_optional_i64(current: u64, candidate: Option<i64>) -> DeriveResult<u64> {
    let Some(candidate_value) = candidate else {
        return Ok(current);
    };
    if candidate_value < 0_i64 {
        return Err(DeriveError::new(
            "negative event count in press summary record",
        ));
    }
    current
        .checked_add(u64::try_from(candidate_value).map_err(|error| {
            DeriveError::new(format!("event count conversion overflow: {error}"))
        })?)
        .ok_or_else(|| DeriveError::new("event count overflow"))
}

fn checked_increment(value: u64) -> DeriveResult<u64> {
    value
        .checked_add(1)
        .ok_or_else(|| DeriveError::new("counter overflow"))
}

fn increment_map_value(map: &mut BTreeMap<String, u64>, key: String) -> DeriveResult<()> {
    let current = map.get(&key).copied().unwrap_or(0_u64);
    map.insert(key, checked_increment(current)?);
    Ok(())
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
#[path = "summary/tests.rs"]
mod tests;
