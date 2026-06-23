//! Dismissal inference from derived touch gestures and IME lifecycle events.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::clock::{AlignmentStatus, ClockDomain};
use crate::derivation::touch::{EdgeSide, GestureKind, TouchGesture};
use crate::derivation::{
    DISMISSAL_INFERENCE_SCHEMA, DismissalDerivationPolicy, ImeEvent, RunContext, confidence_value,
    us_to_ms_floor,
};

const TIME_DELTA_STATUS: &str = "legacy_mixed_clock_heuristic";

#[derive(Clone, Debug)]
pub(crate) struct DismissalInference {
    id: String,
    inferred: DismissalKind,
    confidence_ppm: i64,
    gesture: Option<TouchGesture>,
    ime_event: ImeEvent,
    delta_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
enum DismissalKind {
    SystemBackEdgeGesture,
    OutsideKeyboardTap,
    FocusOrAppHideUnknown,
    Unknown,
}

impl DismissalKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SystemBackEdgeGesture => "system_back_edge_gesture",
            Self::OutsideKeyboardTap => "outside_keyboard_tap",
            Self::FocusOrAppHideUnknown => "focus_or_app_hide_unknown",
            Self::Unknown => "unknown",
        }
    }
}

impl DismissalInference {
    pub(crate) fn to_json(&self, policy_summary: Option<&Value>) -> Value {
        let mut evidence = Vec::new();
        if let Some(gesture) = self.gesture.as_ref() {
            evidence.push(json!({
                "kind": "getevent_gesture",
                "gesture_id": gesture.id,
                "event_path": gesture.event_path,
                "touch_sequence_index": gesture.touch_sequence_index,
                "classification": gesture.classification.kind.as_str(),
                "edge_side": gesture.classification.edge_side.map(EdgeSide::as_str),
                "end_t_getevent_ms": us_to_ms_floor(gesture.end.t_getevent_us),
            }));
        }
        evidence.push(json!({
            "kind": "ime_event",
            "event": self.ime_event.event,
            "line_index": self.ime_event.line_index,
            "t_uptime_ms": self.ime_event.t_uptime_ms,
            "target_package": self.ime_event.target_package,
        }));
        json!({
            "schema": DISMISSAL_INFERENCE_SCHEMA,
            "event": "dismissal_inference",
            "inference_id": self.id,
            "inferred_dismissal": self.inferred.as_str(),
            "confidence": confidence_value(self.confidence_ppm),
            "time_delta_ms": self.delta_ms,
            "time_delta_status": TIME_DELTA_STATUS,
            "clock_alignment_status": AlignmentStatus::UnsupportedClockDomain.as_str(),
            "clock_alignment": {
                "status": AlignmentStatus::UnsupportedClockDomain.as_str(),
                "ime_event_clock_domain": ClockDomain::AndroidUptimeMs.as_str(),
                "getevent_gesture_clock_domain": ClockDomain::KernelGeteventUs.as_str(),
                "reason": "dismissal inference currently matches IME lifecycle uptime to raw getevent time without a validated alignment transform",
            },
            "observed_ime_event": self.ime_event.event,
            "target_package": self.ime_event.target_package,
            "derivation_policy": policy_summary,
            "evidence": evidence,
        })
    }
}

pub(crate) fn derive_dismissal_inferences(
    gestures: &[TouchGesture],
    ime_events: &[ImeEvent],
    context: &RunContext,
    policy: DismissalDerivationPolicy,
) -> Vec<DismissalInference> {
    let hide_events = ime_events
        .iter()
        .filter(|event| event.event == "ime_hide_window_called")
        .collect::<Vec<_>>();
    let mut used_gestures = BTreeSet::new();
    let mut inferences = Vec::new();
    for (index, hide_event) in hide_events.iter().enumerate() {
        let maybe_gesture = nearest_preceding_gesture(gestures, hide_event, &used_gestures, policy);
        if let Some(gesture) = maybe_gesture {
            used_gestures.insert(gesture.id.clone());
            let delta_ms = hide_event
                .t_uptime_ms
                .checked_sub(us_to_ms_floor(gesture.end.t_getevent_us));
            let (inferred, confidence_ppm) = dismissal_for_gesture(gesture);
            inferences.push(DismissalInference {
                id: inference_id(context, index),
                inferred,
                confidence_ppm,
                gesture: Some(gesture.clone()),
                ime_event: (*hide_event).clone(),
                delta_ms,
            });
        } else {
            inferences.push(DismissalInference {
                id: inference_id(context, index),
                inferred: DismissalKind::Unknown,
                confidence_ppm: 250_000,
                gesture: None,
                ime_event: (*hide_event).clone(),
                delta_ms: None,
            });
        }
    }
    inferences
}

fn nearest_preceding_gesture<'a>(
    gestures: &'a [TouchGesture],
    hide_event: &ImeEvent,
    used_gestures: &BTreeSet<String>,
    policy: DismissalDerivationPolicy,
) -> Option<&'a TouchGesture> {
    gestures
        .iter()
        .filter(|gesture| !used_gestures.contains(&gesture.id))
        .filter_map(|gesture| {
            let end_ms = us_to_ms_floor(gesture.end.t_getevent_us);
            let delta = hide_event.t_uptime_ms.checked_sub(end_ms)?;
            (0..=policy.hide_correlation_window_ms)
                .contains(&delta)
                .then_some((delta, gesture))
        })
        .min_by_key(|item| item.0)
        .map(|(_delta, gesture)| gesture)
}

const fn dismissal_for_gesture(gesture: &TouchGesture) -> (DismissalKind, i64) {
    match gesture.classification.kind {
        GestureKind::ScreenEdgeInwardSwipe => (DismissalKind::SystemBackEdgeGesture, 900_000),
        GestureKind::OutsideKeyboardTap => (DismissalKind::OutsideKeyboardTap, 750_000),
        GestureKind::KeyboardAreaTouch
        | GestureKind::TapUnknownArea
        | GestureKind::UnknownTouch => (DismissalKind::FocusOrAppHideUnknown, 400_000),
    }
}

fn inference_id(context: &RunContext, index: usize) -> String {
    let run = context.external_run_id.as_deref().unwrap_or("unknown-run");
    format!("dismissal:{run}:{index}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::derivation::dismissal::derive_dismissal_inferences;
    use crate::derivation::touch::{GestureKind, derive_touch_gestures};
    use crate::derivation::{ImeEvent, RunContext, ScreenConfig, default_derivation_policy};

    #[test]
    fn edge_gesture_infers_system_back_dismissal() {
        let records = vec![
            touch_frame(FrameFixture::new(1, 100_000_000, 1430, 1500, 7).started()),
            touch_frame(FrameFixture::new(2, 100_080_000, 850, 1600, 7).ended()),
        ];
        let screen = ScreenConfig {
            width: 1440,
            height: 3120,
            keyboard_top_y: Some(2200),
        };
        let policy_result = default_derivation_policy();
        assert!(policy_result.is_ok(), "default policy should load");
        let Ok(policy) = policy_result else {
            return;
        };
        let gesture_result = derive_touch_gestures(&records, screen, policy);
        assert!(gesture_result.is_ok(), "gestures should derive");
        let Ok(gestures) = gesture_result else {
            return;
        };
        assert_eq!(gestures.len(), 1, "one gesture");
        assert_eq!(
            gestures.first().map(|gesture| gesture.classification.kind),
            Some(GestureKind::ScreenEdgeInwardSwipe),
            "right-edge swipe should classify"
        );
        let ime_events = vec![ImeEvent {
            line_index: 1,
            event: String::from("ime_hide_window_called"),
            t_uptime_ms: 100_300,
            target_package: None,
        }];
        let context = RunContext {
            external_run_id: Some(String::from("run-test")),
            package_name: None,
            session_id: None,
        };
        let inferences = derive_dismissal_inferences(&gestures, &ime_events, &context, policy);

        assert_eq!(inferences.len(), 1, "one inference");
        assert_eq!(
            inferences
                .first()
                .map(|inference| inference.inferred.as_str()),
            Some("system_back_edge_gesture"),
            "edge gesture should infer system back"
        );
        let inference_json = inferences.first().map(|inference| inference.to_json(None));
        assert_eq!(
            inference_json
                .as_ref()
                .and_then(|record| record.pointer("/clock_alignment_status"))
                .and_then(serde_json::Value::as_str),
            Some("unsupported_clock_domain"),
            "dismissal correlation should not claim canonical clock alignment"
        );
        assert_eq!(
            inference_json
                .as_ref()
                .and_then(|record| record.pointer("/time_delta_status"))
                .and_then(serde_json::Value::as_str),
            Some("legacy_mixed_clock_heuristic"),
            "mixed-domain time deltas should be labeled as legacy heuristic"
        );
    }

    #[derive(Clone, Copy)]
    struct FrameFixture {
        line_index: u64,
        t_getevent_us: i64,
        x: i64,
        y: i64,
        sequence_index: i64,
        phase: FramePhase,
    }

    #[derive(Clone, Copy)]
    enum FramePhase {
        Started,
        Ended,
    }

    impl FrameFixture {
        const fn new(
            line_index: u64,
            t_getevent_us: i64,
            x: i64,
            y: i64,
            sequence_index: i64,
        ) -> Self {
            Self {
                line_index,
                t_getevent_us,
                x,
                y,
                sequence_index,
                phase: FramePhase::Started,
            }
        }

        const fn started(mut self) -> Self {
            self.phase = FramePhase::Started;
            self
        }

        const fn ended(mut self) -> Self {
            self.phase = FramePhase::Ended;
            self
        }

        const fn is_ended(self) -> bool {
            matches!(self.phase, FramePhase::Ended)
        }
    }

    fn touch_frame(frame: FrameFixture) -> serde_json::Value {
        let ended = frame.is_ended();
        json!({
            "event": "touch_frame",
            "line_index": frame.line_index,
            "event_path": "/dev/input/event3",
            "t_getevent_us": frame.t_getevent_us,
            "active_touch_count": i32::from(!ended),
            "slots": [{
                "slot": 0,
                "touch_active": !ended,
                "touch_started": matches!(frame.phase, FramePhase::Started),
                "touch_ended": ended,
                "touch_sequence_index": frame.sequence_index,
                "tracking_id": 100,
                "x": frame.x,
                "y": frame.y,
                "pressure": if ended { 0 } else { 40 },
                "touch_major": 120,
                "touch_minor": 80,
                "orientation": 0
            }]
        })
    }
}
