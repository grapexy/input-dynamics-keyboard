use std::error::Error;
use std::fmt::Debug;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::{file_fingerprint, inspect_recording};

type TestResult<T> = Result<T, Box<dyn Error>>;

#[test]
fn inspect_reports_ready_recording_and_next_timeline_action() {
    let root = unique_temp_dir("recording-inspect-ready");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/valid_for_analysis")
            .and_then(Value::as_bool),
        Some(true),
        "fixture should be valid for analysis"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_derivation")
            .and_then(Value::as_bool),
        Some(false),
        "derived touch and dismissal files are present"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_press_summaries")
            .and_then(Value::as_bool),
        Some(false),
        "press summaries are present"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_run_summary")
            .and_then(Value::as_bool),
        Some(false),
        "run summary is present and fresh"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_timeline")
            .and_then(Value::as_bool),
        Some(true),
        "timeline is missing"
    );
    assert_eq!(
        result
            .pointer("/session_jsonl/selected")
            .and_then(Value::as_str),
        Some("ime/session-test.jsonl"),
        "single session should be selected"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_selects_session_jsonl_from_pulled_log_layout() {
    let root = unique_temp_dir("recording-inspect-pulled-log-layout");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let flat_session = root.join("ime").join("session-test.jsonl");
    let nested_dir = root.join("ime").join("input_dynamics_logs");
    let nested_session = nested_dir.join("session-test.jsonl");
    let Some(()) = assert_ok(fs::create_dir_all(&nested_dir), "create nested ime dir") else {
        return;
    };
    let Some(()) = assert_ok(
        fs::rename(&flat_session, &nested_session),
        "move session jsonl",
    ) else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/session_jsonl/selected")
            .and_then(Value::as_str),
        Some("ime/input_dynamics_logs/session-test.jsonl"),
        "single pulled session should be selected recursively"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_detects_run_summary_source_staleness() {
    let root = unique_temp_dir("recording-inspect-summary-stale");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let source_path = root.join("derived").join("press_summaries.jsonl");
    let mutation_result = fs::write(
        &source_path,
        concat!(
            "{\"schema\":\"input_dynamics_press_summary.v1\",\"event\":\"press_summary\"}\n",
            "{\"schema\":\"input_dynamics_press_summary.v1\",\"event\":\"press_summary\"}\n"
        ),
    );
    let Some(()) = assert_ok(mutation_result, "mutate source") else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/needs_run_summary")
            .and_then(Value::as_bool),
        Some(true),
        "changed press summary source should stale run summary"
    );
    let stale_count = result
        .pointer("/run_summary/stale_reasons")
        .and_then(Value::as_array)
        .map_or(0_usize, Vec::len);
    assert!(
        stale_count > 0,
        "stale reasons should explain why run summary needs refresh"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_detects_timeline_source_staleness() {
    let root = unique_temp_dir("recording-inspect-stale");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::WithTimeline), "fixture") else {
        return;
    };
    let touch_path = root.join("derived").join("touch_gestures.jsonl");
    let append_result = fs::write(
        &touch_path,
        concat!(
            "{\"schema\":\"input_dynamics_touch_gesture.v1\",\"event\":\"touch_gesture\"}\n",
            "{\"schema\":\"input_dynamics_touch_gesture.v1\",\"event\":\"touch_gesture\"}\n"
        ),
    );
    let Some(()) = assert_ok(append_result, "mutate source") else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/needs_timeline")
            .and_then(Value::as_bool),
        Some(true),
        "changed source fingerprint should stale timeline"
    );
    let stale_count = result
        .pointer("/timeline/stale_reasons")
        .and_then(Value::as_array)
        .map_or(0_usize, Vec::len);
    assert!(
        stale_count > 0,
        "stale reasons should explain why timeline needs refresh"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_accepts_optional_missing_timeline_sources() {
    let root = unique_temp_dir("recording-inspect-optional-missing");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::WithTimeline), "fixture") else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/needs_timeline")
            .and_then(Value::as_bool),
        Some(false),
        "optional absent evidence sources should not stale a fresh timeline"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_requires_declared_video_artifacts() {
    let root = unique_temp_dir("recording-inspect-required-video-missing");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(()) = assert_ok(
        write_manifest(&root, Some(required_video_manifest())),
        "write manifest",
    ) else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/valid_for_analysis")
            .and_then(Value::as_bool),
        Some(false),
        "recording should not be analysis-ready when required video is missing"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_video")
            .and_then(Value::as_bool),
        Some(true),
        "required missing video should request a fresh video-backed recording"
    );
    assert_eq!(
        result.pointer("/video/required").and_then(Value::as_bool),
        Some(true),
        "inspection should surface the video requirement"
    );
    let stale_count = result
        .pointer("/video/stale_reasons")
        .and_then(Value::as_array)
        .map_or(0_usize, Vec::len);
    assert!(
        stale_count >= 2,
        "video stale reasons should mention both screen and timing artifacts"
    );
    let has_record_with_video_action = result
        .get("next_actions")
        .and_then(Value::as_array)
        .is_some_and(|actions| {
            actions.iter().any(|action| {
                action.get("kind").and_then(Value::as_str) == Some("record_with_video")
            })
        });
    assert!(
        has_record_with_video_action,
        "inspect should give a canonical rerun action for missing video"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_accepts_declared_video_artifacts() {
    let root = unique_temp_dir("recording-inspect-required-video-present");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_video_fixture(&root), "create video fixture") else {
        return;
    };
    let Some(()) = assert_ok(write_manifest(&root, Some(video)), "write manifest") else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/valid_for_analysis")
            .and_then(Value::as_bool),
        Some(true),
        "recording should be analysis-ready with fresh required video artifacts"
    );
    assert_eq!(
        result.pointer("/flags/has_video").and_then(Value::as_bool),
        Some(true),
        "fresh video should be reported as present"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_video")
            .and_then(Value::as_bool),
        Some(false),
        "fresh video should not request a rerun"
    );
    assert_eq!(
        result
            .pointer("/video/stale_reasons")
            .and_then(Value::as_array)
            .map_or(0_usize, Vec::len),
        0_usize,
        "fresh video should not produce stale reasons"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[derive(Clone, Copy)]
enum FixtureShape {
    DerivedOnly,
    WithTimeline,
}

fn create_fixture(root: &Path, shape: FixtureShape) -> TestResult<()> {
    fs::create_dir_all(root.join("ime"))?;
    fs::create_dir_all(root.join("adb"))?;
    fs::create_dir_all(root.join("derived"))?;
    write_manifest(root, None)?;
    write_jsonl(&root.join("ime").join("session-test.jsonl"), &ime_records())?;
    write_jsonl(
        &root.join("adb").join("getevent.jsonl"),
        &[json!({"schema": "input_dynamics_getevent.v1", "event": "touch_frame"})],
    )?;
    fs::write(root.join("adb").join("getevent.raw.log"), "raw\n")?;
    write_jsonl(
        &root.join("derived").join("touch_gestures.jsonl"),
        &[json!({"schema": "input_dynamics_touch_gesture.v1", "event": "touch_gesture"})],
    )?;
    write_jsonl(
        &root.join("derived").join("press_summaries.jsonl"),
        &[json!({"schema": "input_dynamics_press_summary.v1", "event": "press_summary"})],
    )?;
    create_run_summary_fixture(root)?;
    write_jsonl(
        &root.join("derived").join("dismissal_inferences.jsonl"),
        &[
            json!({"schema": "input_dynamics_dismissal_inference.v1", "event": "dismissal_inference"}),
        ],
    )?;
    write_json(
        &root.join("validation.json"),
        &json!({
            "ok": true,
            "record_count": 3_u64,
            "selected_record_count": 3_u64,
            "session_start_count": 1_u64,
            "session_stop_count": 1_u64,
            "password_record_count": 0_u64,
            "target_package_seen": true,
        }),
    )?;
    if matches!(shape, FixtureShape::WithTimeline) {
        create_timeline_fixture(root)?;
    }
    Ok(())
}

fn write_manifest(root: &Path, video: Option<Value>) -> TestResult<()> {
    let mut manifest = json!({
        "schema": "input_dynamics_record_manifest.v1",
        "external_run_id": "run-test",
        "package_name": "org.inputdynamics.ime.debug",
        "input_actor": "human",
        "input_controller": null,
        "input_cadence_policy": "manual",
        "host_start_wall_ms": 1_000_i64,
        "host_stop_wall_ms": 2_000_i64,
    });
    if let Some(video_value) = video {
        let Some(object) = manifest.as_object_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "manifest fixture should be a JSON object",
            )
            .into());
        };
        object.insert(String::from("video"), video_value);
    }
    write_json(&root.join("manifest.json"), &manifest)
}

fn required_video_manifest() -> Value {
    json!({
        "schema": "input_dynamics_video_capture.v1",
        "enabled": true,
        "required": true,
        "remote_path": "/sdcard/Download/input-dynamics-run-test.mp4",
        "local_path": "video/screen.mp4",
        "timing_path": "video/timing.json",
        "file": null,
        "start": {
            "phase": "start",
            "host_wall_ms_before_device_timestamp": 1_100_i64,
            "device_epoch_ms": 1_101_i64,
            "host_wall_ms_after_device_timestamp": 1_102_i64,
        },
        "stop": {
            "phase": "stop",
            "host_wall_ms_before_device_timestamp": 1_900_i64,
            "device_epoch_ms": 1_901_i64,
            "host_wall_ms_after_device_timestamp": 1_902_i64,
        },
        "ok": true,
    })
}

fn create_video_fixture(root: &Path) -> TestResult<Value> {
    let video_dir = root.join("video");
    fs::create_dir_all(&video_dir)?;
    let screen_path = video_dir.join("screen.mp4");
    fs::write(&screen_path, "not-a-real-mp4-for-inspection\n")?;
    let mut video = required_video_manifest();
    let Some(object) = video.as_object_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "video fixture should be a JSON object",
        )
        .into());
    };
    object.insert(String::from("file"), file_fingerprint(&screen_path)?);
    write_json(&video_dir.join("timing.json"), &video)?;
    fs::write(video_dir.join("screenrecord.stdout.log"), "")?;
    fs::write(video_dir.join("screenrecord.stderr.log"), "")?;
    fs::write(video_dir.join("adb-pull-video.log"), "pulled\n")?;
    Ok(video)
}

fn create_run_summary_fixture(root: &Path) -> TestResult<()> {
    let source_path = root.join("derived").join("press_summaries.jsonl");
    let fingerprint = file_fingerprint(&source_path)?;
    write_json(
        &root.join("derived").join("run_summary.json"),
        &json!({
            "schema": "input_dynamics_run_summary.v1",
            "event": "run_summary",
            "source_ref": {
                "path": "derived/press_summaries.jsonl",
                "record_count": 1_u64,
                "fingerprint": fingerprint,
            },
        }),
    )
}

fn create_timeline_fixture(root: &Path) -> TestResult<()> {
    let timeline_dir = root.join("derived").join("timeline");
    fs::create_dir_all(&timeline_dir)?;
    let source_path = root.join("derived").join("touch_gestures.jsonl");
    let fingerprint = file_fingerprint(&source_path)?;
    write_json(
        &timeline_dir.join("index.json"),
        &json!({
            "schema": "input_dynamics_timeline_index.v1",
            "sources": [
                {
                    "kind": "derived_touch_gestures",
                    "path": "derived/touch_gestures.jsonl",
                    "exists": true,
                    "required": false,
                    "fingerprint": fingerprint
                },
                {
                    "kind": "evidence_start",
                    "path": "evidence/start/index.json",
                    "exists": false,
                    "required": false,
                    "fingerprint": null
                }
            ],
        }),
    )?;
    write_jsonl(
        &timeline_dir.join("events.jsonl"),
        &[json!({"schema": "input_dynamics_timeline_event.v1"})],
    )?;
    Ok(())
}

fn ime_records() -> Vec<Value> {
    vec![
        json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-test",
            "external_run_id": "run-test",
            "event": "session_start",
            "t_wall_ms": 1_i64,
            "t_uptime_ms": 1_i64,
            "input_actor": "human",
            "input_controller": null,
            "input_cadence_policy": "manual",
        }),
        json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-test",
            "external_run_id": "run-test",
            "event": "field_enter",
            "t_wall_ms": 2_i64,
            "t_uptime_ms": 2_i64,
            "target_package": "example.app",
            "password_field": false,
        }),
        json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-test",
            "external_run_id": "run-test",
            "event": "session_stop",
            "t_wall_ms": 3_i64,
            "t_uptime_ms": 3_i64,
        }),
    ]
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
