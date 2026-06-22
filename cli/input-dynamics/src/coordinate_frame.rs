//! Recorded coordinate-frame extraction for run analysis.

use std::fs;
use std::path::Path;

use input_dynamics_analysis::derivation::ScreenConfig;
use serde_json::{Value, json};

use crate::error::{CliError, CliResult};

const COORDINATE_FRAME_SCHEMA: &str = "input_dynamics_coordinate_frame.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScreenFrame {
    x_min: i64,
    x_max: i64,
    y_min: i64,
    y_max: i64,
    width: i64,
    height: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeyboardFrame {
    source: String,
    top_y: i64,
    height: Option<i64>,
    visible: bool,
}

pub(crate) fn manifest_coordinate_frame(
    touchscreen_snapshot: &Value,
    layout_snapshots: &[(&str, &Value)],
) -> Value {
    let physical_touchscreen = touchscreen_snapshot
        .get("physical_touchscreen")
        .cloned()
        .unwrap_or(Value::Null);
    let screen = screen_frame_from_touchscreen(&physical_touchscreen);
    let keyboard = layout_snapshots
        .iter()
        .find_map(|&(source, value)| keyboard_frame_from_layout(source, value));
    let errors = coordinate_frame_errors(screen, touchscreen_snapshot);
    json!({
        "schema": COORDINATE_FRAME_SCHEMA,
        "ok": errors.is_empty(),
        "screen": screen.map_or(Value::Null, screen_frame_json),
        "keyboard": keyboard.as_ref().map_or(Value::Null, keyboard_frame_json),
        "physical_touchscreen_profile_hash": touchscreen_snapshot
            .get("physical_touchscreen_profile_hash")
            .cloned()
            .unwrap_or(Value::Null),
        "physical_touchscreen": physical_touchscreen,
        "sources": {
            "touchscreen": "commands.touchscreen_profile",
            "layouts": layout_snapshots
                .iter()
                .map(|&(source, _value)| source)
                .collect::<Vec<_>>(),
        },
        "errors": errors,
    })
}

pub(crate) fn screen_config_from_run_manifest(run_dir: &Path) -> CliResult<ScreenConfig> {
    let manifest_path = run_dir.join("manifest.json");
    let manifest_text = fs::read_to_string(&manifest_path).map_err(|error| {
        CliError::new(format!(
            "failed to read recorded run manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest = serde_json::from_str::<Value>(&manifest_text)?;
    screen_config_from_manifest(&manifest).map_err(|error| {
        CliError::new(format!(
            "{error}; record a new run with current input-dynamics record so manifest.json contains coordinate_frame"
        ))
    })
}

fn screen_config_from_manifest(manifest: &Value) -> CliResult<ScreenConfig> {
    let frame = manifest
        .get("coordinate_frame")
        .ok_or_else(|| CliError::new("record manifest is missing coordinate_frame"))?;
    if frame.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(CliError::new(format!(
            "recorded coordinate_frame is not usable: {}",
            frame
                .get("errors")
                .map_or_else(|| String::from("unknown error"), Value::to_string)
        )));
    }
    let width = required_i64_pointer(frame, "/screen/width_px")?;
    let height = required_i64_pointer(frame, "/screen/height_px")?;
    let keyboard_top_y = optional_i64_pointer(frame, "/keyboard/top_y_px")?;
    Ok(ScreenConfig {
        width,
        height,
        keyboard_top_y,
    })
}

fn coordinate_frame_errors(
    screen: Option<ScreenFrame>,
    touchscreen_snapshot: &Value,
) -> Vec<String> {
    let mut errors = Vec::new();
    if touchscreen_snapshot.get("ok").and_then(Value::as_bool) != Some(true) {
        errors.push(String::from("touchscreen profile discovery failed"));
    }
    if screen.is_none() {
        errors.push(String::from(
            "physical touchscreen profile is missing x/y coordinate ranges",
        ));
    }
    errors
}

fn screen_frame_from_touchscreen(touchscreen: &Value) -> Option<ScreenFrame> {
    let x_min = i64_pointer(touchscreen, "/x_range/minimum")?;
    let x_max = i64_pointer(touchscreen, "/x_range/maximum")?;
    let y_min = i64_pointer(touchscreen, "/y_range/minimum")?;
    let y_max = i64_pointer(touchscreen, "/y_range/maximum")?;
    let width = inclusive_size(x_min, x_max)?;
    let height = inclusive_size(y_min, y_max)?;
    Some(ScreenFrame {
        x_min,
        x_max,
        y_min,
        y_max,
        width,
        height,
    })
}

fn keyboard_frame_from_layout(source: &str, value: &Value) -> Option<KeyboardFrame> {
    let layout = value.get("keyboard_layout")?;
    let available = layout
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let visible = layout
        .get("keyboard_view_visible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !available || !visible {
        return None;
    }
    let top_y = i64_pointer(layout, "/keyboard_view_location_on_screen_y_px")
        .or_else(|| i64_pointer(layout, "/keyboard_view_top_screen_px"))?;
    Some(KeyboardFrame {
        source: String::from(source),
        top_y,
        height: i64_pointer(layout, "/keyboard_view_height_px"),
        visible,
    })
}

fn inclusive_size(minimum: i64, maximum: i64) -> Option<i64> {
    maximum.checked_sub(minimum)?.checked_add(1)
}

fn screen_frame_json(frame: ScreenFrame) -> Value {
    json!({
        "width_px": frame.width,
        "height_px": frame.height,
        "x_min_px": frame.x_min,
        "x_max_px": frame.x_max,
        "y_min_px": frame.y_min,
        "y_max_px": frame.y_max,
    })
}

fn keyboard_frame_json(frame: &KeyboardFrame) -> Value {
    json!({
        "source": frame.source,
        "top_y_px": frame.top_y,
        "height_px": frame.height,
        "visible": frame.visible,
    })
}

fn required_i64_pointer(value: &Value, pointer: &str) -> CliResult<i64> {
    i64_pointer(value, pointer)
        .ok_or_else(|| CliError::new(format!("recorded coordinate_frame missing {pointer}")))
}

fn optional_i64_pointer(value: &Value, pointer: &str) -> CliResult<Option<i64>> {
    match value.pointer(pointer) {
        None => Ok(None),
        Some(pointer_value) if pointer_value.is_null() => Ok(None),
        Some(pointer_value) if pointer_value.is_number() => Ok(pointer_value.as_i64()),
        Some(other) => Err(CliError::new(format!(
            "recorded coordinate_frame {pointer} is not an integer: {other}"
        ))),
    }
}

fn i64_pointer(value: &Value, pointer: &str) -> Option<i64> {
    value.pointer(pointer).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::coordinate_frame::{
        manifest_coordinate_frame, screen_config_from_manifest, screen_frame_from_touchscreen,
    };

    #[test]
    fn manifest_coordinate_frame_uses_touchscreen_ranges_and_visible_layout() {
        let touchscreen = json!({
            "ok": true,
            "physical_touchscreen_profile_hash": "abc123",
            "physical_touchscreen": {
                "x_range": {"minimum": 0_i64, "maximum": 1_439_i64},
                "y_range": {"minimum": 0_i64, "maximum": 3_119_i64}
            }
        });
        let layout = json!({
            "keyboard_layout": {
                "available": true,
                "keyboard_view_visible": true,
                "keyboard_view_location_on_screen_y_px": 2_063_i64,
                "keyboard_view_height_px": 1_057_i64
            }
        });

        let frame = manifest_coordinate_frame(&touchscreen, &[("layout_before_capture", &layout)]);

        assert_eq!(
            frame.pointer("/ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            frame
                .pointer("/screen/width_px")
                .and_then(serde_json::Value::as_i64),
            Some(1440)
        );
        assert_eq!(
            frame
                .pointer("/screen/height_px")
                .and_then(serde_json::Value::as_i64),
            Some(3120)
        );
        assert_eq!(
            frame
                .pointer("/keyboard/top_y_px")
                .and_then(serde_json::Value::as_i64),
            Some(2063)
        );
    }

    #[test]
    fn screen_config_requires_recorded_coordinate_frame() {
        let parsed = screen_config_from_manifest(&json!({}));

        assert!(
            parsed.is_err(),
            "derive should fail instead of accepting caller-supplied geometry"
        );
    }

    #[test]
    fn touchscreen_screen_frame_requires_axis_ranges() {
        let frame = screen_frame_from_touchscreen(&json!({
            "x_range": {"minimum": 0_i64, "maximum": 1_439_i64}
        }));

        assert!(
            frame.is_none(),
            "screen frame needs both x and y touchscreen ranges"
        );
    }
}
