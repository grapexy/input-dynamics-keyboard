use std::error::Error;
use std::fmt::Debug;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::{file_fingerprint, inspect_recording, validation_stale_reasons};

type TestResult<T> = Result<T, Box<dyn Error>>;

#[test]
fn validation_staleness_includes_clock_validation_schema() {
    let stored = json!({
        "ok": true,
        "record_count": 3_u64,
        "selected_record_count": 3_u64,
        "session_start_count": 1_u64,
        "session_stop_count": 1_u64,
        "password_record_count": 0_u64,
        "target_package_seen": true
    });
    let current = json!({
        "ok": true,
        "record_count": 3_u64,
        "selected_record_count": 3_u64,
        "session_start_count": 1_u64,
        "session_stop_count": 1_u64,
        "password_record_count": 0_u64,
        "invalid_timestamp_metadata_count": 0_u64,
        "clock_validation": {
            "timestamp_metadata_record_count": 3_u64
        },
        "failure_reasons": [],
        "diagnostic_reasons": [],
        "target_package_seen": true
    });

    let reasons = validation_stale_reasons(&stored, &current);

    assert!(
        reasons
            .iter()
            .any(|reason| reason == "validation field changed: clock_validation"),
        "stored validation without clock_validation should be stale"
    );
}

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
    let has_session_with_video_action = result
        .get("next_actions")
        .and_then(Value::as_array)
        .is_some_and(|actions| {
            actions.iter().any(|action| {
                action.get("kind").and_then(Value::as_str) == Some("session_with_video")
            })
        });
    assert!(
        has_session_with_video_action,
        "inspect should give a canonical rerun action for missing video"
    );
    assert_session_refresh_sequence(&result, "session_with_video", EvidenceExpectation::Excluded);
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[allow(clippy::too_many_lines)]
#[test]
fn inspect_accepts_declared_canonical_video_artifacts() {
    let root = unique_temp_dir("recording-inspect-required-video-canonical");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_current_video_fixture(&root), "create video fixture") else {
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
    assert_eq!(
        result
            .pointer("/flags/canonical_clock_ready")
            .and_then(Value::as_bool),
        Some(true),
        "canonical video markers should make the recording clock-ready"
    );
    assert_eq!(
        result
            .pointer("/clock/video/status")
            .and_then(Value::as_str),
        Some("bracketed"),
        "current video marker shape should be classified as bracketed"
    );
    assert_eq!(
        result
            .pointer("/flags/has_legacy_timing")
            .and_then(Value::as_bool),
        Some(false),
        "current video marker shape should not be legacy"
    );
    assert_eq!(
        result
            .pointer("/flags/has_video_frame_index")
            .and_then(Value::as_bool),
        Some(false),
        "fresh video alone should not imply a frame index"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_video_frame_index")
            .and_then(Value::as_bool),
        Some(true),
        "fresh video without a frame index should request video-map derivation"
    );
    assert!(
        action_command(&result, "derive_video_map").is_none(),
        "inspect should not suggest video-map derivation before timeline is ready"
    );
    assert!(
        action_command(&result, "derive_timeline").is_some(),
        "inspect should suggest timeline derivation before video-map derivation"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_accepts_fresh_video_frame_index() {
    let root = unique_temp_dir("recording-inspect-video-map-fresh");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_current_video_fixture(&root), "create video fixture") else {
        return;
    };
    let Some(()) = assert_ok(write_manifest(&root, Some(video)), "write manifest") else {
        return;
    };
    let Some(()) = assert_ok(create_video_map_fixture(&root), "create video map fixture") else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/has_video_frame_index")
            .and_then(Value::as_bool),
        Some(true),
        "fresh video frame index should be ready"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_video_frame_index")
            .and_then(Value::as_bool),
        Some(false),
        "fresh video frame index should not request refresh"
    );
    assert_eq!(
        result
            .pointer("/video_map/event_mapping/status")
            .and_then(Value::as_str),
        Some("not_estimated"),
        "frame-index artifact should not imply event mapping"
    );
    assert!(
        action_command(&result, "derive_video_map").is_none(),
        "fresh frame index should not produce a video-map next action"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_accepts_fresh_event_video_map() {
    let root = unique_temp_dir("recording-inspect-video-map-event-ready");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::WithTimeline), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_current_video_fixture(&root), "create video fixture") else {
        return;
    };
    let Some(()) = assert_ok(write_manifest(&root, Some(video)), "write manifest") else {
        return;
    };
    let Some(()) = assert_ok(
        create_event_video_map_fixture(&root),
        "create event video map fixture",
    ) else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect") else {
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/has_video_frame_index")
            .and_then(Value::as_bool),
        Some(true),
        "full event map should still satisfy frame-index readiness"
    );
    assert_eq!(
        result
            .pointer("/flags/has_video_map")
            .and_then(Value::as_bool),
        Some(true),
        "fresh event-frame map should be ready"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_video_map")
            .and_then(Value::as_bool),
        Some(false),
        "fresh event-frame map should not need derivation"
    );
    assert!(
        action_command(&result, "derive_video_map").is_none(),
        "fresh event-frame map should not request derivation"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_rejects_event_video_map_without_timeline_source_lineage() {
    let root = unique_temp_dir("recording-inspect-video-map-missing-source");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::WithTimeline), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_current_video_fixture(&root), "create video fixture") else {
        return;
    };
    let Some(()) = assert_ok(write_manifest(&root, Some(video)), "write manifest") else {
        return;
    };
    let Some(()) = assert_ok(
        create_event_video_map_fixture(&root),
        "create event video map fixture",
    ) else {
        return;
    };
    let index_path = root.join("derived").join("video_map").join("index.json");
    let Some(mut index) = assert_ok(read_json(&index_path), "read video map index") else {
        return;
    };
    if let Some(sources) = index.get_mut("sources").and_then(Value::as_array_mut) {
        sources
            .retain(|source| source.get("kind").and_then(Value::as_str) != Some("timeline_events"));
    }
    let Some(()) = assert_ok(
        write_json(&index_path, &index),
        "write stale video map index",
    ) else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect") else {
        let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/has_video_map")
            .and_then(Value::as_bool),
        Some(false),
        "event map without timeline_events source lineage must not be ready"
    );
    assert!(
        result
            .pointer("/video_map/stale_reasons")
            .and_then(Value::as_array)
            .is_some_and(|reasons| reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|text| text.contains("timeline_events")))),
        "stale reasons should name the missing timeline_events source"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_detects_video_frame_index_staleness() {
    let root = unique_temp_dir("recording-inspect-video-map-stale");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_current_video_fixture(&root), "create video fixture") else {
        return;
    };
    let Some(()) = assert_ok(write_manifest(&root, Some(video)), "write manifest") else {
        return;
    };
    let Some(()) = assert_ok(create_video_map_fixture(&root), "create video map fixture") else {
        return;
    };
    let frames_path = root.join("derived").join("video_map").join("frames.jsonl");
    let Some(()) = assert_ok(
        write_jsonl(
            &frames_path,
            &[
                json!({"schema": "input_dynamics_video_frame.v1", "frame_sequence": 1_u64}),
                json!({"schema": "input_dynamics_video_frame.v1", "frame_sequence": 2_u64}),
            ],
        ),
        "mutate video frame index",
    ) else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/has_video_frame_index")
            .and_then(Value::as_bool),
        Some(false),
        "stale frame index should not be ready"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_video_frame_index")
            .and_then(Value::as_bool),
        Some(true),
        "stale frame index should request refresh"
    );
    let stale_count = result
        .pointer("/video_map/stale_reasons")
        .and_then(Value::as_array)
        .map_or(0_usize, Vec::len);
    assert!(
        stale_count > 0,
        "stale reasons should explain why the frame index needs refresh"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_keeps_declared_legacy_video_readable_but_noncanonical() {
    let root = unique_temp_dir("recording-inspect-required-video-legacy");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_legacy_video_fixture(&root), "create video fixture") else {
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
        "legacy video artifacts should remain analysis-readable"
    );
    assert_eq!(
        result
            .pointer("/flags/canonical_clock_ready")
            .and_then(Value::as_bool),
        Some(false),
        "legacy timing must not be canonical clock-ready"
    );
    assert_eq!(
        result
            .pointer("/flags/has_legacy_timing")
            .and_then(Value::as_bool),
        Some(true),
        "legacy timing should be explicit"
    );
    assert_eq!(
        result
            .pointer("/clock/video/status")
            .and_then(Value::as_str),
        Some("legacy_wall_clock_bracketed"),
        "legacy video timing should be classified as legacy wall-clock bracketed"
    );
    assert_session_refresh_sequence(
        &result,
        "session_with_canonical_clocks",
        EvidenceExpectation::Excluded,
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_keeps_nested_legacy_video_readable_but_noncanonical() {
    let root = unique_temp_dir("recording-inspect-nested-video-legacy");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(
        create_nested_legacy_video_fixture(&root),
        "create video fixture",
    ) else {
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
        "nested legacy video artifacts should remain analysis-readable"
    );
    assert_eq!(
        result
            .pointer("/flags/canonical_clock_ready")
            .and_then(Value::as_bool),
        Some(false),
        "nested legacy timing must not be canonical clock-ready"
    );
    assert_eq!(
        result
            .pointer("/flags/has_legacy_timing")
            .and_then(Value::as_bool),
        Some(true),
        "nested legacy timing should be explicit"
    );
    assert_eq!(
        result
            .pointer("/clock/video/status")
            .and_then(Value::as_str),
        Some("legacy_wall_clock_bracketed"),
        "nested legacy video timing should be classified as legacy wall-clock bracketed"
    );
    assert_eq!(
        result
            .pointer("/clock/video/clock_domain")
            .and_then(Value::as_str),
        None,
        "legacy video timing must not claim a canonical clock domain"
    );
    assert_session_refresh_sequence(
        &result,
        "session_with_canonical_clocks",
        EvidenceExpectation::Excluded,
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_rejects_nonmonotonic_video_clock_markers() -> TestResult<()> {
    let root = unique_temp_dir("recording-inspect-video-nonmonotonic");
    create_fixture(&root, FixtureShape::DerivedOnly)?;
    let mut video = create_current_video_fixture(&root)?;
    let object = video.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "video fixture should be an object",
        )
    })?;
    let stop = object
        .get_mut("stop")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "video stop fixture should be an object",
            )
        })?;
    stop.insert(
        String::from("before"),
        probe_marker("before_screenrecord_stop", 9, 19),
    );
    write_json(&root.join("video").join("timing.json"), &video)?;
    write_manifest(&root, Some(video))?;
    let result = inspect_recording(&root)?;

    ensure_eq(
        &result
            .pointer("/clock/video/status")
            .and_then(Value::as_str),
        &Some("probe_failed"),
        "nonmonotonic current markers should fail probe validation",
    )?;
    ensure_eq(
        &result
            .pointer("/flags/canonical_clock_ready")
            .and_then(Value::as_bool),
        &Some(false),
        "invalid markers must not be canonical",
    )?;
    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn inspect_rejects_malformed_video_clock_wrapper() -> TestResult<()> {
    let root = unique_temp_dir("recording-inspect-video-bad-wrapper");
    create_fixture(&root, FixtureShape::DerivedOnly)?;
    let mut video = create_current_video_fixture(&root)?;
    let marker = video
        .pointer_mut("/start/before")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "video start before marker should be an object",
            )
        })?;
    marker.insert(String::from("schema"), json!("bad_probe_wrapper.v1"));
    write_json(&root.join("video").join("timing.json"), &video)?;
    write_manifest(&root, Some(video))?;
    let result = inspect_recording(&root)?;

    ensure_eq(
        &result
            .pointer("/clock/video/status")
            .and_then(Value::as_str),
        &Some("probe_failed"),
        "malformed probe wrappers should fail canonical readiness",
    )?;
    ensure_eq(
        &result
            .pointer("/flags/canonical_clock_ready")
            .and_then(Value::as_bool),
        &Some(false),
        "malformed wrappers must not be canonical",
    )?;
    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn inspect_reports_stale_video_clock_inputs() -> TestResult<()> {
    let root = unique_temp_dir("recording-inspect-video-clock-stale");
    create_fixture(&root, FixtureShape::DerivedOnly)?;
    let video = create_current_video_fixture(&root)?;
    fs::write(root.join("video").join("screen.mp4"), "changed-video\n")?;
    write_manifest(&root, Some(video))?;
    let result = inspect_recording(&root)?;

    ensure_eq(
        &result
            .pointer("/clock/video/status")
            .and_then(Value::as_str),
        &Some("stale_inputs"),
        "changed screen video should stale canonical video clock anchors",
    )?;
    ensure_eq(
        &result
            .pointer("/clock/stale_inputs")
            .and_then(Value::as_bool),
        &Some(true),
        "top-level clock should surface stale inputs",
    )?;
    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn inspect_preserves_evidence_capture_in_missing_video_rerun_action() {
    let root = unique_temp_dir("recording-inspect-video-evidence-rerun");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(evidence) = assert_ok(create_legacy_evidence_fixture(&root), "create evidence") else {
        return;
    };
    let Some(()) = assert_ok(
        write_manifest_with(&root, Some(required_video_manifest()), Some(evidence)),
        "write manifest",
    ) else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/flags/needs_video")
            .and_then(Value::as_bool),
        Some(true),
        "missing required video should need video rerun"
    );
    assert_eq!(
        result
            .pointer("/flags/needs_canonical_evidence")
            .and_then(Value::as_bool),
        Some(true),
        "legacy requested evidence should be rerun with evidence"
    );
    assert_session_refresh_sequence(&result, "session_with_video", EvidenceExpectation::Included);
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_classifies_canonical_evidence_brackets() {
    let root = unique_temp_dir("recording-inspect-evidence-canonical");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_current_video_fixture(&root), "create video fixture") else {
        return;
    };
    let Some(evidence) = assert_ok(create_current_evidence_fixture(&root), "create evidence")
    else {
        return;
    };
    let Some(()) = assert_ok(
        write_manifest_with(&root, Some(video), Some(evidence)),
        "write manifest",
    ) else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/clock/evidence/status")
            .and_then(Value::as_str),
        Some("bracketed"),
        "current evidence brackets should be canonical"
    );
    assert_eq!(
        result
            .pointer("/flags/canonical_clock_ready")
            .and_then(Value::as_bool),
        Some(true),
        "canonical video and evidence brackets should be clock-ready"
    );
    let _cleanup = assert_ok(fs::remove_dir_all(&root), "cleanup");
}

#[test]
fn inspect_classifies_legacy_evidence_as_noncanonical() {
    let root = unique_temp_dir("recording-inspect-evidence-legacy");
    let Some(()) = assert_ok(create_fixture(&root, FixtureShape::DerivedOnly), "fixture") else {
        return;
    };
    let Some(video) = assert_ok(create_current_video_fixture(&root), "create video fixture") else {
        return;
    };
    let Some(evidence) = assert_ok(create_legacy_evidence_fixture(&root), "create evidence") else {
        return;
    };
    let Some(()) = assert_ok(
        write_manifest_with(&root, Some(video), Some(evidence)),
        "write manifest",
    ) else {
        return;
    };

    let Some(result) = assert_ok(inspect_recording(&root), "inspect recording") else {
        return;
    };

    assert_eq!(
        result
            .pointer("/clock/evidence/status")
            .and_then(Value::as_str),
        Some("legacy_wall_clock_bracketed"),
        "wall-clock evidence indexes should be legacy"
    );
    assert_eq!(
        result
            .pointer("/flags/canonical_clock_ready")
            .and_then(Value::as_bool),
        Some(false),
        "legacy evidence should block canonical clock readiness when requested"
    );
    assert_session_refresh_sequence(
        &result,
        "session_with_canonical_clocks",
        EvidenceExpectation::Included,
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
    write_manifest_with(root, video, None)
}

fn write_manifest_with(
    root: &Path,
    video: Option<Value>,
    evidence: Option<Value>,
) -> TestResult<()> {
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
    if let Some(evidence_value) = evidence {
        let Some(object) = manifest.as_object_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "manifest fixture should be a JSON object",
            )
            .into());
        };
        object.insert(String::from("evidence"), evidence_value);
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

fn create_legacy_video_fixture(root: &Path) -> TestResult<Value> {
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

fn create_nested_legacy_video_fixture(root: &Path) -> TestResult<Value> {
    let video_dir = root.join("video");
    fs::create_dir_all(&video_dir)?;
    let screen_path = video_dir.join("screen.mp4");
    fs::write(&screen_path, "not-a-real-mp4-for-inspection\n")?;
    let video = json!({
        "enabled": true,
        "required": true,
        "remote_path": "/sdcard/Download/input-dynamics-run-test.mp4",
        "local_path": "video/screen.mp4",
        "timing_path": "video/timing.json",
        "file": file_fingerprint(&screen_path)?,
        "start": {
            "phase": "start",
            "before": {
                "host_wall_ms_before_device_timestamp": 1_100_i64,
                "device_wall_ms": 1_101_i64,
                "host_wall_ms_after_device_timestamp": 1_102_i64
            },
            "after": {
                "host_wall_ms_before_device_timestamp": 1_103_i64,
                "device_wall_ms": 1_104_i64,
                "host_wall_ms_after_device_timestamp": 1_105_i64
            }
        },
        "stop": {
            "phase": "stop",
            "before": {
                "host_wall_ms_before_device_timestamp": 1_900_i64,
                "device_wall_ms": 1_901_i64,
                "host_wall_ms_after_device_timestamp": 1_902_i64
            },
            "after": {
                "host_wall_ms_before_device_timestamp": 1_903_i64,
                "device_wall_ms": 1_904_i64,
                "host_wall_ms_after_device_timestamp": 1_905_i64
            }
        },
        "ok": true,
    });
    write_json(&video_dir.join("timing.json"), &video)?;
    fs::write(video_dir.join("screenrecord.stdout.log"), "")?;
    fs::write(video_dir.join("screenrecord.stderr.log"), "")?;
    fs::write(video_dir.join("adb-pull-video.log"), "pulled\n")?;
    Ok(video)
}

fn create_current_video_fixture(root: &Path) -> TestResult<Value> {
    let video_dir = root.join("video");
    fs::create_dir_all(&video_dir)?;
    let screen_path = video_dir.join("screen.mp4");
    fs::write(&screen_path, "not-a-real-mp4-for-inspection\n")?;
    let file = file_fingerprint(&screen_path)?;
    let video = json!({
        "schema": "input_dynamics_video_capture.v1",
        "enabled": true,
        "required": true,
        "remote_path": "/sdcard/Download/input-dynamics-run-test.mp4",
        "local_path": "video/screen.mp4",
        "timing_path": "video/timing.json",
        "stdout_log": "video/screenrecord.stdout.log",
        "stderr_log": "video/screenrecord.stderr.log",
        "pull_log": "video/adb-pull-video.log",
        "file": file,
        "start": {
            "schema": "input_dynamics_video_capture.v1",
            "ok": true,
            "enabled": true,
            "required": true,
            "requested": true,
            "before": probe_marker("before_screenrecord_start", 10, 20),
            "after": probe_marker("after_screenrecord_start", 11, 21),
        },
        "stop": {
            "ok": true,
            "enabled": true,
            "required": true,
            "requested": true,
            "before": probe_marker("before_screenrecord_stop", 20, 30),
            "after": probe_marker("after_screenrecord_stop", 21, 31),
            "file": file,
        },
        "ok": true,
    });
    write_json(&video_dir.join("timing.json"), &video)?;
    fs::write(video_dir.join("screenrecord.stdout.log"), "")?;
    fs::write(video_dir.join("screenrecord.stderr.log"), "")?;
    fs::write(video_dir.join("adb-pull-video.log"), "pulled\n")?;
    Ok(video)
}

fn create_current_evidence_fixture(root: &Path) -> TestResult<Value> {
    create_evidence_indexes(root)?;
    Ok(json!({
        "enabled": true,
        "policy": "start_end",
        "start": current_evidence_phase("start", 12, 22, 13, 23),
        "end": current_evidence_phase("end", 18, 28, 19, 29),
    }))
}

fn create_legacy_evidence_fixture(root: &Path) -> TestResult<Value> {
    create_evidence_indexes(root)?;
    Ok(json!({
        "enabled": true,
        "policy": "start_end",
        "start": legacy_evidence_phase("start"),
        "end": legacy_evidence_phase("end"),
    }))
}

fn create_evidence_indexes(root: &Path) -> TestResult<()> {
    for phase in ["start", "end"] {
        let phase_dir = root.join("evidence").join(phase);
        fs::create_dir_all(&phase_dir)?;
        write_json(
            &phase_dir.join("index.json"),
            &json!({
                "schema": "input_dynamics_observation_bundle.v1",
                "phase": phase,
                "captured_wall_ms": 1_800_000_000_000_i64,
                "artifacts": {
                    "status": "status.json",
                    "layout": "layout.json",
                    "accessibility": "accessibility.xml",
                    "screenshot": "screenshot.png",
                    "state": "state.json"
                },
                "state": {"ok": true}
            }),
        )?;
    }
    Ok(())
}

fn current_evidence_phase(
    phase: &str,
    before_uptime_ms: i64,
    before_elapsed_ms: i64,
    after_uptime_ms: i64,
    after_elapsed_ms: i64,
) -> Value {
    json!({
        "schema": "input_dynamics_record_evidence_capture.v1",
        "enabled": true,
        "requested": true,
        "phase": phase,
        "policy": "start_end",
        "clock_domain": "device_elapsed_realtime_ns",
        "clock_alignment_status": "bracketed",
        "before": probe_marker(&format!("before_evidence_{phase}"), before_uptime_ms, before_elapsed_ms),
        "after": probe_marker(&format!("after_evidence_{phase}"), after_uptime_ms, after_elapsed_ms),
        "bundle": {
            "schema": "input_dynamics_observation_bundle.v1",
            "index": format!("evidence/{phase}/index.json")
        }
    })
}

fn legacy_evidence_phase(phase: &str) -> Value {
    json!({
        "schema": "input_dynamics_record_evidence_capture.v1",
        "enabled": true,
        "requested": true,
        "phase": phase,
        "policy": "start_end",
        "bundle": {
            "schema": "input_dynamics_observation_bundle.v1",
            "index": format!("evidence/{phase}/index.json")
        }
    })
}

fn probe_marker(phase: &str, uptime_ms: i64, elapsed_realtime_ms: i64) -> Value {
    let uptime_ns = uptime_ms.saturating_mul(1_000_000);
    let elapsed_realtime_ns = elapsed_realtime_ms.saturating_mul(1_000_000);
    let request_id = format!("request-{phase}");
    json!({
        "schema": "input_dynamics_device_clock_probe.v1",
        "phase": phase,
        "probe_source": "ime_status_broadcast",
        "request_id": request_id,
        "package_name": "org.inputdynamics.ime.debug",
        "command": "STATUS",
        "result_file_path": "/sdcard/Android/data/org.inputdynamics.ime.debug/files/research_typing_logs/input_dynamics_command_result.json",
        "status_file_path": "/sdcard/Android/data/org.inputdynamics.ime.debug/files/research_typing_logs/input_dynamics_control_status.json",
        "host_wall_ms_before_device_timestamp": 1_800_000_000_000_i64,
        "host_wall_ms_after_device_timestamp": 1_800_000_000_001_i64,
        "host_monotonic_ns_before_device_timestamp": 1_000_i64,
        "host_monotonic_ns_after_device_timestamp": 2_000_i64,
        "host_monotonic_reference": "cli_process_start",
        "host_bracket": {
            "clock_domain": "host_process_monotonic_ns",
            "timestamp_source": "host_process",
            "timestamp_precision": "nanoseconds",
            "before_ns": 1_000_i64,
            "after_ns": 2_000_i64
        },
        "host_wall_bracket": {
            "clock_domain": "host_wall_ms",
            "timestamp_source": "host_process",
            "timestamp_precision": "milliseconds",
            "before_ms": 1_800_000_000_000_i64,
            "after_ms": 1_800_000_000_001_i64
        },
        "clock_domain": "device_elapsed_realtime_ns",
        "clock_alignment_status": "not_estimated",
        "device_clock_probe": device_clock_probe(&request_id, uptime_ms, uptime_ns, elapsed_realtime_ns),
        "t_uptime_ms": uptime_ms,
        "t_uptime_ns": uptime_ns,
        "t_elapsed_realtime_ns": elapsed_realtime_ns,
        "device_wall_ms": 1_800_000_000_000_i64,
    })
}

fn device_clock_probe(
    request_id: &str,
    uptime_ms: i64,
    uptime_ns: i64,
    elapsed_realtime_ns: i64,
) -> Value {
    json!({
        "schema": "input_dynamics_device_clock_probe.v1",
        "request_id": request_id,
        "probe_source": "status_broadcast",
        "captured_by": "android_control_status",
        "canonical_clock_domain": "device_elapsed_realtime_ns",
        "wall_time_role": "diagnostic",
        "pending_writes_drained": true,
        "t_uptime_ms": uptime_ms,
        "t_uptime_ns": uptime_ns,
        "t_elapsed_realtime_ns": elapsed_realtime_ns,
        "t_wall_ms": 1_800_000_000_000_i64,
        "uptime_time": {
            "clock_domain": "android_uptime_ms",
            "timestamp_source": "callback_capture",
            "timestamp_precision": "milliseconds",
            "field": "t_uptime_ms",
            "field_ns": "t_uptime_ns",
            "field_ns_precision": "milliseconds_converted_to_nanoseconds"
        },
        "elapsed_realtime_time": {
            "clock_domain": "device_elapsed_realtime_ns",
            "timestamp_source": "callback_capture",
            "timestamp_precision": "nanoseconds",
            "field": "t_elapsed_realtime_ns"
        },
        "wall_time": {
            "clock_domain": "device_wall_ms",
            "timestamp_source": "callback_capture",
            "timestamp_precision": "milliseconds",
            "field": "t_wall_ms"
        }
    })
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

fn create_video_map_fixture(root: &Path) -> TestResult<()> {
    let video_map_dir = root.join("derived").join("video_map");
    fs::create_dir_all(&video_map_dir)?;
    let frames_path = video_map_dir.join("frames.jsonl");
    write_jsonl(
        &frames_path,
        &[json!({
            "schema": "input_dynamics_video_frame.v1",
            "event": "video_frame",
            "artifact_stage": "frame_index",
            "frame_id": "frame:00000001",
            "frame_sequence": 1_u64,
            "clock_domain": "media_pts_ns",
            "media_time": {
                "clock_domain": "media_pts_ns",
                "timestamp_source": "media_probe",
                "timestamp_precision": "nanoseconds",
                "timestamp_field": "pts_time",
                "pts_ns": 0_i64,
                "pts_time_seconds": "0.000000",
                "pts_tick": 0_i64
            },
            "duration_ns": 33_333_333_i64,
            "pts_interval_ns": null,
            "is_key_frame": true,
            "width": 1080_i64,
            "height": 2400_i64,
            "encoded_size_bytes": 123_i64,
        })],
    )?;
    write_json(
        &video_map_dir.join("index.json"),
        &json!({
            "schema": "input_dynamics_video_map_index.v1",
            "artifact_stage": "frame_index",
            "sources": video_map_sources_json(root)?,
            "outputs": video_map_outputs_json(&frames_path)?,
            "frame_count": 1_u64,
            "probe_status": "ok",
            "alignment_status": "not_estimated",
            "event_mapping": {
                "status": "not_estimated",
                "mapped_event_count": null,
                "unmapped_event_count": null
            }
        }),
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn create_event_video_map_fixture(root: &Path) -> TestResult<()> {
    let video_map_dir = root.join("derived").join("video_map");
    fs::create_dir_all(&video_map_dir)?;
    let frames_path = video_map_dir.join("frames.jsonl");
    let alignment_path = video_map_dir.join("alignment.json");
    let event_frames_path = video_map_dir.join("event_frames.jsonl");
    write_jsonl(
        &frames_path,
        &[json!({
            "schema": "input_dynamics_video_frame.v1",
            "event": "video_frame",
            "artifact_stage": "frame_index",
            "frame_id": "frame:00000001",
            "frame_sequence": 1_u64,
            "clock_domain": "media_pts_ns",
            "media_time": {
                "clock_domain": "media_pts_ns",
                "timestamp_source": "media_probe",
                "timestamp_precision": "nanoseconds",
                "field": "pts_ns",
                "pts_ns": 0_i64,
                "pts_tick": 0_i64
            },
            "duration_ns": 33_333_333_i64,
            "pts_interval_ns": 33_333_333_i64,
            "is_key_frame": true,
            "width": 1080_i64,
            "height": 2400_i64,
            "encoded_size_bytes": 123_i64,
        })],
    )?;
    write_json(
        &alignment_path,
        &json!({
            "schema": "input_dynamics_video_alignment.v1",
            "alignment_id": "video_alignment:device_elapsed_realtime_ns_to_media_pts_ns:v1",
            "status": "bracketed"
        }),
    )?;
    write_jsonl(
        &event_frames_path,
        &[json!({
            "schema": "input_dynamics_event_video_frame_map.v1",
            "timeline_event_id": "timeline:000001",
            "timeline_ref": {
                "path": "derived/timeline/events.jsonl",
                "line_index": 1_u64
            },
            "event": "key_down",
            "record_kind": "ime_event",
            "source_ref": {
                "path": "ime/session-test.jsonl",
                "line_index": 1_u64
            },
            "mapping_status": "bracketed",
            "mapping_input_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "derived_transform",
                "timestamp_precision": "nanoseconds",
                "time_interval_ns": [1_000_i64, 2_000_i64],
                "source_time_status": "timeline_normalized_time",
                "transform_id": "timeline_normalized_time"
            },
            "video_time": {
                "clock_domain": "media_pts_ns",
                "timestamp_source": "derived_transform",
                "timestamp_precision": "nanoseconds",
                "time_interval_ns": [1_000_i64, 2_000_i64],
                "unclipped_time_interval_ns": [1_000_i64, 2_000_i64],
                "transform_id": "video_alignment:device_elapsed_realtime_ns_to_media_pts_ns:v1",
                "uncertainty_ns": 1_000_i64
            },
            "frame_window": {
                "start_frame_id": "frame:00000001",
                "end_frame_id": "frame:00000001",
                "nominal_frame_id": "frame:00000001",
                "start_frame_sequence": 1_u64,
                "end_frame_sequence": 1_u64,
                "nominal_frame_sequence": 1_u64,
                "selection_policy": "interval_overlap_with_midpoint_nominal"
            },
            "reasons": ["timeline_normalized_device_elapsed_time"],
            "warnings": []
        })],
    )?;
    write_json(
        &video_map_dir.join("index.json"),
        &json!({
            "schema": "input_dynamics_video_map_index.v1",
            "artifact_stage": "event_frame_map",
            "sources": event_video_map_sources_json(root)?,
            "outputs": event_video_map_outputs_json(&frames_path, &alignment_path, &event_frames_path)?,
            "frame_count": 1_u64,
            "probe_status": "ok",
            "alignment_status": "bracketed",
            "event_mapping": {
                "status": "bracketed",
                "source_event_count": 1_u64,
                "row_count": 1_u64,
                "mapped_event_count": 1_u64,
                "unmapped_event_count": 0_u64
            }
        }),
    )?;
    Ok(())
}

fn video_map_sources_json(root: &Path) -> TestResult<Value> {
    Ok(json!([
        {
            "kind": "manifest",
            "path": "manifest.json",
            "exists": true,
            "required": true,
            "fingerprint": file_fingerprint(&root.join("manifest.json"))?
        },
        {
            "kind": "video_screen",
            "path": "video/screen.mp4",
            "exists": true,
            "required": true,
            "fingerprint": file_fingerprint(&root.join("video").join("screen.mp4"))?
        },
        {
            "kind": "video_timing",
            "path": "video/timing.json",
            "exists": true,
            "required": true,
            "fingerprint": file_fingerprint(&root.join("video").join("timing.json"))?
        }
    ]))
}

fn event_video_map_sources_json(root: &Path) -> TestResult<Value> {
    let Some(mut sources) = video_map_sources_json(root)?.as_array().cloned() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "video map sources fixture should be an array",
        )
        .into());
    };
    sources.push(json!({
        "kind": "timeline_index",
        "path": "derived/timeline/index.json",
        "exists": true,
        "required": true,
        "fingerprint": file_fingerprint(&root.join("derived").join("timeline").join("index.json"))?
    }));
    sources.push(json!({
        "kind": "timeline_events",
        "path": "derived/timeline/events.jsonl",
        "exists": true,
        "required": true,
        "record_count": 1_u64,
        "fingerprint": file_fingerprint(&root.join("derived").join("timeline").join("events.jsonl"))?
    }));
    Ok(Value::Array(sources))
}

fn video_map_outputs_json(frames_path: &Path) -> TestResult<Value> {
    Ok(json!({
        "video_map_index": {
            "path": "derived/video_map/index.json",
            "schema": "input_dynamics_video_map_index.v1",
            "record_count": null,
            "sensitive": true,
            "fingerprint": null,
            "fingerprint_status": "not_embedded_self_reference"
        },
        "video_map_frames": {
            "path": "derived/video_map/frames.jsonl",
            "schema": "input_dynamics_video_frame.v1",
            "record_count": 1_u64,
            "sensitive": true,
            "fingerprint": file_fingerprint(frames_path)?
        }
    }))
}

fn event_video_map_outputs_json(
    frames_path: &Path,
    alignment_path: &Path,
    event_frames_path: &Path,
) -> TestResult<Value> {
    Ok(json!({
        "video_map_index": {
            "path": "derived/video_map/index.json",
            "schema": "input_dynamics_video_map_index.v1",
            "record_count": null,
            "sensitive": true,
            "fingerprint": null,
            "fingerprint_status": "not_embedded_self_reference"
        },
        "video_map_frames": {
            "path": "derived/video_map/frames.jsonl",
            "schema": "input_dynamics_video_frame.v1",
            "record_count": 1_u64,
            "sensitive": true,
            "fingerprint": file_fingerprint(frames_path)?
        },
        "video_map_alignment": {
            "path": "derived/video_map/alignment.json",
            "schema": "input_dynamics_video_alignment.v1",
            "record_count": 1_u64,
            "sensitive": true,
            "fingerprint": file_fingerprint(alignment_path)?
        },
        "video_map_event_frames": {
            "path": "derived/video_map/event_frames.jsonl",
            "schema": "input_dynamics_event_video_frame_map.v1",
            "record_count": 1_u64,
            "sensitive": true,
            "fingerprint": file_fingerprint(event_frames_path)?
        }
    }))
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

fn read_json(path: &Path) -> TestResult<Value> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
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

fn ensure_eq<T>(actual: &T, expected: &T, label: &str) -> Result<(), String>
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{label} mismatch: actual={actual:?} expected={expected:?}"
        ))
    }
}

fn action_command<'a>(result: &'a Value, kind: &str) -> Option<&'a str> {
    result
        .get("next_actions")
        .and_then(Value::as_array)?
        .iter()
        .find(|action| action.get("kind").and_then(Value::as_str) == Some(kind))?
        .get("command")
        .and_then(Value::as_str)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EvidenceExpectation {
    Included,
    Excluded,
}

impl EvidenceExpectation {
    const fn is_included(self) -> bool {
        matches!(self, Self::Included)
    }
}

fn assert_session_refresh_sequence(result: &Value, kind: &str, evidence: EvidenceExpectation) {
    let action_candidate = result
        .get("next_actions")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions
                .iter()
                .find(|action| action.get("kind").and_then(Value::as_str) == Some(kind))
        });
    assert!(
        action_candidate.is_some(),
        "missing session refresh action {kind}"
    );
    let Some(action) = action_candidate else {
        return;
    };
    assert_eq!(
        action.get("workflow").and_then(Value::as_str),
        Some("session_start_status_stop_inspect"),
        "session refresh action should name the complete lifecycle"
    );
    assert_eq!(
        action.get("command").and_then(Value::as_str),
        Some(if evidence.is_included() {
            "input-dynamics session start --input-actor human --run-id <new-run-id> --out <new-run-dir> --with-evidence"
        } else {
            "input-dynamics session start --input-actor human --run-id <new-run-id> --out <new-run-dir>"
        }),
        "compatibility command should stay the session start step"
    );
    let command_sequence = action.get("commands").and_then(Value::as_array);
    assert!(
        command_sequence.is_some(),
        "session refresh action {kind} should include command sequence"
    );
    let Some(commands) = command_sequence else {
        return;
    };
    let steps = commands
        .iter()
        .map(|command| command.get("step").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        steps,
        vec![Some("start"), Some("status"), Some("stop"), Some("inspect")],
        "session refresh action should include start/status/stop/inspect sequence"
    );
    let start_step = commands.first();
    assert!(
        start_step.is_some(),
        "session refresh action {kind} is missing start"
    );
    let Some(start) = start_step else {
        return;
    };
    assert_eq!(
        start.get("command").and_then(Value::as_str),
        action.get("command").and_then(Value::as_str),
        "legacy command field should mirror sequence start step"
    );
    let start_argv_values = start.get("argv").and_then(Value::as_array);
    assert!(
        start_argv_values.is_some(),
        "session refresh action {kind} start should include argv"
    );
    let Some(start_argv) = start_argv_values else {
        return;
    };
    assert_eq!(
        start_argv.iter().any(|value| value == "--with-evidence"),
        evidence.is_included(),
        "session refresh action should preserve evidence request in structured argv"
    );
}

fn assert_ok<T, E>(result: Result<T, E>, label: &str) -> Option<T>
where
    E: Debug,
{
    let error = result.as_ref().err();
    assert!(error.is_none(), "{label} failed: {error:?}");
    result.ok()
}
