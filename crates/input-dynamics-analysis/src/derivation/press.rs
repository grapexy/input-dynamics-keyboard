//! Per-press summary derivation from IME JSONL records.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::clock::{AlignmentStatus, ClockDomain, TimestampPrecision, millis_to_nanos};
use crate::derivation::{
    DERIVATION_SUMMARY_SCHEMA, DeriveError, DeriveResult, PRESS_SUMMARY_SCHEMA, RunContext,
    find_ime_jsonl, path_text, required_i64, squared_distance, string_field,
};

const CLOCK_ALIGNMENT_STATUS: &str = "not_estimated";

/// Configuration for deriving per-press summaries from a recording.
#[derive(Clone, Debug)]
pub struct DerivePressesConfig {
    /// Recording directory created by `input-dynamics record`.
    pub recording_dir: PathBuf,
    /// IME session JSONL path. Defaults to the single `ime/session-*.jsonl`.
    pub ime_jsonl: Option<PathBuf>,
    /// Output path for derived press summaries.
    pub output: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct PressPaths {
    ime_jsonl: PathBuf,
    output: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PressKey {
    press_id: i64,
}

#[derive(Default)]
struct PressBuilder {
    pointer_samples: Vec<PointerSample>,
    key_events: Vec<KeyEventRecord>,
    line_indexes: SourceLineIndexes,
    has_password_record: bool,
    external_run_id: Option<String>,
    session_id: Option<String>,
    package_name: Option<String>,
    target_package: Option<String>,
    gesture_id: Option<i64>,
}

#[derive(Default)]
struct SourceLineIndexes {
    pointer_samples: Vec<u64>,
    key_down: Vec<u64>,
    key_up: Vec<u64>,
    key_commit: Vec<u64>,
    key_repeat: Vec<u64>,
    key_long_press: Vec<u64>,
    key_cancel: Vec<u64>,
}

#[derive(Clone, Debug)]
struct LineRecord {
    line_index: u64,
    value: Value,
}

#[derive(Clone, Debug)]
struct PointerSample {
    line_index: u64,
    sample_kind: Option<String>,
    action_name: Option<String>,
    event_time: Option<Value>,
    t_uptime_ms: Option<i64>,
    t_event_uptime_ms: Option<i64>,
    x_px: Option<i64>,
    y_px: Option<i64>,
    x_screen_px: Option<i64>,
    y_screen_px: Option<i64>,
    pressure: Option<f64>,
    size: Option<f64>,
    touch_major_px: Option<i64>,
    touch_minor_px: Option<i64>,
    tool_major_px: Option<i64>,
    tool_minor_px: Option<i64>,
    orientation: Option<f64>,
}

#[derive(Clone, Debug)]
struct KeyEventRecord {
    kind: KeyEventKind,
    line_index: u64,
    value: Value,
    event_time: Option<Value>,
    t_uptime_ms: Option<i64>,
    t_event_uptime_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum KeyEventKind {
    Down,
    Up,
    Commit,
    Repeat,
    LongPress,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceTimeStatus {
    CanonicalEventTimeMetadata,
    LegacyEventUptimeMs,
    LegacyUptimeMsFallback,
    Missing,
}

#[derive(Clone, Debug)]
struct SourceEventTime {
    value_ms: Option<i64>,
    status: SourceTimeStatus,
    source_field: Option<String>,
    metadata: Option<Value>,
}

#[derive(Default)]
struct SourceTimeCounts {
    canonical_event_time_metadata: u64,
    legacy_t_event_uptime_ms: u64,
    legacy_t_uptime_ms_fallback: u64,
    missing: u64,
}

#[derive(Default)]
struct FloatStats {
    count: u64,
    first: Option<f64>,
    last: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Default)]
struct IntegerStats {
    count: u64,
    first: Option<i64>,
    last: Option<i64>,
    min: Option<i64>,
    max: Option<i64>,
    sum: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Point {
    x: i64,
    y: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MovementStats {
    dx: i64,
    dy: i64,
    straight_distance: i64,
    path_length: i64,
    max_distance_from_start: i64,
}

#[derive(Clone, Copy)]
struct PressSummaryTiming {
    sort_time_ms: Option<i64>,
    commit_time_ms: Option<i64>,
}

struct PressSummaryInput<'a> {
    context: &'a RunContext,
    recording_dir: &'a Path,
    ime_jsonl: &'a Path,
    key: PressKey,
    builder: &'a PressBuilder,
    previous_commit_time_ms: Option<i64>,
}

impl PressPaths {
    fn from_config(config: &DerivePressesConfig) -> DeriveResult<Self> {
        let ime_jsonl = match config.ime_jsonl.clone() {
            Some(path) => path,
            None => find_ime_jsonl(&config.recording_dir)?,
        };
        let output = config.output.clone().unwrap_or_else(|| {
            config
                .recording_dir
                .join("derived")
                .join("press_summaries.jsonl")
        });
        Ok(Self { ime_jsonl, output })
    }
}

impl KeyEventKind {
    fn from_event_name(event: &str) -> Option<Self> {
        match event {
            "key_down" => Some(Self::Down),
            "key_up" => Some(Self::Up),
            "key_commit" => Some(Self::Commit),
            "key_repeat" => Some(Self::Repeat),
            "key_long_press" => Some(Self::LongPress),
            "key_cancel" => Some(Self::Cancel),
            "pointer_sample" => None,
            _other => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Down => "key_down",
            Self::Up => "key_up",
            Self::Commit => "key_commit",
            Self::Repeat => "key_repeat",
            Self::LongPress => "key_long_press",
            Self::Cancel => "key_cancel",
        }
    }
}

impl SourceTimeStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalEventTimeMetadata => "canonical_event_time_metadata",
            Self::LegacyEventUptimeMs => "legacy_t_event_uptime_ms",
            Self::LegacyUptimeMsFallback => "legacy_t_uptime_ms_fallback",
            Self::Missing => "missing",
        }
    }
}

impl SourceEventTime {
    fn from_parts(
        event_time: Option<&Value>,
        t_event_uptime_ms: Option<i64>,
        t_uptime_ms: Option<i64>,
    ) -> Self {
        if let Some(metadata) = event_time
            && metadata.get("clock_domain").and_then(Value::as_str)
                == Some(ClockDomain::AndroidUptimeMs.as_str())
            && metadata.get("timestamp_precision").and_then(Value::as_str)
                == Some(TimestampPrecision::Milliseconds.as_str())
            && metadata.get("field").and_then(Value::as_str) == Some("t_event_uptime_ms")
            && let Some(value_ms) = t_event_uptime_ms
        {
            return Self {
                value_ms: Some(value_ms),
                status: SourceTimeStatus::CanonicalEventTimeMetadata,
                source_field: Some(String::from("t_event_uptime_ms")),
                metadata: Some(metadata.clone()),
            };
        }
        if let Some(value_ms) = t_event_uptime_ms {
            return Self {
                value_ms: Some(value_ms),
                status: SourceTimeStatus::LegacyEventUptimeMs,
                source_field: Some(String::from("t_event_uptime_ms")),
                metadata: event_time.cloned(),
            };
        }
        if let Some(value_ms) = t_uptime_ms {
            return Self {
                value_ms: Some(value_ms),
                status: SourceTimeStatus::LegacyUptimeMsFallback,
                source_field: Some(String::from("t_uptime_ms")),
                metadata: event_time.cloned(),
            };
        }
        Self {
            value_ms: None,
            status: SourceTimeStatus::Missing,
            source_field: None,
            metadata: event_time.cloned(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "source_clock_domain": self.value_ms.map(|_value| ClockDomain::AndroidUptimeMs.as_str()),
            "source_timestamp_precision": self.value_ms.map(|_value| TimestampPrecision::Milliseconds.as_str()),
            "source_time_ms": self.value_ms,
            "source_time_ns": self.value_ms.and_then(millis_to_nanos),
            "source_field": self.source_field,
            "source_time_status": self.status.as_str(),
            "timestamp_role_metadata": self.metadata,
            "normalized_clock_domain": Value::Null,
            "normalized_time_ns": Value::Null,
            "alignment_status": AlignmentStatus::NotEstimated.as_str(),
            "transform_id": Value::Null,
            "uncertainty_ns": Value::Null,
        })
    }
}

impl SourceTimeCounts {
    fn push(&mut self, status: SourceTimeStatus) -> DeriveResult<()> {
        let count = match status {
            SourceTimeStatus::CanonicalEventTimeMetadata => &mut self.canonical_event_time_metadata,
            SourceTimeStatus::LegacyEventUptimeMs => &mut self.legacy_t_event_uptime_ms,
            SourceTimeStatus::LegacyUptimeMsFallback => &mut self.legacy_t_uptime_ms_fallback,
            SourceTimeStatus::Missing => &mut self.missing,
        };
        *count = checked_increment(*count)?;
        Ok(())
    }

    fn to_json(&self) -> Value {
        json!({
            "canonical_event_time_metadata": self.canonical_event_time_metadata,
            "legacy_t_event_uptime_ms": self.legacy_t_event_uptime_ms,
            "legacy_t_uptime_ms_fallback": self.legacy_t_uptime_ms_fallback,
            "missing": self.missing,
        })
    }
}

impl PressBuilder {
    fn ingest(&mut self, record: &LineRecord) {
        capture_context(self, &record.value);
        if record
            .value
            .get("password_field")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.has_password_record = true;
        }
        if let Some(gesture_id) = record.value.get("gesture_id").and_then(Value::as_i64) {
            self.gesture_id.get_or_insert(gesture_id);
        }
        let event = string_field(&record.value, "event");
        match event.as_deref() {
            Some("pointer_sample") => self.add_pointer_sample(record),
            Some(event_name) => {
                if let Some(kind) = KeyEventKind::from_event_name(event_name) {
                    self.add_key_event(record, kind);
                }
            }
            None => {}
        }
    }

    fn add_pointer_sample(&mut self, record: &LineRecord) {
        self.line_indexes.pointer_samples.push(record.line_index);
        self.pointer_samples
            .push(PointerSample::from_record(record));
    }

    fn add_key_event(&mut self, record: &LineRecord, kind: KeyEventKind) {
        self.line_indexes.push_key(kind, record.line_index);
        self.key_events
            .push(KeyEventRecord::from_record(record, kind));
    }

    fn primary_key_event(&self) -> Option<&KeyEventRecord> {
        self.first_key_event(KeyEventKind::Down)
            .or_else(|| self.first_key_event(KeyEventKind::Commit))
            .or_else(|| self.first_key_event(KeyEventKind::Up))
            .or_else(|| self.first_key_event(KeyEventKind::Repeat))
            .or_else(|| self.first_key_event(KeyEventKind::LongPress))
            .or_else(|| self.first_key_event(KeyEventKind::Cancel))
    }

    fn first_key_event(&self, kind: KeyEventKind) -> Option<&KeyEventRecord> {
        self.key_events
            .iter()
            .filter(|event| event.kind == kind)
            .min_by_key(|event| event.sort_key())
    }

    fn timing(&self) -> PressSummaryTiming {
        let sort_time_ms = self
            .first_key_event(KeyEventKind::Down)
            .and_then(KeyEventRecord::event_time_ms)
            .or_else(|| {
                self.pointer_samples
                    .iter()
                    .filter_map(PointerSample::event_time_ms)
                    .min()
            });
        let commit_time_ms = self
            .first_key_event(KeyEventKind::Commit)
            .and_then(KeyEventRecord::event_time_ms);
        PressSummaryTiming {
            sort_time_ms,
            commit_time_ms,
        }
    }
}

impl SourceLineIndexes {
    fn push_key(&mut self, kind: KeyEventKind, line_index: u64) {
        match kind {
            KeyEventKind::Down => self.key_down.push(line_index),
            KeyEventKind::Up => self.key_up.push(line_index),
            KeyEventKind::Commit => self.key_commit.push(line_index),
            KeyEventKind::Repeat => self.key_repeat.push(line_index),
            KeyEventKind::LongPress => self.key_long_press.push(line_index),
            KeyEventKind::Cancel => self.key_cancel.push(line_index),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "pointer_samples": self.pointer_samples,
            "key_down": self.key_down,
            "key_up": self.key_up,
            "key_commit": self.key_commit,
            "key_repeat": self.key_repeat,
            "key_long_press": self.key_long_press,
            "key_cancel": self.key_cancel,
        })
    }
}

impl PointerSample {
    fn from_record(record: &LineRecord) -> Self {
        Self {
            line_index: record.line_index,
            sample_kind: string_field(&record.value, "sample_kind"),
            action_name: string_field(&record.value, "action_name")
                .or_else(|| string_field(&record.value, "motion_action_name")),
            event_time: record.value.get("event_time").cloned(),
            t_uptime_ms: record.value.get("t_uptime_ms").and_then(Value::as_i64),
            t_event_uptime_ms: record
                .value
                .get("t_event_uptime_ms")
                .and_then(Value::as_i64),
            x_px: record.value.get("x_px").and_then(Value::as_i64),
            y_px: record.value.get("y_px").and_then(Value::as_i64),
            x_screen_px: record.value.get("x_screen_px").and_then(Value::as_i64),
            y_screen_px: record.value.get("y_screen_px").and_then(Value::as_i64),
            pressure: finite_f64(&record.value, "pressure"),
            size: finite_f64(&record.value, "size"),
            touch_major_px: record.value.get("touch_major_px").and_then(Value::as_i64),
            touch_minor_px: record.value.get("touch_minor_px").and_then(Value::as_i64),
            tool_major_px: record.value.get("tool_major_px").and_then(Value::as_i64),
            tool_minor_px: record.value.get("tool_minor_px").and_then(Value::as_i64),
            orientation: finite_f64(&record.value, "orientation"),
        }
    }

    fn sort_key(&self) -> (i64, u64) {
        (self.event_time_ms().unwrap_or(i64::MAX), self.line_index)
    }

    fn event_time_ms(&self) -> Option<i64> {
        self.source_event_time().value_ms
    }

    fn source_event_time(&self) -> SourceEventTime {
        SourceEventTime::from_parts(
            self.event_time.as_ref(),
            self.t_event_uptime_ms,
            self.t_uptime_ms,
        )
    }

    fn point(&self) -> Option<Point> {
        Some(Point {
            x: self.x_px?,
            y: self.y_px?,
        })
    }

    fn endpoint_json(&self) -> Value {
        json!({
            "line_index": self.line_index,
            "sample_kind": self.sample_kind,
            "action_name": self.action_name,
            "t_uptime_ms": self.t_uptime_ms,
            "t_event_uptime_ms": self.t_event_uptime_ms,
            "time": self.source_event_time().to_json(),
            "x_px": self.x_px,
            "y_px": self.y_px,
            "x_screen_px": self.x_screen_px,
            "y_screen_px": self.y_screen_px,
            "pressure": self.pressure,
            "size": self.size,
            "touch_major_px": self.touch_major_px,
            "touch_minor_px": self.touch_minor_px,
            "tool_major_px": self.tool_major_px,
            "tool_minor_px": self.tool_minor_px,
            "orientation": self.orientation,
        })
    }
}

impl KeyEventRecord {
    fn from_record(record: &LineRecord, kind: KeyEventKind) -> Self {
        Self {
            kind,
            line_index: record.line_index,
            value: record.value.clone(),
            event_time: record.value.get("event_time").cloned(),
            t_uptime_ms: record.value.get("t_uptime_ms").and_then(Value::as_i64),
            t_event_uptime_ms: record
                .value
                .get("t_event_uptime_ms")
                .and_then(Value::as_i64),
        }
    }

    fn sort_key(&self) -> (i64, u64) {
        (self.event_time_ms().unwrap_or(i64::MAX), self.line_index)
    }

    fn event_time_ms(&self) -> Option<i64> {
        self.source_event_time().value_ms
    }

    fn source_event_time(&self) -> SourceEventTime {
        SourceEventTime::from_parts(
            self.event_time.as_ref(),
            self.t_event_uptime_ms,
            self.t_uptime_ms,
        )
    }
}

impl FloatStats {
    fn push(&mut self, value: f64) -> DeriveResult<()> {
        if !value.is_finite() {
            return Ok(());
        }
        self.count = checked_increment(self.count)?;
        self.first.get_or_insert(value);
        self.last = Some(value);
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
        Ok(())
    }

    fn to_json(&self) -> Value {
        if self.count == 0 {
            return Value::Null;
        }
        json!({
            "count": self.count,
            "first": self.first,
            "last": self.last,
            "min": self.min,
            "max": self.max,
        })
    }
}

impl IntegerStats {
    fn push(&mut self, value: i64) -> DeriveResult<()> {
        self.count = checked_increment(self.count)?;
        self.first.get_or_insert(value);
        self.last = Some(value);
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
        self.sum = self
            .sum
            .checked_add(value)
            .ok_or_else(|| DeriveError::new("integer statistic sum overflow"))?;
        Ok(())
    }

    fn to_json(&self) -> Value {
        if self.count == 0 {
            return Value::Null;
        }
        json!({
            "count": self.count,
            "first": self.first,
            "last": self.last,
            "min": self.min,
            "max": self.max,
            "mean_fraction": {
                "sum": self.sum,
                "count": self.count,
            },
        })
    }
}

impl Point {
    fn distance_to(self, other: Self) -> DeriveResult<i64> {
        let dx = other
            .x
            .checked_sub(self.x)
            .ok_or_else(|| DeriveError::new("point dx overflow"))?;
        let dy = other
            .y
            .checked_sub(self.y)
            .ok_or_else(|| DeriveError::new("point dy overflow"))?;
        Ok(squared_distance(dx, dy)?.isqrt())
    }
}

/// Derive per-press summary JSONL output.
pub fn derive_press_summaries(config: &DerivePressesConfig) -> DeriveResult<Value> {
    let paths = PressPaths::from_config(config)?;
    let records = read_jsonl_with_line_indexes(&paths.ime_jsonl)?;
    let values = records
        .iter()
        .map(|record| record.value.clone())
        .collect::<Vec<_>>();
    let context = RunContext::from_records(&config.recording_dir, &values)?;
    let mut builders = group_press_records(&records)?;
    let skipped_password_press_count = builders
        .values()
        .filter(|builder| builder.has_password_record)
        .count();
    builders.retain(|_key, builder| !builder.has_password_record);
    let mut ordered = builders.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|entry| {
        let key = entry.0;
        let builder = &entry.1;
        let timing = builder.timing();
        (timing.sort_time_ms.unwrap_or(i64::MAX), key.press_id)
    });

    let mut previous_commit_time_ms = None;
    let mut output = Vec::new();
    for entry in &ordered {
        let key = entry.0;
        let builder = &entry.1;
        let input = PressSummaryInput {
            context: &context,
            recording_dir: &config.recording_dir,
            ime_jsonl: &paths.ime_jsonl,
            key,
            builder,
            previous_commit_time_ms,
        };
        output.push(press_summary_json(&input)?);
        if let Some(commit_time_ms) = builder.timing().commit_time_ms {
            previous_commit_time_ms = Some(commit_time_ms);
        }
    }

    crate::derivation::jsonl::write_jsonl(&paths.output, &output)?;
    Ok(json!({
        "ok": true,
        "schema": DERIVATION_SUMMARY_SCHEMA,
        "derivation": "press_summaries",
        "recording_dir": path_text(&config.recording_dir),
        "ime_jsonl": path_text(&paths.ime_jsonl),
        "output": path_text(&paths.output),
        "external_run_id": context.external_run_id,
        "package_name": context.package_name,
        "press_summary_count": output.len(),
        "skipped_password_press_count": skipped_password_press_count,
        "clock_alignment_status": CLOCK_ALIGNMENT_STATUS,
    }))
}

fn group_press_records(records: &[LineRecord]) -> DeriveResult<BTreeMap<PressKey, PressBuilder>> {
    let mut builders = BTreeMap::new();
    for record in records {
        if !is_press_record(&record.value) {
            continue;
        }
        let press_id = required_i64(&record.value, "press_id")?;
        if press_id < 0 {
            return Err(DeriveError::new("press_id must be non-negative"));
        }
        builders
            .entry(PressKey { press_id })
            .or_insert_with(PressBuilder::default)
            .ingest(record);
    }
    Ok(builders)
}

fn is_press_record(record: &Value) -> bool {
    let event = string_field(record, "event");
    match event.as_deref() {
        Some("pointer_sample") => true,
        Some(event_name) => KeyEventKind::from_event_name(event_name).is_some(),
        None => false,
    }
}

fn press_summary_json(input: &PressSummaryInput<'_>) -> DeriveResult<Value> {
    let timing = input.builder.timing();
    let primary_key = input.builder.primary_key_event();
    Ok(json!({
        "schema": PRESS_SUMMARY_SCHEMA,
        "event": "press_summary",
        "press_summary_id": press_summary_id(input.context, input.key.press_id),
        "press_id": input.key.press_id,
        "gesture_id": input.builder.gesture_id,
        "external_run_id": input
            .builder
            .external_run_id
            .clone()
            .or_else(|| input.context.external_run_id.clone()),
        "session_id": input
            .builder
            .session_id
            .clone()
            .or_else(|| input.context.session_id.clone()),
        "package_name": input
            .builder
            .package_name
            .clone()
            .or_else(|| input.context.package_name.clone()),
        "target_package": input.builder.target_package,
        "password_field": false,
        "source": "ime_jsonl",
        "source_ref": source_ref(input.recording_dir, input.ime_jsonl, input.key.press_id, &input.builder.line_indexes),
        "clock_domain": ClockDomain::AndroidUptimeMs.as_str(),
        "clock_alignment": {
            "getevent": CLOCK_ALIGNMENT_STATUS,
        },
        "timing_clock": timing_clock_json(input.builder)?,
        "timing": timing_json(input.builder, input.previous_commit_time_ms),
        "key": primary_key.map_or(Value::Null, key_summary_json),
        "key_events": key_events_json(input.builder),
        "pointer": pointer_summary_json(&input.builder.pointer_samples)?,
        "quality": quality_json(input.builder, timing),
    }))
}

fn capture_context(builder: &mut PressBuilder, record: &Value) {
    if builder.external_run_id.is_none() {
        builder.external_run_id = string_field(record, "external_run_id");
    }
    if builder.session_id.is_none() {
        builder.session_id = string_field(record, "session_id");
    }
    if builder.package_name.is_none() {
        builder.package_name = string_field(record, "package_name");
    }
    if builder.target_package.is_none() {
        builder.target_package = string_field(record, "target_package");
    }
}

fn timing_json(builder: &PressBuilder, previous_commit_time_ms: Option<i64>) -> Value {
    let down = builder
        .first_key_event(KeyEventKind::Down)
        .and_then(KeyEventRecord::event_time_ms);
    let up = builder
        .first_key_event(KeyEventKind::Up)
        .and_then(KeyEventRecord::event_time_ms);
    let commit = builder
        .first_key_event(KeyEventKind::Commit)
        .and_then(KeyEventRecord::event_time_ms);
    let first_pointer = builder
        .pointer_samples
        .iter()
        .filter_map(PointerSample::event_time_ms)
        .min();
    let last_pointer = builder
        .pointer_samples
        .iter()
        .filter_map(PointerSample::event_time_ms)
        .max();
    json!({
        "first_pointer_t_event_uptime_ms": first_pointer,
        "last_pointer_t_event_uptime_ms": last_pointer,
        "key_down_t_event_uptime_ms": down,
        "key_up_t_event_uptime_ms": up,
        "key_commit_t_event_uptime_ms": commit,
        "pointer_duration_ms": optional_delta(first_pointer, last_pointer),
        "hold_ms": optional_delta(down, up),
        "down_to_commit_ms": optional_delta(down, commit),
        "up_to_commit_ms": optional_delta(up, commit),
        "flight_since_previous_commit_ms": optional_delta(previous_commit_time_ms, down),
    })
}

fn key_events_json(builder: &PressBuilder) -> Value {
    json!({
        "down": builder.first_key_event(KeyEventKind::Down).map_or(Value::Null, key_event_json),
        "up": builder.first_key_event(KeyEventKind::Up).map_or(Value::Null, key_event_json),
        "commit": builder.first_key_event(KeyEventKind::Commit).map_or(Value::Null, key_event_json),
        "repeat_count": count_key_events(builder, KeyEventKind::Repeat),
        "long_press_count": count_key_events(builder, KeyEventKind::LongPress),
        "cancel_count": count_key_events(builder, KeyEventKind::Cancel),
        "line_indexes": builder.line_indexes.to_json(),
    })
}

fn key_event_json(event: &KeyEventRecord) -> Value {
    json!({
        "event": event.kind.as_str(),
        "line_index": event.line_index,
        "t_uptime_ms": event.t_uptime_ms,
        "t_event_uptime_ms": event.t_event_uptime_ms,
        "time": event.source_event_time().to_json(),
        "x_px": event.value.get("x_px").cloned().unwrap_or(Value::Null),
        "y_px": event.value.get("y_px").cloned().unwrap_or(Value::Null),
        "x_screen_px": event.value.get("x_screen_px").cloned().unwrap_or(Value::Null),
        "y_screen_px": event.value.get("y_screen_px").cloned().unwrap_or(Value::Null),
    })
}

fn timing_clock_json(builder: &PressBuilder) -> DeriveResult<Value> {
    let mut counts = SourceTimeCounts::default();
    for sample in &builder.pointer_samples {
        counts.push(sample.source_event_time().status)?;
    }
    for event in &builder.key_events {
        counts.push(event.source_event_time().status)?;
    }
    Ok(json!({
        "source_clock_domain": ClockDomain::AndroidUptimeMs.as_str(),
        "source_timestamp_precision": TimestampPrecision::Milliseconds.as_str(),
        "duration_unit": "milliseconds",
        "alignment_status": AlignmentStatus::NotEstimated.as_str(),
        "normalized_clock_domain": Value::Null,
        "normalized_time_interval_ns": Value::Null,
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
        "source_time_status_counts": counts.to_json(),
    }))
}

fn key_summary_json(event: &KeyEventRecord) -> Value {
    json!({
        "code": event.value.get("key_code").cloned().unwrap_or(Value::Null),
        "code_printable": event.value.get("key_code_printable").cloned().unwrap_or(Value::Null),
        "label": event.value.get("key_label").cloned().unwrap_or(Value::Null),
        "class": event.value.get("key_class").cloned().unwrap_or(Value::Null),
        "icon_name": event.value.get("key_icon_name").cloned().unwrap_or(Value::Null),
        "output_text": event.value.get("key_output_text").cloned().unwrap_or(Value::Null),
        "present": event.value.get("key_present").cloned().unwrap_or(Value::Null),
        "repeatable": event.value.get("key_repeatable").cloned().unwrap_or(Value::Null),
        "bounds": key_bounds_json(&event.value),
        "landing": key_landing_json(&event.value),
        "coordinate_frame": coordinate_frame_json(&event.value),
    })
}

fn key_bounds_json(record: &Value) -> Value {
    json!({
        "x_px": record.get("key_x_px").cloned().unwrap_or(Value::Null),
        "y_px": record.get("key_y_px").cloned().unwrap_or(Value::Null),
        "width_px": record.get("key_width_px").cloned().unwrap_or(Value::Null),
        "height_px": record.get("key_height_px").cloned().unwrap_or(Value::Null),
        "hitbox_left_px": record.get("key_hitbox_left_px").cloned().unwrap_or(Value::Null),
        "hitbox_top_px": record.get("key_hitbox_top_px").cloned().unwrap_or(Value::Null),
        "hitbox_right_px": record.get("key_hitbox_right_px").cloned().unwrap_or(Value::Null),
        "hitbox_bottom_px": record.get("key_hitbox_bottom_px").cloned().unwrap_or(Value::Null),
    })
}

fn key_landing_json(record: &Value) -> Value {
    json!({
        "x_px": record.get("x_px").cloned().unwrap_or(Value::Null),
        "y_px": record.get("y_px").cloned().unwrap_or(Value::Null),
        "x_screen_px": record.get("x_screen_px").cloned().unwrap_or(Value::Null),
        "y_screen_px": record.get("y_screen_px").cloned().unwrap_or(Value::Null),
        "key_touch_x_ratio": record.get("key_touch_x_ratio").cloned().unwrap_or(Value::Null),
        "key_touch_y_ratio": record.get("key_touch_y_ratio").cloned().unwrap_or(Value::Null),
        "key_center_offset_x_px": record.get("key_center_offset_x_px").cloned().unwrap_or(Value::Null),
        "key_center_offset_y_px": record.get("key_center_offset_y_px").cloned().unwrap_or(Value::Null),
    })
}

fn coordinate_frame_json(record: &Value) -> Value {
    json!({
        "available": record.get("coordinate_frame_available").cloned().unwrap_or(Value::Null),
        "coordinate_space": record.get("coordinate_space").cloned().unwrap_or(Value::Null),
        "keyboard_view_visible": record.get("keyboard_view_visible").cloned().unwrap_or(Value::Null),
        "keyboard_view_width_px": record.get("keyboard_view_width_px").cloned().unwrap_or(Value::Null),
        "keyboard_view_height_px": record.get("keyboard_view_height_px").cloned().unwrap_or(Value::Null),
        "keyboard_view_left_screen_px": record.get("keyboard_view_left_screen_px").cloned().unwrap_or(Value::Null),
        "keyboard_view_top_screen_px": record.get("keyboard_view_top_screen_px").cloned().unwrap_or(Value::Null),
        "keyboard_view_right_screen_px": record.get("keyboard_view_right_screen_px").cloned().unwrap_or(Value::Null),
        "keyboard_view_bottom_screen_px": record.get("keyboard_view_bottom_screen_px").cloned().unwrap_or(Value::Null),
        "display_width_px": record.get("display_width_px").cloned().unwrap_or(Value::Null),
        "display_height_px": record.get("display_height_px").cloned().unwrap_or(Value::Null),
        "display_rotation": record.get("display_rotation").cloned().unwrap_or(Value::Null),
        "display_rotation_name": record.get("display_rotation_name").cloned().unwrap_or(Value::Null),
    })
}

fn pointer_summary_json(samples: &[PointerSample]) -> DeriveResult<Value> {
    let mut sorted = samples.to_vec();
    sorted.sort_by_key(PointerSample::sort_key);
    Ok(json!({
        "sample_count": sorted.len(),
        "current_sample_count": sample_kind_count(&sorted, "current"),
        "historical_sample_count": sample_kind_count(&sorted, "historical"),
        "action_names": action_names(&sorted),
        "first": sorted.first().map_or(Value::Null, PointerSample::endpoint_json),
        "last": sorted.last().map_or(Value::Null, PointerSample::endpoint_json),
        "movement": movement_json(&sorted)?,
        "pressure": float_stats_json(&sorted, |sample| sample.pressure)?,
        "size": float_stats_json(&sorted, |sample| sample.size)?,
        "touch_major_px": integer_stats_json(&sorted, |sample| sample.touch_major_px)?,
        "touch_minor_px": integer_stats_json(&sorted, |sample| sample.touch_minor_px)?,
        "tool_major_px": integer_stats_json(&sorted, |sample| sample.tool_major_px)?,
        "tool_minor_px": integer_stats_json(&sorted, |sample| sample.tool_minor_px)?,
        "orientation": float_stats_json(&sorted, |sample| sample.orientation)?,
    }))
}

fn movement_json(samples: &[PointerSample]) -> DeriveResult<Value> {
    let points = samples
        .iter()
        .filter_map(PointerSample::point)
        .collect::<Vec<_>>();
    let Some(stats) = path_metrics_for_points(&points)? else {
        return Ok(Value::Null);
    };
    Ok(json!({
        "dx_px": stats.dx,
        "dy_px": stats.dy,
        "straight_distance_px": stats.straight_distance,
        "path_length_px": stats.path_length,
        "max_distance_from_start_px": stats.max_distance_from_start,
    }))
}

fn path_metrics_for_points(points: &[Point]) -> DeriveResult<Option<MovementStats>> {
    let Some(first) = points.first().copied() else {
        return Ok(None);
    };
    let Some(last) = points.last().copied() else {
        return Ok(None);
    };
    let mut path_length_px = 0_i64;
    let mut max_distance_from_start_px = 0_i64;
    let mut previous = first;
    for point in points {
        path_length_px = path_length_px
            .checked_add(previous.distance_to(*point)?)
            .ok_or_else(|| DeriveError::new("path length overflow"))?;
        max_distance_from_start_px = max_distance_from_start_px.max(first.distance_to(*point)?);
        previous = *point;
    }
    let dx = last
        .x
        .checked_sub(first.x)
        .ok_or_else(|| DeriveError::new("movement dx overflow"))?;
    let dy = last
        .y
        .checked_sub(first.y)
        .ok_or_else(|| DeriveError::new("movement dy overflow"))?;
    Ok(Some(MovementStats {
        dx,
        dy,
        straight_distance: first.distance_to(last)?,
        path_length: path_length_px,
        max_distance_from_start: max_distance_from_start_px,
    }))
}

fn float_stats_json<F>(samples: &[PointerSample], get_value: F) -> DeriveResult<Value>
where
    F: Fn(&PointerSample) -> Option<f64>,
{
    let mut stats = FloatStats::default();
    for sample in samples {
        if let Some(value) = get_value(sample) {
            stats.push(value)?;
        }
    }
    Ok(stats.to_json())
}

fn integer_stats_json<F>(samples: &[PointerSample], get_value: F) -> DeriveResult<Value>
where
    F: Fn(&PointerSample) -> Option<i64>,
{
    let mut stats = IntegerStats::default();
    for sample in samples {
        if let Some(value) = get_value(sample) {
            stats.push(value)?;
        }
    }
    Ok(stats.to_json())
}

fn count_key_events(builder: &PressBuilder, kind: KeyEventKind) -> usize {
    builder
        .key_events
        .iter()
        .filter(|event| event.kind == kind)
        .count()
}

fn sample_kind_count(samples: &[PointerSample], kind: &str) -> usize {
    samples
        .iter()
        .filter(|sample| sample.sample_kind.as_deref() == Some(kind))
        .count()
}

fn action_names(samples: &[PointerSample]) -> Vec<String> {
    samples
        .iter()
        .filter_map(|sample| sample.action_name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn quality_json(builder: &PressBuilder, timing: PressSummaryTiming) -> Value {
    json!({
        "has_key_down": builder.first_key_event(KeyEventKind::Down).is_some(),
        "has_key_up": builder.first_key_event(KeyEventKind::Up).is_some(),
        "has_key_commit": builder.first_key_event(KeyEventKind::Commit).is_some(),
        "has_pointer_samples": !builder.pointer_samples.is_empty(),
        "has_sort_time": timing.sort_time_ms.is_some(),
    })
}

fn source_ref(
    recording_dir: &Path,
    ime_jsonl: &Path,
    press_id: i64,
    line_indexes: &SourceLineIndexes,
) -> Value {
    json!({
        "path": relative_path_text(recording_dir, ime_jsonl),
        "press_id": press_id,
        "line_indexes": line_indexes.to_json(),
    })
}

fn press_summary_id(context: &RunContext, press_id: i64) -> String {
    let run_id = context.external_run_id.as_deref().unwrap_or("unknown-run");
    format!("press:{run_id}:{press_id}")
}

fn optional_delta(start: Option<i64>, end: Option<i64>) -> Option<i64> {
    end?.checked_sub(start?)
}

fn finite_f64(record: &Value, field: &str) -> Option<f64> {
    record
        .get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn checked_increment(value: u64) -> DeriveResult<u64> {
    value
        .checked_add(1)
        .ok_or_else(|| DeriveError::new("counter overflow"))
}

fn read_jsonl_with_line_indexes(path: &Path) -> DeriveResult<Vec<LineRecord>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for (index, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }
        let line_index = u64::try_from(index)
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

fn relative_path_text(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .map_or_else(|_strip_error| path_text(path), path_text)
}

#[cfg(test)]
#[path = "press/tests.rs"]
mod tests;
