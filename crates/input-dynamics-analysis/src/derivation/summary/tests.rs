use std::error::Error;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use proptest::{prop_assert, prop_assert_eq, proptest};
use serde_json::{Value, json};

use super::{DeriveRunSummaryConfig, I64Stats, derive_run_summary};

type TestResult<T> = Result<T, Box<dyn Error>>;

#[test]
fn derives_run_summary_from_press_summaries() {
    let root = unique_temp_dir("run-summary-complete");
    let Some(()) = assert_ok(create_fixture(&root), "create fixture") else {
        return;
    };

    let Some(command_summary) = assert_ok(
        derive_run_summary(&DeriveRunSummaryConfig {
            recording_dir: root.clone(),
            press_summaries_jsonl: None,
            output: None,
        }),
        "derive run summary",
    ) else {
        return;
    };

    assert_eq!(
        command_summary
            .get("press_summary_count")
            .and_then(Value::as_u64),
        Some(3_u64),
        "command summary should report source rows"
    );
    let output_path = root.join("derived").join("run_summary.json");
    let Some(summary) = assert_ok(read_json(&output_path), "read run summary") else {
        return;
    };
    assert_summary_identity(&summary);
    assert_summary_counts(&summary);
    assert_summary_timing_and_pointer(&summary);
    assert_summary_provenance_and_source(&summary);
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "remove fixture");
}

fn assert_summary_identity(summary: &Value) {
    assert_eq!(
        summary.get("schema").and_then(Value::as_str),
        Some("input_dynamics_run_summary.v1"),
        "schema should identify run summaries"
    );
}

fn assert_summary_counts(summary: &Value) {
    assert_eq!(
        summary.pointer("/counts/presses").and_then(Value::as_u64),
        Some(3_u64),
        "press count should match source rows"
    );
    assert_eq!(
        summary.pointer("/counts/deletes").and_then(Value::as_u64),
        Some(1_u64),
        "semantic delete keys should be counted"
    );
    assert_eq!(
        summary.pointer("/counts/spaces").and_then(Value::as_u64),
        Some(1_u64),
        "semantic space keys should be counted"
    );
    assert_eq!(
        summary
            .pointer("/target_packages/example.app")
            .and_then(Value::as_u64),
        Some(3_u64),
        "target package coverage should be counted"
    );
}

fn assert_summary_timing_and_pointer(summary: &Value) {
    assert_eq!(
        summary
            .pointer("/timing/hold_ms/mean_fraction/sum")
            .and_then(Value::as_i64),
        Some(150_i64),
        "hold timing should preserve integer aggregate sum"
    );
    assert_eq!(
        summary
            .pointer("/timing/pause_buckets/under_100_ms")
            .and_then(Value::as_u64),
        Some(1_u64),
        "short flight pauses should be bucketed"
    );
    assert_eq!(
        summary
            .pointer("/pointer/pressure/count")
            .and_then(Value::as_u64),
        Some(12_u64),
        "pointer pressure count should aggregate source sample counts"
    );
}

fn assert_summary_provenance_and_source(summary: &Value) {
    assert_eq!(
        summary
            .pointer("/provenance/input_actor")
            .and_then(Value::as_str),
        Some("human"),
        "session provenance should come from manifest"
    );
    assert_eq!(
        summary
            .pointer("/source_ref/fingerprint/sha256")
            .and_then(Value::as_str)
            .map(|value| value.starts_with("sha256:")),
        Some(true),
        "source fingerprint should be embedded"
    );
}

proptest! {
    #[test]
    fn integer_stats_preserve_count_and_sum(
        values in proptest::collection::vec(0_i64..1000_i64, 0_usize..64_usize),
    ) {
        let mut stats = I64Stats::default();
        let mut expected_count = 0_u64;
        let mut expected_sum = 0_i64;
        for value in values {
            let push_result = stats.push(value);
            prop_assert!(push_result.is_ok(), "stat push should not fail");
            let Some(next_count) = expected_count.checked_add(1_u64) else {
                prop_assert!(false, "expected count overflow");
                return Ok(());
            };
            let Some(next_sum) = expected_sum.checked_add(value) else {
                prop_assert!(false, "expected sum overflow");
                return Ok(());
            };
            expected_count = next_count;
            expected_sum = next_sum;
        }
        prop_assert_eq!(stats.count, expected_count);
        prop_assert_eq!(stats.sum, expected_sum);
    }
}

fn create_fixture(root: &Path) -> TestResult<()> {
    let derived_dir = root.join("derived");
    fs::create_dir_all(&derived_dir)?;
    write_json(
        &root.join("manifest.json"),
        &json!({
            "external_run_id": "run-test",
            "package_name": "org.inputdynamics.ime.debug",
            "input_actor": "human",
            "input_controller": null,
            "input_backend": null,
            "input_cadence_policy": "manual",
            "input_controller_runtime": {
                "enabled": false,
                "requested": false,
                "summary": null,
            },
            "host_start_wall_ms": 10_i64,
            "host_stop_wall_ms": 20_i64,
        }),
    )?;
    write_jsonl(
        &derived_dir.join("press_summaries.jsonl"),
        &[
            press_summary(1_i64, "letter", 40_i64, None, 4_i64),
            press_summary(2_i64, "space", 50_i64, Some(80_i64), 3_i64),
            press_summary(3_i64, "delete", 60_i64, Some(300_i64), 5_i64),
        ],
    )?;
    Ok(())
}

fn press_summary(
    press_id: i64,
    key_class: &str,
    hold_ms: i64,
    flight_ms: Option<i64>,
    sample_count: i64,
) -> Value {
    json!({
        "schema": "input_dynamics_press_summary.v1",
        "event": "press_summary",
        "press_id": press_id,
        "external_run_id": "run-test",
        "session_id": "session-test",
        "package_name": "org.inputdynamics.ime.debug",
        "target_package": "example.app",
        "password_field": false,
        "clock_alignment": {
            "getevent": "not_estimated",
        },
        "timing": {
            "hold_ms": hold_ms,
            "flight_since_previous_commit_ms": flight_ms,
            "down_to_commit_ms": hold_ms,
            "pointer_duration_ms": hold_ms,
        },
        "key": {
            "class": key_class,
            "landing": {
                "key_center_offset_x_px": press_id,
                "key_center_offset_y_px": press_id,
                "key_touch_x_ratio": 0.5,
                "key_touch_y_ratio": 0.6,
            },
        },
        "key_events": {
            "repeat_count": 0_i64,
            "long_press_count": 0_i64,
            "cancel_count": 0_i64,
        },
        "pointer": {
            "sample_count": sample_count,
            "current_sample_count": sample_count,
            "historical_sample_count": 0_i64,
            "movement": {
                "path_length_px": press_id,
                "max_distance_from_start_px": press_id,
            },
            "pressure": {
                "count": sample_count,
                "first": 0.5,
                "last": 0.6,
                "min": 0.5,
                "max": 0.6,
            },
            "size": {
                "count": sample_count,
                "first": 0.1,
                "last": 0.2,
                "min": 0.1,
                "max": 0.2,
            },
            "touch_major_px": {
                "count": sample_count,
                "min": 10_i64,
                "max": 20_i64,
                "sum": 100_i64,
            },
            "touch_minor_px": {
                "count": sample_count,
                "min": 4_i64,
                "max": 8_i64,
                "sum": 50_i64,
            },
        },
        "quality": {
            "has_key_down": true,
            "has_key_up": true,
            "has_key_commit": true,
            "has_pointer_samples": true,
        },
    })
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_millis());
    std::env::temp_dir().join(format!("input-dynamics-{prefix}-{millis}"))
}

fn read_json(path: &Path) -> TestResult<Value> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
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
