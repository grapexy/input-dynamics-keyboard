//! Touch gesture derivation from normalized getevent records.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::clock::{AlignmentStatus, ClockDomain, TimestampPrecision, micros_to_nanos};
use crate::derivation::{
    DeriveError, DeriveResult, DismissalDerivationPolicy, RunContext, ScreenConfig,
    TOUCH_GESTURE_SCHEMA, confidence_value, coordinate_max, micros_between, ratio_value,
    required_i64, required_string, required_u64, squared_distance, us_to_ms_floor,
};

#[derive(Clone, Debug)]
pub(crate) struct TouchGesture {
    pub(crate) id: String,
    pub(crate) event_path: String,
    pub(crate) touch_sequence_index: i64,
    tracking_id: Option<i64>,
    pub(crate) start: TouchSample,
    pub(crate) end: TouchSample,
    sample_count: usize,
    duration_us: i64,
    duration_ms: i64,
    distance_px: i64,
    dx: i64,
    dy: i64,
    pub(crate) classification: GestureClassification,
}

#[derive(Clone, Debug)]
pub(crate) struct TouchSample {
    pub(crate) t_getevent_us: i64,
    line_index: u64,
    x: i64,
    y: i64,
    pressure: Option<i64>,
    touch_major: Option<i64>,
    touch_minor: Option<i64>,
    orientation: Option<i64>,
    tracking_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GestureClassification {
    pub(crate) kind: GestureKind,
    pub(crate) edge_side: Option<EdgeSide>,
    confidence_ppm: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GestureKind {
    ScreenEdgeInwardSwipe,
    OutsideKeyboardTap,
    KeyboardAreaTouch,
    TapUnknownArea,
    UnknownTouch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EdgeSide {
    Left,
    Right,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GestureKey {
    event_path: String,
    touch_sequence_index: i64,
}

#[derive(Default)]
struct GestureBuilder {
    samples: Vec<TouchSample>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GestureMeasure {
    distance_px: i64,
    duration_ms: i64,
}

impl GestureKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ScreenEdgeInwardSwipe => "screen_edge_inward_swipe",
            Self::OutsideKeyboardTap => "outside_keyboard_tap",
            Self::KeyboardAreaTouch => "keyboard_area_touch",
            Self::TapUnknownArea => "tap_unknown_area",
            Self::UnknownTouch => "unknown_touch",
        }
    }
}

impl EdgeSide {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

impl TouchGesture {
    pub(crate) fn to_json(
        &self,
        context: &RunContext,
        screen: ScreenConfig,
        policy_summary: Option<&Value>,
    ) -> Value {
        json!({
            "schema": TOUCH_GESTURE_SCHEMA,
            "event": "touch_gesture",
            "gesture_id": self.id,
            "external_run_id": context.external_run_id,
            "session_id": context.session_id,
            "package_name": context.package_name,
            "source": "adb_getevent",
            "clock_domain": ClockDomain::KernelGeteventUs.as_str(),
            "clock_alignment_status": AlignmentStatus::UnsupportedClockDomain.as_str(),
            "time": gesture_time_json(self),
            "event_path": self.event_path,
            "touch_sequence_index": self.touch_sequence_index,
            "tracking_id": self.tracking_id,
            "classification": self.classification.kind.as_str(),
            "classification_confidence": confidence_value(self.classification.confidence_ppm),
            "edge_side": self.classification.edge_side.map(EdgeSide::as_str),
            "derivation_policy": policy_summary,
            "screen": {
                "width_px": screen.width,
                "height_px": screen.height,
                "keyboard_top_y_px": screen.keyboard_top_y,
            },
            "sample_count": self.sample_count,
            "start": sample_json(&self.start, screen),
            "end": sample_json(&self.end, screen),
            "delta": {
                "clock_domain": ClockDomain::KernelGeteventUs.as_str(),
                "source_timestamp_precision": TimestampPrecision::Microseconds.as_str(),
                "dx_px": self.dx,
                "dy_px": self.dy,
                "distance_px": self.distance_px,
                "duration_us": self.duration_us,
                "duration_ms": self.duration_ms,
            },
        })
    }
}

impl TouchSample {
    fn from_slot(slot: &Value, t_getevent_us: i64, line_index: u64) -> DeriveResult<Self> {
        Ok(Self {
            t_getevent_us,
            line_index,
            x: required_i64(slot, "x")?,
            y: required_i64(slot, "y")?,
            pressure: slot.get("pressure").and_then(Value::as_i64),
            touch_major: slot.get("touch_major").and_then(Value::as_i64),
            touch_minor: slot.get("touch_minor").and_then(Value::as_i64),
            orientation: slot.get("orientation").and_then(Value::as_i64),
            tracking_id: slot.get("tracking_id").and_then(Value::as_i64),
        })
    }
}

impl GestureBuilder {
    fn push(&mut self, sample: TouchSample) {
        self.samples.push(sample);
    }

    fn finish(
        mut self,
        key: GestureKey,
        screen: ScreenConfig,
        policy: DismissalDerivationPolicy,
    ) -> DeriveResult<Option<TouchGesture>> {
        if self.samples.is_empty() {
            return Ok(None);
        }
        self.samples.sort_by_key(|sample| sample.t_getevent_us);
        let Some(start) = self.samples.first().cloned() else {
            return Ok(None);
        };
        let Some(end) = self.samples.last().cloned() else {
            return Ok(None);
        };
        let dx = end
            .x
            .checked_sub(start.x)
            .ok_or_else(|| DeriveError::new("gesture dx overflow"))?;
        let dy = end
            .y
            .checked_sub(start.y)
            .ok_or_else(|| DeriveError::new("gesture dy overflow"))?;
        let duration_us = micros_between(start.t_getevent_us, end.t_getevent_us)?;
        let duration_ms = us_to_ms_floor(duration_us);
        let distance_px = squared_distance(dx, dy)?.isqrt();
        let classification = classify_gesture(
            &start,
            &end,
            GestureMeasure {
                distance_px,
                duration_ms,
            },
            screen,
            policy,
        )?;
        Ok(Some(TouchGesture {
            id: format!("getevent:{}:{}", key.event_path, key.touch_sequence_index),
            event_path: key.event_path,
            touch_sequence_index: key.touch_sequence_index,
            tracking_id: start.tracking_id,
            start,
            end,
            sample_count: self.samples.len(),
            duration_us,
            duration_ms,
            distance_px,
            dx,
            dy,
            classification,
        }))
    }
}

pub(crate) fn derive_touch_gestures(
    getevent_records: &[Value],
    screen: ScreenConfig,
    policy: DismissalDerivationPolicy,
) -> DeriveResult<Vec<TouchGesture>> {
    let mut builders: BTreeMap<GestureKey, GestureBuilder> = BTreeMap::new();
    for record in getevent_records {
        if record.get("event").and_then(Value::as_str) != Some("touch_frame") {
            continue;
        }
        let event_path = required_string(record, "event_path")?;
        let t_getevent_us = required_i64(record, "t_getevent_us")?;
        let line_index = required_u64(record, "line_index")?;
        let slots = record
            .get("slots")
            .and_then(Value::as_array)
            .ok_or_else(|| DeriveError::new("touch_frame missing slots array"))?;
        for slot in slots {
            let Some(sequence_index) = slot.get("touch_sequence_index").and_then(Value::as_i64)
            else {
                continue;
            };
            let key = GestureKey {
                event_path: event_path.clone(),
                touch_sequence_index: sequence_index,
            };
            let sample = TouchSample::from_slot(slot, t_getevent_us, line_index)?;
            builders.entry(key).or_default().push(sample);
        }
    }
    let mut gestures = builders
        .into_iter()
        .filter_map(|(key, builder)| builder.finish(key, screen, policy).transpose())
        .collect::<DeriveResult<Vec<_>>>()?;
    gestures.sort_by_key(|gesture| gesture.start.t_getevent_us);
    Ok(gestures)
}

fn sample_json(sample: &TouchSample, screen: ScreenConfig) -> Value {
    json!({
        "line_index": sample.line_index,
        "t_getevent_us": sample.t_getevent_us,
        "t_getevent_ms": us_to_ms_floor(sample.t_getevent_us),
        "time": touch_sample_time_json(sample),
        "x_px": sample.x,
        "y_px": sample.y,
        "x_ratio": ratio_value(sample.x, coordinate_max(screen.width)),
        "y_ratio": ratio_value(sample.y, coordinate_max(screen.height)),
        "pressure": sample.pressure,
        "touch_major": sample.touch_major,
        "touch_minor": sample.touch_minor,
        "orientation": sample.orientation,
    })
}

fn gesture_time_json(gesture: &TouchGesture) -> Value {
    json!({
        "source_clock_domain": ClockDomain::KernelGeteventUs.as_str(),
        "source_timestamp_precision": TimestampPrecision::Microseconds.as_str(),
        "source_time_interval_us": [
            gesture.start.t_getevent_us,
            gesture.end.t_getevent_us,
        ],
        "source_time_interval_ns": [
            micros_to_nanos(gesture.start.t_getevent_us),
            micros_to_nanos(gesture.end.t_getevent_us),
        ],
        "source_field": "start.t_getevent_us,end.t_getevent_us",
        "source_time_status": "derived_getevent_time",
        "duration_us": gesture.duration_us,
        "duration_ms": gesture.duration_ms,
        "normalized_clock_domain": Value::Null,
        "normalized_time_interval_ns": Value::Null,
        "alignment_status": AlignmentStatus::UnsupportedClockDomain.as_str(),
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
    })
}

fn touch_sample_time_json(sample: &TouchSample) -> Value {
    json!({
        "source_clock_domain": ClockDomain::KernelGeteventUs.as_str(),
        "source_timestamp_precision": TimestampPrecision::Microseconds.as_str(),
        "source_time_us": sample.t_getevent_us,
        "source_time_ns": micros_to_nanos(sample.t_getevent_us),
        "source_field": "t_getevent_us",
        "source_time_status": "derived_getevent_time",
        "normalized_clock_domain": Value::Null,
        "normalized_time_ns": Value::Null,
        "alignment_status": AlignmentStatus::UnsupportedClockDomain.as_str(),
        "transform_id": Value::Null,
        "uncertainty_ns": Value::Null,
    })
}

fn classify_gesture(
    start: &TouchSample,
    end: &TouchSample,
    measure: GestureMeasure,
    screen: ScreenConfig,
    policy: DismissalDerivationPolicy,
) -> DeriveResult<GestureClassification> {
    if is_right_edge_swipe(start, end, screen, policy)? {
        return Ok(GestureClassification {
            kind: GestureKind::ScreenEdgeInwardSwipe,
            edge_side: Some(EdgeSide::Right),
            confidence_ppm: 900_000,
        });
    }
    if is_left_edge_swipe(start, end, screen, policy)? {
        return Ok(GestureClassification {
            kind: GestureKind::ScreenEdgeInwardSwipe,
            edge_side: Some(EdgeSide::Left),
            confidence_ppm: 900_000,
        });
    }
    if measure.distance_px <= policy.tap_max_distance_px
        && measure.duration_ms <= policy.tap_max_duration_ms
    {
        if let Some(keyboard_top_y) = screen.keyboard_top_y {
            let kind = if start.y < keyboard_top_y {
                GestureKind::OutsideKeyboardTap
            } else {
                GestureKind::KeyboardAreaTouch
            };
            return Ok(GestureClassification {
                kind,
                edge_side: None,
                confidence_ppm: 750_000,
            });
        }
        return Ok(GestureClassification {
            kind: GestureKind::TapUnknownArea,
            edge_side: None,
            confidence_ppm: 550_000,
        });
    }
    Ok(GestureClassification {
        kind: GestureKind::UnknownTouch,
        edge_side: None,
        confidence_ppm: 300_000,
    })
}

fn is_right_edge_swipe(
    start: &TouchSample,
    end: &TouchSample,
    screen: ScreenConfig,
    policy: DismissalDerivationPolicy,
) -> DeriveResult<bool> {
    Ok(near_right_edge(start.x, screen.width, policy)?
        && inward_left(start.x, end.x, screen.width, policy)?
        && within_vertical_drift(start.y, end.y, screen.height, policy)?)
}

fn is_left_edge_swipe(
    start: &TouchSample,
    end: &TouchSample,
    screen: ScreenConfig,
    policy: DismissalDerivationPolicy,
) -> DeriveResult<bool> {
    Ok(near_left_edge(start.x, screen.width, policy)?
        && inward_right(start.x, end.x, screen.width, policy)?
        && within_vertical_drift(start.y, end.y, screen.height, policy)?)
}

fn near_right_edge(x: i64, width: i64, policy: DismissalDerivationPolicy) -> DeriveResult<bool> {
    let edge_px = ratio_apply(width, policy.edge_ratio_ppm)?;
    Ok(width
        .checked_sub(x)
        .ok_or_else(|| DeriveError::new("edge distance overflow"))?
        <= edge_px)
}

fn near_left_edge(x: i64, width: i64, policy: DismissalDerivationPolicy) -> DeriveResult<bool> {
    Ok(x <= ratio_apply(width, policy.edge_ratio_ppm)?)
}

fn inward_left(
    start_x: i64,
    end_x: i64,
    width: i64,
    policy: DismissalDerivationPolicy,
) -> DeriveResult<bool> {
    let min_delta = ratio_apply(width, policy.edge_inward_ratio_ppm)?;
    Ok(start_x
        .checked_sub(end_x)
        .ok_or_else(|| DeriveError::new("edge swipe delta overflow"))?
        >= min_delta)
}

fn inward_right(
    start_x: i64,
    end_x: i64,
    width: i64,
    policy: DismissalDerivationPolicy,
) -> DeriveResult<bool> {
    let min_delta = ratio_apply(width, policy.edge_inward_ratio_ppm)?;
    Ok(end_x
        .checked_sub(start_x)
        .ok_or_else(|| DeriveError::new("edge swipe delta overflow"))?
        >= min_delta)
}

fn within_vertical_drift(
    start_y: i64,
    end_y: i64,
    height: i64,
    policy: DismissalDerivationPolicy,
) -> DeriveResult<bool> {
    let max_drift = ratio_apply(height, policy.max_edge_vertical_drift_ratio_ppm)?;
    Ok(end_y
        .checked_sub(start_y)
        .ok_or_else(|| DeriveError::new("vertical drift overflow"))?
        .abs()
        <= max_drift)
}

fn ratio_apply(value: i64, ppm: i64) -> DeriveResult<i64> {
    value
        .checked_mul(ppm)
        .and_then(|scaled| scaled.checked_div(1_000_000))
        .ok_or_else(|| DeriveError::new("ratio arithmetic overflow"))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{Just, any};
    use proptest::prop_assert_eq;

    use crate::derivation::touch::{
        GestureKind, GestureMeasure, TouchSample, classify_gesture, derive_touch_gestures,
    };
    use crate::derivation::{RunContext, ScreenConfig, default_derivation_policy};

    fn test_screen() -> ScreenConfig {
        ScreenConfig {
            width: 1440,
            height: 3120,
            keyboard_top_y: Some(2200),
        }
    }

    #[test]
    fn top_tap_classifies_as_outside_keyboard() {
        let start = sample(1060, 194);
        let end = sample(1060, 194);
        let policy_result = default_derivation_policy();
        assert!(policy_result.is_ok(), "default policy should load");
        let Ok(policy) = policy_result else {
            return;
        };

        let classification = classify_gesture(
            &start,
            &end,
            GestureMeasure {
                distance_px: 0,
                duration_ms: 58,
            },
            test_screen(),
            policy,
        );

        assert!(classification.is_ok(), "classification should succeed");
        assert_eq!(
            classification.ok().map(|value| value.kind),
            Some(GestureKind::OutsideKeyboardTap),
            "top tap should be outside the known keyboard bounds"
        );
    }

    #[test]
    fn touch_gesture_json_exposes_kernel_getevent_clock_domain() {
        let records = vec![
            touch_frame(1_u64, 150_000_i64, 10_i64, 100_i64, 200_i64),
            touch_frame(2_u64, 190_000_i64, 10_i64, 300_i64, 200_i64),
        ];
        let policy_result = default_derivation_policy();
        assert!(policy_result.is_ok(), "default policy should load");
        let Ok(policy) = policy_result else {
            return;
        };
        let gesture_result = derive_touch_gestures(&records, test_screen(), policy);
        assert!(gesture_result.is_ok(), "gesture should derive");
        let Ok(gestures) = gesture_result else {
            return;
        };
        let Some(gesture) = gestures.first() else {
            assert_eq!(gestures.len(), 1_usize, "one gesture should derive");
            return;
        };
        let record = gesture.to_json(
            &RunContext {
                external_run_id: Some(String::from("run-test")),
                package_name: Some(String::from("org.inputdynamics.ime.debug")),
                session_id: Some(String::from("session-test")),
            },
            test_screen(),
            None,
        );
        assert_eq!(
            record
                .get("clock_domain")
                .and_then(serde_json::Value::as_str),
            Some("kernel_getevent_us"),
            "touch gesture should declare the getevent source clock"
        );
        assert_eq!(
            record
                .pointer("/time/source_clock_domain")
                .and_then(serde_json::Value::as_str),
            Some("kernel_getevent_us"),
            "gesture time should be self-describing"
        );
        assert_eq!(
            record
                .pointer("/time/alignment_status")
                .and_then(serde_json::Value::as_str),
            Some("unsupported_clock_domain"),
            "getevent gestures should not imply cross-source alignment"
        );
        assert_eq!(
            record
                .pointer("/start/time/source_time_us")
                .and_then(serde_json::Value::as_i64),
            Some(150_000_i64),
            "endpoint time should preserve raw getevent microseconds"
        );
    }

    proptest::proptest! {
        #[test]
        fn generated_right_edge_swipes_classify(
            start_y in 200_i64..2_900_i64,
            dy in -120_i64..120_i64,
            end_x in 700_i64..1_100_i64,
        ) {
            let start = sample(1430, start_y);
            let maybe_end_y = start_y.checked_add(dy);
            prop_assert_eq!(maybe_end_y.is_some(), true, "end y should not overflow");
            let Some(end_y) = maybe_end_y else {
                return Ok(());
            };
            let end = sample(end_x, end_y);

            let policy_result = default_derivation_policy();
            prop_assert_eq!(policy_result.is_ok(), true, "default policy should load");
            let Ok(policy) = policy_result else {
                return Ok(());
            };
            let classification_result = classify_gesture(
                &start,
                &end,
                GestureMeasure {
                    distance_px: 500,
                    duration_ms: 80,
                },
                test_screen(),
                policy,
            );

            prop_assert_eq!(classification_result.is_ok(), true, "classification should succeed");
            if let Ok(classification) = classification_result {
                prop_assert_eq!(
                    classification.kind,
                    GestureKind::ScreenEdgeInwardSwipe,
                    "generated right-edge gesture should classify"
                );
            }
        }

        #[test]
        fn top_taps_classify_as_outside_keyboard(
            x in 0_i64..1439_i64,
            y in 0_i64..1000_i64,
            _unit in Just(()),
        ) {
            let start = sample(x, y);
            let end = sample(x, y);

            let policy_result = default_derivation_policy();
            prop_assert_eq!(policy_result.is_ok(), true, "default policy should load");
            let Ok(policy) = policy_result else {
                return Ok(());
            };
            let classification_result = classify_gesture(
                &start,
                &end,
                GestureMeasure {
                    distance_px: 0,
                    duration_ms: 80,
                },
                test_screen(),
                policy,
            );

            prop_assert_eq!(classification_result.is_ok(), true, "classification should succeed");
            if let Ok(classification) = classification_result {
                prop_assert_eq!(
                    classification.kind,
                    GestureKind::OutsideKeyboardTap,
                    "top tap should be outside keyboard when keyboard top is known"
                );
            }
        }

        #[test]
        fn non_edge_unknown_swipes_do_not_classify_as_edge(
            start_x in 300_i64..900_i64,
            end_x in 301_i64..1000_i64,
            y in 500_i64..2_000_i64,
            _any in any::<u8>(),
        ) {
            let start = sample(start_x, y);
            let end = sample(end_x, y);

            let policy_result = default_derivation_policy();
            prop_assert_eq!(policy_result.is_ok(), true, "default policy should load");
            let Ok(policy) = policy_result else {
                return Ok(());
            };
            let classification_result = classify_gesture(
                &start,
                &end,
                GestureMeasure {
                    distance_px: 100,
                    duration_ms: 80,
                },
                test_screen(),
                policy,
            );

            prop_assert_eq!(classification_result.is_ok(), true, "classification should succeed");
            if let Ok(classification) = classification_result {
                prop_assert_eq!(
                    classification.kind == GestureKind::ScreenEdgeInwardSwipe,
                    false,
                    "middle swipe should not be screen-edge"
                );
            }
        }
    }

    fn sample(x: i64, y: i64) -> TouchSample {
        TouchSample {
            t_getevent_us: 1_000,
            line_index: 1,
            x,
            y,
            pressure: Some(10),
            touch_major: Some(20),
            touch_minor: Some(10),
            orientation: Some(0),
            tracking_id: Some(7),
        }
    }

    fn touch_frame(
        line_index: u64,
        t_getevent_us: i64,
        sequence_index: i64,
        x: i64,
        y: i64,
    ) -> serde_json::Value {
        serde_json::json!({
            "event": "touch_frame",
            "event_path": "/dev/input/event1",
            "line_index": line_index,
            "t_getevent_us": t_getevent_us,
            "slots": [{
                "touch_sequence_index": sequence_index,
                "tracking_id": sequence_index,
                "x": x,
                "y": y,
                "pressure": 20_i64,
                "touch_major": 12_i64,
                "touch_minor": 8_i64,
            }],
        })
    }
}
