use std::error::Error;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use proptest::prelude::Just;
use proptest::{prop_assert, prop_assert_eq, proptest};
use serde_json::{Value, json};

use super::{DerivePressesConfig, Point, derive_press_summaries, path_metrics_for_points};
use crate::derivation::jsonl::read_jsonl;

type TestResult<T> = Result<T, Box<dyn Error>>;

#[test]
fn derives_press_summary_with_timings_and_pointer_stats() {
    let root = unique_temp_dir("press-summary-complete");
    let Some(()) = assert_ok(create_fixture(&root), "create fixture") else {
        return;
    };

    let Some(summary) = assert_ok(
        derive_press_summaries(&DerivePressesConfig {
            recording_dir: root.clone(),
            ime_jsonl: None,
            output: None,
        }),
        "derive press summaries",
    ) else {
        return;
    };

    assert_eq!(
        summary.get("press_summary_count").and_then(Value::as_u64),
        Some(2_u64),
        "two non-password presses should be summarized"
    );
    assert_eq!(
        summary
            .get("skipped_password_press_count")
            .and_then(Value::as_u64),
        Some(1_u64),
        "password press is skipped"
    );
    let output_path = root.join("derived").join("press_summaries.jsonl");
    let Some(records) = assert_ok(read_jsonl(&output_path), "read summaries") else {
        return;
    };
    let Some(first) = records.first() else {
        assert_eq!(records.len(), 2_usize, "summary records should exist");
        return;
    };
    assert_eq!(
        first.get("schema").and_then(Value::as_str),
        Some("input_dynamics_press_summary.v1"),
        "schema should identify press summaries"
    );
    assert_eq!(
        first.pointer("/timing/hold_ms").and_then(Value::as_i64),
        Some(40_i64),
        "hold time should use key event timestamps"
    );
    assert_eq!(
        first
            .pointer("/pointer/movement/path_length_px")
            .and_then(Value::as_i64),
        Some(10_i64),
        "path length should summarize pointer movement"
    );
    let Some(second) = records.get(1) else {
        assert_eq!(records.len(), 2_usize, "second summary should exist");
        return;
    };
    assert_eq!(
        second
            .pointer("/timing/flight_since_previous_commit_ms")
            .and_then(Value::as_i64),
        Some(30_i64),
        "flight should compare current down to previous commit"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
}

proptest! {
    #[test]
    fn path_length_is_never_shorter_than_straight_distance(
        first_x in 0_i64..1000_i64,
        first_y in 0_i64..1000_i64,
        mid_dx in -100_i64..100_i64,
        mid_dy in -100_i64..100_i64,
        end_dx in -100_i64..100_i64,
        end_dy in -100_i64..100_i64,
        _guard in Just(()),
    ) {
        let first = Point { x: first_x, y: first_y };
        let mid = Point {
            x: first_x.saturating_add(mid_dx),
            y: first_y.saturating_add(mid_dy),
        };
        let end = Point {
            x: mid.x.saturating_add(end_dx),
            y: mid.y.saturating_add(end_dy),
        };
        let metrics = path_metrics_for_points(&[first, mid, end]);
        prop_assert!(metrics.is_ok(), "path metrics should derive");
        let Ok(Some(stats)) = metrics else {
            prop_assert_eq!(0_i64, 1_i64, "path metrics should exist");
            return Ok(());
        };
        prop_assert!(
            stats.path_length.saturating_add(2_i64) >= stats.straight_distance,
            "floored integer path length should stay within one pixel per segment"
        );
    }
}

fn create_fixture(root: &Path) -> TestResult<()> {
    let ime_dir = root.join("ime");
    fs::create_dir_all(&ime_dir)?;
    write_json(
        &root.join("manifest.json"),
        &json!({
            "external_run_id": "run-test",
            "package_name": "org.inputdynamics.ime.debug",
        }),
    )?;
    write_jsonl(&ime_dir.join("session-test.jsonl"), &fixture_records())?;
    Ok(())
}

fn fixture_records() -> Vec<Value> {
    vec![
        json!({
            "schema": "input_dynamics_event.v1",
            "event": "session_start",
            "session_id": "session-test",
            "external_run_id": "run-test",
            "t_uptime_ms": 1_i64,
        }),
        PointerFixture::new(1, 10, "down", 100, 200).to_json(),
        PointerFixture::new(1, 30, "move", 106, 208).to_json(),
        KeyFixture::new("key_down", 1, 10, 97).label("a").to_json(),
        KeyFixture::new("key_up", 1, 50, 97).label("a").to_json(),
        KeyFixture::new("key_commit", 1, 60, 97)
            .label("a")
            .to_json(),
        PointerFixture::new(2, 90, "down", 300, 400).to_json(),
        KeyFixture::new("key_down", 2, 90, -7).to_json(),
        KeyFixture::new("key_commit", 2, 110, -7).to_json(),
        PointerFixture::new(3, 120, "down", 10, 20)
            .password()
            .to_json(),
        KeyFixture::new("key_down", 3, 120, 120)
            .label("x")
            .password()
            .to_json(),
        json!({
            "schema": "input_dynamics_event.v1",
            "event": "session_stop",
            "session_id": "session-test",
            "external_run_id": "run-test",
            "t_uptime_ms": 200_i64,
        }),
    ]
}

#[derive(Clone, Copy)]
enum FieldPrivacy {
    NonPassword,
    Password,
}

struct PointerFixture {
    press_id: i64,
    t_event_uptime_ms: i64,
    action_name: &'static str,
    x_px: i64,
    y_px: i64,
    privacy: FieldPrivacy,
}

struct KeyFixture {
    event: &'static str,
    press_id: i64,
    t_event_uptime_ms: i64,
    key_code: i64,
    key_label: Value,
    privacy: FieldPrivacy,
}

impl FieldPrivacy {
    const fn is_password(self) -> bool {
        matches!(self, Self::Password)
    }
}

impl PointerFixture {
    const fn new(
        press_id: i64,
        t_event_uptime_ms: i64,
        action_name: &'static str,
        x_px: i64,
        y_px: i64,
    ) -> Self {
        Self {
            press_id,
            t_event_uptime_ms,
            action_name,
            x_px,
            y_px,
            privacy: FieldPrivacy::NonPassword,
        }
    }

    const fn password(mut self) -> Self {
        self.privacy = FieldPrivacy::Password;
        self
    }

    fn to_json(&self) -> Value {
        json!({
            "schema": "input_dynamics_event.v1",
            "event": "pointer_sample",
            "session_id": "session-test",
            "external_run_id": "run-test",
            "target_package": "example.app",
            "password_field": self.privacy.is_password(),
            "press_id": self.press_id,
            "gesture_id": self.press_id,
            "sample_kind": "current",
            "action_name": self.action_name,
            "t_uptime_ms": self.t_event_uptime_ms,
            "t_event_uptime_ms": self.t_event_uptime_ms,
            "x_px": self.x_px,
            "y_px": self.y_px,
            "x_screen_px": self.x_px,
            "y_screen_px": self.y_px,
            "pressure": 0.5,
            "size": 0.04,
            "touch_major_px": 12_i64,
            "touch_minor_px": 8_i64,
        })
    }
}

impl KeyFixture {
    fn new(event: &'static str, press_id: i64, t_event_uptime_ms: i64, key_code: i64) -> Self {
        Self {
            event,
            press_id,
            t_event_uptime_ms,
            key_code,
            key_label: Value::Null,
            privacy: FieldPrivacy::NonPassword,
        }
    }

    fn label(mut self, label: &'static str) -> Self {
        self.key_label = json!(label);
        self
    }

    fn password(mut self) -> Self {
        self.privacy = FieldPrivacy::Password;
        self
    }

    fn to_json(&self) -> Value {
        json!({
            "schema": "input_dynamics_event.v1",
            "event": self.event,
            "session_id": "session-test",
            "external_run_id": "run-test",
            "target_package": "example.app",
            "password_field": self.privacy.is_password(),
            "press_id": self.press_id,
            "gesture_id": self.press_id,
            "t_uptime_ms": self.t_event_uptime_ms,
            "t_event_uptime_ms": self.t_event_uptime_ms,
            "x_px": 10_i64,
            "y_px": 20_i64,
            "key_code": self.key_code,
            "key_code_printable": "key",
            "key_label": self.key_label.clone(),
            "key_class": "letter",
            "key_present": true,
            "key_touch_x_ratio": 0.5,
            "key_touch_y_ratio": 0.5,
            "key_center_offset_x_px": 0_i64,
            "key_center_offset_y_px": 0_i64,
        })
    }
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

fn assert_ok<T, E>(result: Result<T, E>, label: &str) -> Option<T>
where
    E: Debug,
{
    let error = result.as_ref().err();
    assert!(error.is_none(), "{label} failed: {error:?}");
    result.ok()
}
