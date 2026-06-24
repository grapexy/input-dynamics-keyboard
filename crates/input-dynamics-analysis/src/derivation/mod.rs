//! Derive higher-level records from normalized input-dynamics streams.

mod dismissal;
mod error;
mod jsonl;
mod press;
mod summary;
mod timeline;
mod touch;
mod video_map;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use error::{DeriveError, DeriveResult};

use crate::derivation::dismissal::derive_dismissal_inferences;
use crate::derivation::jsonl::{read_jsonl, write_jsonl};
pub use crate::derivation::press::{DerivePressesConfig, derive_press_summaries};
pub use crate::derivation::summary::{DeriveRunSummaryConfig, derive_run_summary};
pub use crate::derivation::timeline::{DeriveTimelineConfig, derive_timeline};
use crate::derivation::touch::derive_touch_gestures;
pub use crate::derivation::video_map::{DeriveVideoMapConfig, FfprobeInvocation, derive_video_map};

/// Schema written to derived touch gesture JSONL records.
pub const TOUCH_GESTURE_SCHEMA: &str = "input_dynamics_touch_gesture.v1";

/// Schema written to derived dismissal inference JSONL records.
pub const DISMISSAL_INFERENCE_SCHEMA: &str = "input_dynamics_dismissal_inference.v1";

/// Schema written to derived press summary JSONL records.
pub const PRESS_SUMMARY_SCHEMA: &str = "input_dynamics_press_summary.v1";

/// Schema written to derivation command summaries.
pub const DERIVATION_SUMMARY_SCHEMA: &str = "input_dynamics_derivation_summary.v1";

/// Schema written to timeline event JSONL records.
pub const TIMELINE_EVENT_SCHEMA: &str = "input_dynamics_timeline_event.v1";

/// Schema written to timeline index JSON files.
pub const TIMELINE_INDEX_SCHEMA: &str = "input_dynamics_timeline_index.v1";

/// Schema written to run summary JSON files.
pub const RUN_SUMMARY_SCHEMA: &str = "input_dynamics_run_summary.v1";

/// Schema written to video-map index JSON files.
pub const VIDEO_MAP_INDEX_SCHEMA: &str = "input_dynamics_video_map_index.v1";

/// Schema written to video-frame JSONL records.
pub const VIDEO_FRAME_SCHEMA: &str = "input_dynamics_video_frame.v1";

/// Schema written to video-alignment JSON files.
pub const VIDEO_ALIGNMENT_SCHEMA: &str = "input_dynamics_video_alignment.v1";

/// Schema written to event-video-frame map JSONL records.
pub const EVENT_VIDEO_FRAME_MAP_SCHEMA: &str = "input_dynamics_event_video_frame_map.v1";

/// Schema for derivation policy JSON files.
pub const DERIVATION_POLICY_SCHEMA: &str = "input_dynamics_derivation_policy.v1";

/// Bundled default derivation policy JSON.
pub const DEFAULT_DERIVATION_POLICY_JSON: &str =
    include_str!("../../../../policies/default-derivation-v1.json");

/// Screen coordinate frame used for normalized ratios.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenConfig {
    /// Screen width in touchscreen coordinate pixels.
    pub width: i64,
    /// Screen height in touchscreen coordinate pixels.
    pub height: i64,
    /// Visible keyboard top y-coordinate, when known.
    pub keyboard_top_y: Option<i64>,
}

/// Tunable policy for deriving gestures and dismissal inferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DismissalDerivationPolicy {
    /// Edge-start threshold as parts per million of screen width.
    pub edge_ratio_ppm: i64,
    /// Required inward movement as parts per million of screen width.
    pub edge_inward_ratio_ppm: i64,
    /// Maximum vertical drift as parts per million of screen height.
    pub max_edge_vertical_drift_ratio_ppm: i64,
    /// Maximum tap movement in pixels.
    pub tap_max_distance_px: i64,
    /// Maximum tap duration in milliseconds.
    pub tap_max_duration_ms: i64,
    /// Gesture-to-hide correlation window in milliseconds.
    pub hide_correlation_window_ms: i64,
}

/// Configuration for deriving dismissal records from a run directory.
#[derive(Clone, Debug)]
pub struct DeriveDismissalsConfig {
    /// Recording directory created by the complete session workflow.
    pub recording_dir: PathBuf,
    /// Normalized `adb/getevent.jsonl` path. Defaults under `recording_dir`.
    pub getevent_jsonl: Option<PathBuf>,
    /// IME session JSONL path. Defaults to the single `ime/session-*.jsonl`.
    pub ime_jsonl: Option<PathBuf>,
    /// Output path for derived touch gestures.
    pub touch_gestures_output: Option<PathBuf>,
    /// Output path for derived dismissal inferences.
    pub dismissals_output: Option<PathBuf>,
    /// Screen coordinate frame.
    pub screen: ScreenConfig,
    /// Derivation policy.
    pub policy: DismissalDerivationPolicy,
    /// Optional derivation-policy provenance summary supplied by the caller.
    pub policy_summary: Option<Value>,
}

#[derive(Clone, Debug)]
pub(crate) struct RunContext {
    pub(crate) external_run_id: Option<String>,
    pub(crate) package_name: Option<String>,
    pub(crate) session_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ImeEvent {
    pub(crate) line_index: u64,
    pub(crate) event: String,
    pub(crate) t_uptime_ms: i64,
    pub(crate) target_package: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct RatioValue {
    numerator: i64,
    denominator: i64,
    decimal_ppm: i64,
}

#[derive(Debug)]
struct DerivePaths {
    getevent_jsonl: PathBuf,
    ime_jsonl: PathBuf,
    touch_gestures_output: PathBuf,
    dismissals_output: PathBuf,
}

#[derive(Debug, Deserialize)]
struct BundledDerivationPolicyFile {
    schema: String,
    id: String,
    edge_ratio_ppm: i64,
    edge_inward_ratio_ppm: i64,
    max_edge_vertical_drift_ratio_ppm: i64,
    tap_max_distance_px: i64,
    tap_max_duration_ms: i64,
    hide_correlation_window_ms: i64,
}

/// Load the bundled default derivation policy.
pub fn default_derivation_policy() -> DeriveResult<DismissalDerivationPolicy> {
    let policy_file =
        serde_json::from_str::<BundledDerivationPolicyFile>(DEFAULT_DERIVATION_POLICY_JSON)?;
    if policy_file.schema != DERIVATION_POLICY_SCHEMA {
        return Err(DeriveError::new(format!(
            "unsupported derivation policy schema {}; expected {DERIVATION_POLICY_SCHEMA}",
            policy_file.schema
        )));
    }
    if policy_file.id.trim().is_empty() {
        return Err(DeriveError::new("derivation policy id must not be empty"));
    }
    let policy = DismissalDerivationPolicy {
        edge_ratio_ppm: policy_file.edge_ratio_ppm,
        edge_inward_ratio_ppm: policy_file.edge_inward_ratio_ppm,
        max_edge_vertical_drift_ratio_ppm: policy_file.max_edge_vertical_drift_ratio_ppm,
        tap_max_distance_px: policy_file.tap_max_distance_px,
        tap_max_duration_ms: policy_file.tap_max_duration_ms,
        hide_correlation_window_ms: policy_file.hide_correlation_window_ms,
    };
    validate_policy(policy)?;
    Ok(policy)
}

impl DerivePaths {
    fn from_config(config: &DeriveDismissalsConfig) -> DeriveResult<Self> {
        let getevent_jsonl = config
            .getevent_jsonl
            .clone()
            .unwrap_or_else(|| config.recording_dir.join("adb").join("getevent.jsonl"));
        let ime_jsonl = match config.ime_jsonl.clone() {
            Some(path) => path,
            None => find_ime_jsonl(&config.recording_dir)?,
        };
        let touch_gestures_output = config.touch_gestures_output.clone().unwrap_or_else(|| {
            config
                .recording_dir
                .join("derived")
                .join("touch_gestures.jsonl")
        });
        let dismissals_output = config.dismissals_output.clone().unwrap_or_else(|| {
            config
                .recording_dir
                .join("derived")
                .join("dismissal_inferences.jsonl")
        });
        Ok(Self {
            getevent_jsonl,
            ime_jsonl,
            touch_gestures_output,
            dismissals_output,
        })
    }
}

/// Derive touch gesture and dismissal inference JSONL outputs.
pub fn derive_dismissals(config: &DeriveDismissalsConfig) -> DeriveResult<Value> {
    let paths = DerivePaths::from_config(config)?;
    validate_screen(config.screen)?;
    validate_policy(config.policy)?;
    let getevent_records = read_jsonl(&paths.getevent_jsonl)?;
    let ime_records = read_jsonl(&paths.ime_jsonl)?;
    let run_context = RunContext::from_records(&config.recording_dir, &ime_records)?;
    let gestures = derive_touch_gestures(&getevent_records, config.screen, config.policy)?;
    let ime_events = read_ime_events(&ime_records);
    let dismissals =
        derive_dismissal_inferences(&gestures, &ime_events, &run_context, config.policy);
    let gesture_records = gestures
        .iter()
        .map(|gesture| gesture.to_json(&run_context, config.screen, config.policy_summary.as_ref()))
        .collect::<Vec<_>>();
    let dismissal_records = dismissals
        .iter()
        .map(|dismissal| dismissal.to_json(config.policy_summary.as_ref()))
        .collect::<Vec<_>>();
    write_jsonl(&paths.touch_gestures_output, &gesture_records)?;
    write_jsonl(&paths.dismissals_output, &dismissal_records)?;
    Ok(json!({
        "ok": true,
        "schema": DERIVATION_SUMMARY_SCHEMA,
        "recording_dir": path_text(&config.recording_dir),
        "getevent_jsonl": path_text(&paths.getevent_jsonl),
        "ime_jsonl": path_text(&paths.ime_jsonl),
        "touch_gestures_output": path_text(&paths.touch_gestures_output),
        "dismissals_output": path_text(&paths.dismissals_output),
        "screen": screen_json(config.screen),
        "policy": policy_json(config.policy),
        "derivation_policy": config.policy_summary,
        "external_run_id": run_context.external_run_id,
        "package_name": run_context.package_name,
        "touch_gesture_count": gesture_records.len(),
        "dismissal_inference_count": dismissal_records.len(),
    }))
}

impl RunContext {
    fn from_records(run_dir: &Path, records: &[Value]) -> DeriveResult<Self> {
        let manifest = read_manifest(run_dir)?;
        let mut external_run_id = string_at(&manifest, "/external_run_id");
        let mut package_name = string_at(&manifest, "/package_name");
        let mut session_id = None;
        for record in records {
            if external_run_id.is_none() {
                external_run_id = string_field(record, "external_run_id");
            }
            if package_name.is_none() {
                package_name = string_field(record, "package_name")
                    .or_else(|| string_field(record, "target_package"));
            }
            if session_id.is_none() {
                session_id = string_field(record, "session_id");
            }
        }
        Ok(Self {
            external_run_id,
            package_name,
            session_id,
        })
    }
}

pub(crate) fn find_ime_jsonl(run_dir: &Path) -> DeriveResult<PathBuf> {
    let ime_dir = run_dir.join("ime");
    let mut session_files = Vec::new();
    for entry_result in fs::read_dir(&ime_dir)? {
        let entry = entry_result?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        let is_jsonl = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
        if file_name.starts_with("session-") && is_jsonl {
            session_files.push(path);
        }
    }
    session_files.sort();
    if session_files.is_empty() {
        return Err(DeriveError::new(format!(
            "no session JSONL found under {}",
            ime_dir.display()
        )));
    }
    if session_files.len() > 1 {
        return Err(DeriveError::new(format!(
            "multiple session JSONL files found under {}; pass --ime-jsonl",
            ime_dir.display()
        )));
    }
    session_files
        .first()
        .cloned()
        .ok_or_else(|| DeriveError::new("session JSONL selection failed"))
}

fn validate_screen(screen: ScreenConfig) -> DeriveResult<()> {
    if screen.width <= 1 {
        return Err(DeriveError::new("screen width must be greater than 1"));
    }
    if screen.height <= 1 {
        return Err(DeriveError::new("screen height must be greater than 1"));
    }
    if let Some(keyboard_top_y) = screen.keyboard_top_y {
        if keyboard_top_y < 0 || keyboard_top_y > screen.height {
            return Err(DeriveError::new(
                "keyboard top y must be within the screen coordinate frame",
            ));
        }
    }
    Ok(())
}

fn validate_policy(policy: DismissalDerivationPolicy) -> DeriveResult<()> {
    validate_ppm("edge ratio", policy.edge_ratio_ppm)?;
    validate_ppm("edge inward ratio", policy.edge_inward_ratio_ppm)?;
    validate_ppm(
        "max edge vertical drift ratio",
        policy.max_edge_vertical_drift_ratio_ppm,
    )?;
    validate_non_negative("tap max distance", policy.tap_max_distance_px)?;
    validate_non_negative("tap max duration", policy.tap_max_duration_ms)?;
    validate_non_negative("hide correlation window", policy.hide_correlation_window_ms)
}

fn validate_ppm(name: &str, value: i64) -> DeriveResult<()> {
    if !(0..=1_000_000).contains(&value) {
        return Err(DeriveError::new(format!("{name} must be in 0..1000000")));
    }
    Ok(())
}

fn validate_non_negative(name: &str, value: i64) -> DeriveResult<()> {
    if value < 0 {
        return Err(DeriveError::new(format!("{name} must be non-negative")));
    }
    Ok(())
}

pub(crate) fn read_manifest(run_dir: &Path) -> DeriveResult<Value> {
    let manifest_path = run_dir.join("manifest.json");
    if manifest_path.exists() {
        let text = fs::read_to_string(manifest_path)?;
        return Ok(serde_json::from_str(&text)?);
    }
    Ok(Value::Null)
}

fn read_ime_events(records: &[Value]) -> Vec<ImeEvent> {
    records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            let line_index = u64::try_from(index).ok()?.checked_add(1)?;
            let event = string_field(record, "event")?;
            let t_uptime_ms = record.get("t_uptime_ms").and_then(Value::as_i64)?;
            Some(ImeEvent {
                line_index,
                event,
                t_uptime_ms,
                target_package: string_field(record, "target_package"),
            })
        })
        .collect()
}

pub(crate) fn required_string(record: &Value, field: &str) -> DeriveResult<String> {
    string_field(record, field)
        .ok_or_else(|| DeriveError::new(format!("missing string field {field}")))
}

pub(crate) fn string_field(record: &Value, field: &str) -> Option<String> {
    record
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub(crate) fn string_at(record: &Value, pointer: &str) -> Option<String> {
    record
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub(crate) fn required_i64(record: &Value, field: &str) -> DeriveResult<i64> {
    record
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| DeriveError::new(format!("missing integer field {field}")))
}

pub(crate) fn required_u64(record: &Value, field: &str) -> DeriveResult<u64> {
    record
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| DeriveError::new(format!("missing unsigned integer field {field}")))
}

pub(crate) fn micros_between(start_us: i64, end_us: i64) -> DeriveResult<i64> {
    end_us
        .checked_sub(start_us)
        .ok_or_else(|| DeriveError::new("negative gesture duration"))
}

pub(crate) const fn us_to_ms_floor(value: i64) -> i64 {
    value.div_euclid(1_000)
}

pub(crate) const fn coordinate_max(size: i64) -> i64 {
    size.saturating_sub(1)
}

pub(crate) fn squared_distance(dx: i64, dy: i64) -> DeriveResult<i64> {
    let dx2 = dx
        .checked_mul(dx)
        .ok_or_else(|| DeriveError::new("distance overflow"))?;
    let dy2 = dy
        .checked_mul(dy)
        .ok_or_else(|| DeriveError::new("distance overflow"))?;
    dx2.checked_add(dy2)
        .ok_or_else(|| DeriveError::new("distance overflow"))
}

pub(crate) const fn ratio_value(numerator: i64, denominator: i64) -> RatioValue {
    RatioValue {
        numerator,
        denominator,
        decimal_ppm: numerator.saturating_mul(1_000_000).div_euclid(denominator),
    }
}

pub(crate) const fn confidence_value(confidence_ppm: i64) -> RatioValue {
    RatioValue {
        numerator: confidence_ppm,
        denominator: 1_000_000,
        decimal_ppm: confidence_ppm,
    }
}

pub(crate) fn path_text(path: &Path) -> String {
    path.display().to_string()
}

fn screen_json(screen: ScreenConfig) -> Value {
    json!({
        "width_px": screen.width,
        "height_px": screen.height,
        "keyboard_top_y_px": screen.keyboard_top_y,
    })
}

fn policy_json(policy: DismissalDerivationPolicy) -> Value {
    json!({
        "edge_ratio_ppm": policy.edge_ratio_ppm,
        "edge_inward_ratio_ppm": policy.edge_inward_ratio_ppm,
        "max_edge_vertical_drift_ratio_ppm": policy.max_edge_vertical_drift_ratio_ppm,
        "tap_max_distance_px": policy.tap_max_distance_px,
        "tap_max_duration_ms": policy.tap_max_duration_ms,
        "hide_correlation_window_ms": policy.hide_correlation_window_ms,
    })
}
