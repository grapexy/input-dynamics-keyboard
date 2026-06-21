//! AOSP uinput command-stream support.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::app::App;
use crate::error::{CliError, CliResult};
use crate::process::{FailureMode, run_process_with_stdin};
use crate::ratio::{RATIO_SCALE_PPM, RatioPpm};

const UINPUT_COMMAND: &str = "/system/bin/uinput";
const DEVICE_ID: i64 = 1;
const DEFAULT_HOLD_MS: u64 = 70;
pub(crate) const DEVICE_SETTLE_MS: u64 = 1_000;
pub(crate) const DEVICE_TAIL_MS: u64 = 1_000;
const TRACKING_ID: i32 = 100;
const DEFAULT_TOUCH_MAJOR: i32 = 120;
const DEFAULT_TOUCH_MINOR: i32 = 80;
const DEFAULT_PRESSURE: i32 = 30;
const RATIO_SCALE_PPM_I64: i64 = 1_000_000;
const RATIO_HALF_SCALE_PPM_I64: i64 = 500_000;

const EV_SYN: i32 = 0;
const EV_KEY: i32 = 1;
const EV_ABS: i32 = 3;
const SYN_REPORT: i32 = 0;
const KEY_HOMEPAGE: i32 = 172;
const BTN_TOOL_FINGER: i32 = 325;
const BTN_TOUCH: i32 = 330;
const ABS_X: i32 = 0;
const ABS_Y: i32 = 1;
const ABS_PRESSURE: i32 = 24;
const ABS_MT_SLOT: i32 = 47;
const ABS_MT_TOUCH_MAJOR: i32 = 48;
const ABS_MT_TOUCH_MINOR: i32 = 49;
const ABS_MT_ORIENTATION: i32 = 52;
const ABS_MT_POSITION_X: i32 = 53;
const ABS_MT_POSITION_Y: i32 = 54;
const ABS_MT_TOOL_TYPE: i32 = 55;
const ABS_MT_TRACKING_ID: i32 = 57;
const ABS_MT_PRESSURE: i32 = 58;
const INPUT_PROP_DIRECT: i32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TouchscreenProfile {
    event_path: String,
    name: String,
    location: String,
    unique_id: String,
    bus_hex: String,
    bus_name: String,
    vendor_id: i32,
    product_id: i32,
    version_id: i32,
    key_bits: Vec<i32>,
    prop_bits: Vec<i32>,
    abs_axes: Vec<AbsAxis>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AbsAxis {
    code: i32,
    name: String,
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct TapSpec {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) hold_ms: u64,
    pub(crate) pressure: i32,
    pub(crate) touch_major_px: i32,
    pub(crate) touch_minor_px: i32,
    pub(crate) orientation: i32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct TouchPoint {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PathSpec {
    pub(crate) points: Vec<TouchPoint>,
    pub(crate) duration_ms: u64,
}

impl TapSpec {
    pub(crate) const fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            hold_ms: DEFAULT_HOLD_MS,
            pressure: DEFAULT_PRESSURE,
            touch_major_px: DEFAULT_TOUCH_MAJOR,
            touch_minor_px: DEFAULT_TOUCH_MINOR,
            orientation: 0,
        }
    }
}

impl TouchPoint {
    pub(crate) const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl PathSpec {
    pub(crate) const fn new(points: Vec<TouchPoint>, duration_ms: u64) -> Self {
        Self {
            points,
            duration_ms,
        }
    }
}

pub(crate) fn doctor(app: &App) -> CliResult<Value> {
    let command = app.adb_shell(
        vec![
            String::from("command"),
            String::from("-v"),
            String::from("uinput"),
        ],
        FailureMode::AllowFailure,
    )?;
    let profile = discover_touchscreen_profile(app)?;
    Ok(json!({
        "ok": command.status_code == Some(0_i32),
        "input_backend": "uinput",
        "input_device_command": UINPUT_COMMAND,
        "uinput_command": command.json(),
        "physical_touchscreen": profile_json(&profile),
    }))
}

pub(crate) fn tap(app: &App, spec: TapSpec) -> CliResult<Value> {
    let profile = discover_touchscreen_profile(app)?;
    ensure_coordinate_in_range(&profile, spec)?;
    let stream = tap_stream(&profile, spec)?;
    let args = vec![
        String::from("shell"),
        String::from("uinput"),
        String::from("-"),
    ];
    let scoped_args = app.scoped_adb_args(&args)?;
    let output = run_process_with_stdin(
        app.adb_program(),
        &scoped_args,
        &stream,
        FailureMode::RequireSuccess,
    )?;
    Ok(json!({
        "ok": true,
        "input_backend": "uinput",
        "input_device_command": UINPUT_COMMAND,
        "tap": {
            "x": spec.x,
            "y": spec.y,
            "hold_ms": spec.hold_ms,
        },
        "physical_touchscreen": profile_summary_json(&profile),
        "stream_line_count": stream.lines().count(),
        "uinput": output.json(),
    }))
}

pub(crate) const fn input_device_command() -> &'static str {
    UINPUT_COMMAND
}

pub(crate) fn discover_touchscreen_profile(app: &App) -> CliResult<TouchscreenProfile> {
    select_primary_touchscreen_profile(&discover_touchscreen_profiles(app)?)
}

pub(crate) fn discover_touchscreen_profiles(app: &App) -> CliResult<Vec<TouchscreenProfile>> {
    let output = app.adb_shell(
        vec![String::from("getevent"), String::from("-il")],
        FailureMode::RequireSuccess,
    )?;
    parse_touchscreen_profiles(output.stdout())
}

pub(crate) fn select_primary_touchscreen_profile(
    profiles: &[TouchscreenProfile],
) -> CliResult<TouchscreenProfile> {
    profiles
        .iter()
        .max_by_key(|profile| profile_score(profile))
        .cloned()
        .ok_or_else(|| CliError::new("no direct touchscreen profile was found in getevent -il"))
}

#[cfg(test)]
fn parse_touchscreen_profile(text: &str) -> CliResult<TouchscreenProfile> {
    select_primary_touchscreen_profile(&parse_touchscreen_profiles(text)?)
}

fn parse_touchscreen_profiles(text: &str) -> CliResult<Vec<TouchscreenProfile>> {
    let mut devices = Vec::new();
    let mut current = None;
    for raw_line in text.lines() {
        if let Some(path) = parse_add_device_path(raw_line) {
            if let Some(device) = current.take() {
                devices.push(device);
            }
            current = Some(TouchscreenProfile {
                event_path: path,
                name: String::new(),
                location: String::new(),
                unique_id: String::new(),
                bus_hex: String::new(),
                bus_name: String::new(),
                vendor_id: 0,
                product_id: 0,
                version_id: 0,
                key_bits: Vec::new(),
                prop_bits: Vec::new(),
                abs_axes: Vec::new(),
            });
            continue;
        }
        let Some(device) = current.as_mut() else {
            continue;
        };
        parse_device_line(device, raw_line)?;
    }
    if let Some(device) = current {
        devices.push(device);
    }

    Ok(devices
        .into_iter()
        .filter(|device| device.prop_bits.contains(&INPUT_PROP_DIRECT))
        .filter(|device| device.has_abs_axis(ABS_MT_POSITION_X))
        .filter(|device| device.has_abs_axis(ABS_MT_POSITION_Y))
        .filter_map(with_supported_bus_name)
        .collect())
}

fn parse_device_line(device: &mut TouchscreenProfile, raw_line: &str) -> CliResult<()> {
    let line = raw_line.trim();
    if let Some(value) = line.strip_prefix("bus:") {
        device.bus_hex = String::from(value.trim());
        return Ok(());
    }
    if let Some(value) = line.strip_prefix("vendor") {
        device.vendor_id = parse_hex_i32(value.trim())?;
        return Ok(());
    }
    if let Some(value) = line.strip_prefix("product") {
        device.product_id = parse_hex_i32(value.trim())?;
        return Ok(());
    }
    if line.starts_with("version:") {
        return Ok(());
    }
    if let Some(value) = line.strip_prefix("version") {
        device.version_id = parse_hex_i32(value.trim())?;
        return Ok(());
    }
    if let Some(value) = line.strip_prefix("name:") {
        device.name = unquote(value.trim());
        return Ok(());
    }
    if let Some(value) = line.strip_prefix("location:") {
        device.location = unquote(value.trim());
        return Ok(());
    }
    if let Some(value) = line.strip_prefix("id:") {
        device.unique_id = unquote(value.trim());
        return Ok(());
    }
    if let Some(labels) = line
        .strip_prefix("KEY (0001):")
        .or_else(|| key_continuation_labels(line))
    {
        push_codes(&mut device.key_bits, labels, key_code);
        return Ok(());
    }
    if let Some(labels) = line.strip_prefix("input props:") {
        push_codes(&mut device.prop_bits, labels, input_prop_code);
        return Ok(());
    }
    if line.starts_with("INPUT_PROP_") || line == "<none>" {
        push_codes(&mut device.prop_bits, line, input_prop_code);
        return Ok(());
    }
    if line.starts_with("ABS_") || line.starts_with("ABS (0003):") {
        if let Some(axis) = parse_abs_axis(line)? {
            replace_axis(&mut device.abs_axes, axis);
        }
    }
    Ok(())
}

fn key_continuation_labels(line: &str) -> Option<&str> {
    if line.contains(':') || line.starts_with("ABS_") || line.starts_with("input props") {
        None
    } else if line.starts_with("KEY_") || line.starts_with("BTN_") {
        Some(line)
    } else {
        None
    }
}

fn parse_abs_axis(line: &str) -> CliResult<Option<AbsAxis>> {
    let normalized = line.strip_prefix("ABS (0003):").map_or(line, str::trim);
    let Some((name_text, info_text)) = normalized.split_once(':') else {
        return Ok(None);
    };
    let name = name_text.trim();
    let Some(code) = abs_code(name) else {
        return Ok(None);
    };
    Ok(Some(AbsAxis {
        code,
        name: String::from(name),
        value: labeled_number(info_text, "value")?,
        minimum: labeled_number(info_text, "min")?,
        maximum: labeled_number(info_text, "max")?,
        fuzz: labeled_number(info_text, "fuzz")?,
        flat: labeled_number(info_text, "flat")?,
        resolution: labeled_number(info_text, "resolution")?,
    }))
}

fn labeled_number(text: &str, label: &str) -> CliResult<i32> {
    let cleaned = text.replace(',', " ");
    let mut previous = "";
    for part in cleaned.split_whitespace() {
        if previous == label {
            return part
                .parse::<i32>()
                .map_err(|error| CliError::new(format!("invalid {label} value {part}: {error}")));
        }
        previous = part;
    }
    Err(CliError::new(format!("missing {label} in ABS axis info")))
}

pub(crate) fn tap_lines(profile: &TouchscreenProfile, spec: TapSpec) -> CliResult<Vec<String>> {
    let point = TouchPoint::new(spec.x, spec.y);
    ensure_point_in_range(profile, point)?;
    let press_events = down_events(profile, spec);
    let release_events = up_events();
    Ok(vec![
        inject_command_line(&press_events)?,
        delay_command_line(spec.hold_ms)?,
        inject_command_line(&release_events)?,
    ])
}

pub(crate) fn path_lines(profile: &TouchscreenProfile, spec: &PathSpec) -> CliResult<Vec<String>> {
    let segment_count = path_segment_count(spec)?;
    let segment_durations = segment_durations(spec.duration_ms, segment_count)?;
    let mut points = spec.points.iter().copied();
    let first_point = points
        .next()
        .ok_or_else(|| CliError::new("path requires at least two points"))?;
    ensure_point_in_range(profile, first_point)?;
    let first_spec = TapSpec::new(first_point.x, first_point.y);

    let mut lines = Vec::with_capacity(spec.points.len().saturating_mul(2).saturating_add(1));
    lines.push(inject_command_line(&down_events(profile, first_spec))?);
    for (point, delay_ms) in points.zip(segment_durations) {
        ensure_point_in_range(profile, point)?;
        lines.push(delay_command_line(delay_ms)?);
        lines.push(inject_command_line(&move_events(
            profile, point, first_spec,
        ))?);
    }
    lines.push(inject_command_line(&up_events())?);
    Ok(lines)
}

pub(crate) fn swipe_path_spec(
    from: TouchPoint,
    to: TouchPoint,
    duration_ms: u64,
    steps: u16,
) -> CliResult<PathSpec> {
    if steps == 0 {
        return Err(CliError::new("swipe requires at least one generated step"));
    }
    let mut points = Vec::with_capacity(usize::from(steps).saturating_add(1));
    for step in 0_u16..=steps {
        points.push(TouchPoint::new(
            interpolate_coordinate(from.x, to.x, step, steps)?,
            interpolate_coordinate(from.y, to.y, step, steps)?,
        ));
    }
    Ok(PathSpec::new(points, duration_ms))
}

pub(crate) fn x_coordinate_from_ratio(
    profile: &TouchscreenProfile,
    ratio: RatioPpm,
) -> CliResult<i32> {
    coordinate_from_ratio(profile, ABS_MT_POSITION_X, ratio, "x")
}

pub(crate) fn y_coordinate_from_ratio(
    profile: &TouchscreenProfile,
    ratio: RatioPpm,
) -> CliResult<i32> {
    coordinate_from_ratio(profile, ABS_MT_POSITION_Y, ratio, "y")
}

pub(crate) fn register_line(profile: &TouchscreenProfile) -> CliResult<String> {
    register_command_line(profile)
}

pub(crate) fn delay_line(duration_ms: u64) -> CliResult<String> {
    delay_command_line(duration_ms)
}

pub(crate) fn profile_summary(profile: &TouchscreenProfile) -> Value {
    profile_summary_json(profile)
}

pub(crate) fn profile_hash(profile: &TouchscreenProfile) -> CliResult<String> {
    let canonical_json = serde_json::to_vec(&canonical_profile_json(profile))?;
    let digest = Sha256::digest(canonical_json);
    Ok(hex_encode(&digest))
}

pub(crate) fn find_new_mirrored_touchscreen(
    before: &[TouchscreenProfile],
    after: &[TouchscreenProfile],
    physical: &TouchscreenProfile,
) -> Option<TouchscreenProfile> {
    after
        .iter()
        .filter(|candidate| !profile_path_seen(before, candidate.event_path()))
        .filter(|candidate| candidate.mirrors(physical))
        .max_by_key(|candidate| profile_score(candidate))
        .cloned()
}

pub(crate) fn touchscreen_event_path_exists(app: &App, event_path: &str) -> CliResult<bool> {
    Ok(discover_touchscreen_profiles(app)?
        .iter()
        .any(|profile| profile.event_path() == event_path))
}

fn tap_stream(profile: &TouchscreenProfile, spec: TapSpec) -> CliResult<String> {
    let tap_command_lines = tap_lines(profile, spec)?;
    let lines = [
        register_command_line(profile)?,
        delay_command_line(DEVICE_SETTLE_MS)?,
        tap_command_lines.join("\n"),
        delay_command_line(DEVICE_TAIL_MS)?,
    ];
    Ok(format!("{}\n", lines.join("\n")))
}

fn register_command_line(profile: &TouchscreenProfile) -> CliResult<String> {
    let mut key_bits = profile.key_bits.clone();
    key_bits.sort_unstable();
    key_bits.dedup();
    let mut prop_bits = profile.prop_bits.clone();
    prop_bits.sort_unstable();
    prop_bits.dedup();
    let abs_codes = sorted_abs_codes(profile);
    let abs_info = profile
        .abs_axes
        .iter()
        .map(abs_axis_register_entry)
        .collect::<Vec<AbsInfoEntry>>();
    let command = RegisterCommand {
        id: DEVICE_ID,
        command: "register",
        name: &profile.name,
        vid: profile.vendor_id,
        pid: profile.product_id,
        bus: &profile.bus_name,
        port: &profile.location,
        configuration: vec![
            ConfigurationEntry {
                kind: "UI_SET_EVBIT",
                data: vec![EV_KEY, EV_ABS],
            },
            ConfigurationEntry {
                kind: "UI_SET_KEYBIT",
                data: key_bits,
            },
            ConfigurationEntry {
                kind: "UI_SET_ABSBIT",
                data: abs_codes,
            },
            ConfigurationEntry {
                kind: "UI_SET_PROPBIT",
                data: prop_bits,
            },
        ],
        abs_info,
    };
    Ok(serde_json::to_string(&command)?)
}

fn down_events(profile: &TouchscreenProfile, spec: TapSpec) -> Vec<i32> {
    let point = TouchPoint::new(spec.x, spec.y);
    vec![
        EV_ABS,
        ABS_MT_SLOT,
        0,
        EV_ABS,
        ABS_MT_TRACKING_ID,
        TRACKING_ID,
        EV_ABS,
        ABS_MT_POSITION_X,
        point.x,
        EV_ABS,
        ABS_MT_POSITION_Y,
        point.y,
        EV_ABS,
        ABS_MT_TOUCH_MAJOR,
        axis_value(profile, ABS_MT_TOUCH_MAJOR, spec.touch_major_px),
        EV_ABS,
        ABS_MT_TOUCH_MINOR,
        axis_value(profile, ABS_MT_TOUCH_MINOR, spec.touch_minor_px),
        EV_ABS,
        ABS_MT_PRESSURE,
        axis_value(profile, ABS_MT_PRESSURE, spec.pressure),
        EV_ABS,
        ABS_MT_ORIENTATION,
        axis_value(profile, ABS_MT_ORIENTATION, spec.orientation),
        EV_ABS,
        ABS_X,
        point.x,
        EV_ABS,
        ABS_Y,
        point.y,
        EV_ABS,
        ABS_PRESSURE,
        axis_value(profile, ABS_PRESSURE, spec.pressure),
        EV_KEY,
        BTN_TOUCH,
        1,
        EV_KEY,
        BTN_TOOL_FINGER,
        1,
        EV_SYN,
        SYN_REPORT,
        0,
    ]
}

fn move_events(profile: &TouchscreenProfile, point: TouchPoint, spec: TapSpec) -> Vec<i32> {
    vec![
        EV_ABS,
        ABS_MT_SLOT,
        0,
        EV_ABS,
        ABS_MT_POSITION_X,
        point.x,
        EV_ABS,
        ABS_MT_POSITION_Y,
        point.y,
        EV_ABS,
        ABS_MT_TOUCH_MAJOR,
        axis_value(profile, ABS_MT_TOUCH_MAJOR, spec.touch_major_px),
        EV_ABS,
        ABS_MT_TOUCH_MINOR,
        axis_value(profile, ABS_MT_TOUCH_MINOR, spec.touch_minor_px),
        EV_ABS,
        ABS_MT_PRESSURE,
        axis_value(profile, ABS_MT_PRESSURE, spec.pressure),
        EV_ABS,
        ABS_X,
        point.x,
        EV_ABS,
        ABS_Y,
        point.y,
        EV_ABS,
        ABS_PRESSURE,
        axis_value(profile, ABS_PRESSURE, spec.pressure),
        EV_SYN,
        SYN_REPORT,
        0,
    ]
}

fn up_events() -> Vec<i32> {
    vec![
        EV_ABS,
        ABS_MT_SLOT,
        0,
        EV_ABS,
        ABS_MT_PRESSURE,
        0,
        EV_ABS,
        ABS_PRESSURE,
        0,
        EV_KEY,
        BTN_TOUCH,
        0,
        EV_KEY,
        BTN_TOOL_FINGER,
        0,
        EV_ABS,
        ABS_MT_TRACKING_ID,
        -1,
        EV_SYN,
        SYN_REPORT,
        0,
    ]
}

fn inject_command_line(events: &[i32]) -> CliResult<String> {
    let command = InjectCommand {
        id: DEVICE_ID,
        command: "inject",
        events,
    };
    Ok(serde_json::to_string(&command)?)
}

fn delay_command_line(duration_ms: u64) -> CliResult<String> {
    let command = DelayCommand {
        id: DEVICE_ID,
        command: "delay",
        duration: duration_ms,
    };
    Ok(serde_json::to_string(&command)?)
}

#[derive(Serialize)]
struct RegisterCommand<'a> {
    id: i64,
    command: &'static str,
    name: &'a str,
    vid: i32,
    pid: i32,
    bus: &'a str,
    port: &'a str,
    configuration: Vec<ConfigurationEntry>,
    abs_info: Vec<AbsInfoEntry>,
}

#[derive(Serialize)]
struct ConfigurationEntry {
    #[serde(rename = "type")]
    kind: &'static str,
    data: Vec<i32>,
}

#[derive(Serialize)]
struct AbsInfoEntry {
    code: i32,
    info: AbsAxisInfoEntry,
}

#[derive(Serialize)]
struct AbsAxisInfoEntry {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[derive(Serialize)]
struct InjectCommand<'a> {
    id: i64,
    command: &'static str,
    events: &'a [i32],
}

#[derive(Serialize)]
struct DelayCommand {
    id: i64,
    command: &'static str,
    duration: u64,
}

fn profile_json(profile: &TouchscreenProfile) -> Value {
    json!({
        "event_path": profile.event_path,
        "name": profile.name,
        "location": profile.location,
        "unique_id": profile.unique_id,
        "bus_hex": profile.bus_hex,
        "bus": profile.bus_name,
        "vendor_id": profile.vendor_id,
        "product_id": profile.product_id,
        "version_id": profile.version_id,
        "key_bits": profile.key_bits,
        "prop_bits": profile.prop_bits,
        "abs_axes": profile.abs_axes.iter().map(abs_axis_json).collect::<Vec<Value>>(),
    })
}

fn profile_summary_json(profile: &TouchscreenProfile) -> Value {
    json!({
        "event_path": profile.event_path,
        "name": profile.name,
        "location": profile.location,
        "unique_id": profile.unique_id,
        "bus_hex": profile.bus_hex,
        "bus": profile.bus_name,
        "vendor_id": profile.vendor_id,
        "product_id": profile.product_id,
        "version_id": profile.version_id,
        "x_range": axis_range_json(profile, ABS_MT_POSITION_X),
        "y_range": axis_range_json(profile, ABS_MT_POSITION_Y),
    })
}

fn canonical_profile_json(profile: &TouchscreenProfile) -> Value {
    let mut key_bits = profile.key_bits.clone();
    key_bits.sort_unstable();
    key_bits.dedup();
    let mut prop_bits = profile.prop_bits.clone();
    prop_bits.sort_unstable();
    prop_bits.dedup();
    let mut abs_axes = profile.abs_axes.clone();
    abs_axes.sort_by_key(|axis| axis.code);
    json!({
        "event_path": profile.event_path,
        "name": profile.name,
        "location": profile.location,
        "unique_id": profile.unique_id,
        "bus_hex": profile.bus_hex,
        "bus": profile.bus_name,
        "vendor_id": profile.vendor_id,
        "product_id": profile.product_id,
        "version_id": profile.version_id,
        "key_bits": key_bits,
        "prop_bits": prop_bits,
        "abs_axes": abs_axes
            .iter()
            .map(canonical_abs_axis_json)
            .collect::<Vec<Value>>(),
    })
}

fn canonical_abs_axis_json(axis: &AbsAxis) -> Value {
    json!({
        "code": axis.code,
        "name": axis.name,
        "info": {
            "minimum": axis.minimum,
            "maximum": axis.maximum,
            "fuzz": axis.fuzz,
            "flat": axis.flat,
            "resolution": axis.resolution,
        },
    })
}

fn abs_axis_json(axis: &AbsAxis) -> Value {
    json!({
        "code": axis.code,
        "name": axis.name,
        "info": abs_axis_info_json(axis),
    })
}

const fn abs_axis_register_entry(axis: &AbsAxis) -> AbsInfoEntry {
    AbsInfoEntry {
        code: axis.code,
        info: AbsAxisInfoEntry {
            value: axis.value,
            minimum: axis.minimum,
            maximum: axis.maximum,
            fuzz: axis.fuzz,
            flat: axis.flat,
            resolution: axis.resolution,
        },
    }
}

fn abs_axis_info_json(axis: &AbsAxis) -> Value {
    json!({
        "value": axis.value,
        "minimum": axis.minimum,
        "maximum": axis.maximum,
        "fuzz": axis.fuzz,
        "flat": axis.flat,
        "resolution": axis.resolution,
    })
}

fn axis_range_json(profile: &TouchscreenProfile, code: i32) -> Value {
    profile.axis(code).map_or(Value::Null, |axis| {
        json!({
            "minimum": axis.minimum,
            "maximum": axis.maximum,
        })
    })
}

fn ensure_coordinate_in_range(profile: &TouchscreenProfile, spec: TapSpec) -> CliResult<()> {
    ensure_point_in_range(profile, TouchPoint::new(spec.x, spec.y))
}

fn ensure_point_in_range(profile: &TouchscreenProfile, point: TouchPoint) -> CliResult<()> {
    ensure_axis_coordinate(profile, ABS_MT_POSITION_X, point.x, "x")?;
    ensure_axis_coordinate(profile, ABS_MT_POSITION_Y, point.y, "y")
}

fn ensure_axis_coordinate(
    profile: &TouchscreenProfile,
    axis_code: i32,
    value: i32,
    label: &str,
) -> CliResult<()> {
    let axis = profile
        .axis(axis_code)
        .ok_or_else(|| CliError::new(format!("touchscreen profile is missing {label} axis")))?;
    if value < axis.minimum || value > axis.maximum {
        return Err(CliError::new(format!(
            "{label} coordinate {value} is outside {}..{}",
            axis.minimum, axis.maximum
        )));
    }
    Ok(())
}

fn coordinate_from_ratio(
    profile: &TouchscreenProfile,
    axis_code: i32,
    ratio: RatioPpm,
    label: &str,
) -> CliResult<i32> {
    if ratio.ppm() > RATIO_SCALE_PPM {
        return Err(CliError::new(format!(
            "{label} ratio {} ppm is outside 0..{RATIO_SCALE_PPM}",
            ratio.ppm()
        )));
    }
    let axis = profile
        .axis(axis_code)
        .ok_or_else(|| CliError::new(format!("touchscreen profile is missing {label} axis")))?;
    let span = i64::from(axis.maximum)
        .checked_sub(i64::from(axis.minimum))
        .ok_or_else(|| CliError::new(format!("{label} axis range overflowed")))?;
    let scaled = span
        .checked_mul(i64::from(ratio.ppm()))
        .ok_or_else(|| CliError::new(format!("{label} ratio multiplication overflowed")))?;
    let rounded = scaled
        .checked_add(RATIO_HALF_SCALE_PPM_I64)
        .ok_or_else(|| CliError::new(format!("{label} ratio rounding overflowed")))?
        .checked_div(RATIO_SCALE_PPM_I64)
        .ok_or_else(|| CliError::new(format!("{label} ratio division failed")))?;
    let coordinate = i64::from(axis.minimum)
        .checked_add(rounded)
        .ok_or_else(|| CliError::new(format!("{label} coordinate addition overflowed")))?;
    i32::try_from(coordinate)
        .map_err(|error| CliError::new(format!("{label} coordinate conversion failed: {error}")))
}

fn path_segment_count(spec: &PathSpec) -> CliResult<usize> {
    let Some(segment_count) = spec.points.len().checked_sub(1) else {
        return Err(CliError::new("path requires at least two points"));
    };
    if segment_count == 0 {
        return Err(CliError::new("path requires at least two points"));
    }
    Ok(segment_count)
}

fn segment_durations(duration_ms: u64, segment_count: usize) -> CliResult<Vec<u64>> {
    let segment_count_u64 = u64::try_from(segment_count).map_err(|error| {
        CliError::new(format!(
            "path segment count conversion failed for {segment_count}: {error}"
        ))
    })?;
    let base = duration_ms
        .checked_div(segment_count_u64)
        .ok_or_else(|| CliError::new("path segment duration division failed"))?;
    let remainder = duration_ms
        .checked_rem(segment_count_u64)
        .ok_or_else(|| CliError::new("path segment duration remainder failed"))?;
    let mut durations = Vec::with_capacity(segment_count);
    for index in 0_usize..segment_count {
        let index_u64 = u64::try_from(index)
            .map_err(|error| CliError::new(format!("path segment index overflowed: {error}")))?;
        let extra = u64::from(index_u64 < remainder);
        durations.push(
            base.checked_add(extra)
                .ok_or_else(|| CliError::new("path segment duration addition overflowed"))?,
        );
    }
    Ok(durations)
}

fn interpolate_coordinate(start: i32, end: i32, step: u16, steps: u16) -> CliResult<i32> {
    if steps == 0 {
        return Err(CliError::new("coordinate interpolation requires steps > 0"));
    }
    let delta = i64::from(end)
        .checked_sub(i64::from(start))
        .ok_or_else(|| CliError::new("coordinate interpolation subtraction overflowed"))?;
    let numerator = delta
        .checked_mul(i64::from(step))
        .ok_or_else(|| CliError::new("coordinate interpolation multiplication overflowed"))?;
    let offset = numerator
        .checked_div(i64::from(steps))
        .ok_or_else(|| CliError::new("coordinate interpolation division failed"))?;
    let value = i64::from(start)
        .checked_add(offset)
        .ok_or_else(|| CliError::new("coordinate interpolation addition overflowed"))?;
    i32::try_from(value)
        .map_err(|error| CliError::new(format!("coordinate interpolation failed: {error}")))
}

impl TouchscreenProfile {
    pub(crate) fn event_path(&self) -> &str {
        &self.event_path
    }

    fn mirrors(&self, physical: &Self) -> bool {
        self.event_path != physical.event_path
            && self.name == physical.name
            && self.location == physical.location
            && self.bus_hex == physical.bus_hex
            && self.bus_name == physical.bus_name
            && self.vendor_id == physical.vendor_id
            && self.product_id == physical.product_id
            && self.version_id == physical.version_id
            && self.prop_bits.contains(&INPUT_PROP_DIRECT)
            && axes_match(self, physical, ABS_MT_POSITION_X)
            && axes_match(self, physical, ABS_MT_POSITION_Y)
            && axes_match(self, physical, ABS_MT_SLOT)
            && axes_match(self, physical, ABS_MT_TRACKING_ID)
    }

    fn has_abs_axis(&self, code: i32) -> bool {
        self.abs_axes.iter().any(|axis| axis.code == code)
    }

    fn axis(&self, code: i32) -> Option<&AbsAxis> {
        self.abs_axes.iter().find(|axis| axis.code == code)
    }
}

fn axes_match(left: &TouchscreenProfile, right: &TouchscreenProfile, code: i32) -> bool {
    match (left.axis(code), right.axis(code)) {
        (Some(left_axis), Some(right_axis)) => {
            left_axis.minimum == right_axis.minimum
                && left_axis.maximum == right_axis.maximum
                && left_axis.fuzz == right_axis.fuzz
                && left_axis.flat == right_axis.flat
                && left_axis.resolution == right_axis.resolution
        }
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
    }
}

fn profile_path_seen(profiles: &[TouchscreenProfile], event_path: &str) -> bool {
    profiles
        .iter()
        .any(|profile| profile.event_path() == event_path)
}

fn axis_value(profile: &TouchscreenProfile, code: i32, preferred: i32) -> i32 {
    profile.axis(code).map_or(preferred, |axis| {
        preferred.clamp(axis.minimum, axis.maximum)
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(hex_char(byte >> 4));
        encoded.push(hex_char(byte & 0x0f));
    }
    encoded
}

fn hex_char(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0'.saturating_add(nibble)),
        10..=15 => char::from(b'a'.saturating_add(nibble.saturating_sub(10))),
        _ => '0',
    }
}

fn sorted_abs_codes(profile: &TouchscreenProfile) -> Vec<i32> {
    let mut codes = profile
        .abs_axes
        .iter()
        .map(|axis| axis.code)
        .collect::<Vec<i32>>();
    codes.sort_unstable();
    codes.dedup();
    codes
}

fn replace_axis(axes: &mut Vec<AbsAxis>, axis: AbsAxis) {
    if let Some(existing) = axes
        .iter_mut()
        .find(|candidate| candidate.code == axis.code)
    {
        *existing = axis;
    } else {
        axes.push(axis);
        axes.sort_by_key(|candidate| candidate.code);
    }
}

fn push_codes<F>(target: &mut Vec<i32>, labels: &str, lookup: F)
where
    F: Fn(&str) -> Option<i32>,
{
    for label in labels.split_whitespace() {
        if let Some(code) = lookup(label) {
            target.push(code);
        }
    }
    target.sort_unstable();
    target.dedup();
}

fn profile_score(profile: &TouchscreenProfile) -> usize {
    profile
        .axis(ABS_MT_POSITION_X)
        .and_then(|x_axis| {
            profile.axis(ABS_MT_POSITION_Y).map(|y_axis| {
                usize::try_from(x_axis.maximum.saturating_add(y_axis.maximum)).unwrap_or(0)
            })
        })
        .unwrap_or(0)
}

fn with_supported_bus_name(mut profile: TouchscreenProfile) -> Option<TouchscreenProfile> {
    let bus = bus_name(&profile.bus_hex).ok()?;
    profile.bus_name = bus;
    Some(profile)
}

fn parse_add_device_path(line: &str) -> Option<String> {
    let (_, path) = line.split_once(": /dev/input/")?;
    Some(format!("/dev/input/{}", path.trim()))
}

fn parse_hex_i32(value: &str) -> CliResult<i32> {
    let token = value
        .split_whitespace()
        .next()
        .ok_or_else(|| CliError::new("missing hexadecimal value"))?;
    i32::from_str_radix(token, 16)
        .map_err(|error| CliError::new(format!("invalid hexadecimal value {token}: {error}")))
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(value)
        .to_owned()
}

fn bus_name(bus_hex: &str) -> CliResult<String> {
    match parse_hex_i32(bus_hex)? {
        0x03 => Ok(String::from("USB")),
        0x05 => Ok(String::from("BLUETOOTH")),
        0x06 => Ok(String::from("VIRTUAL")),
        0x18 => Ok(String::from("I2C")),
        0x19 => Ok(String::from("HOST")),
        0x1c => Ok(String::from("SPI")),
        value => Err(CliError::new(format!(
            "unsupported touchscreen bus id {value:#x}"
        ))),
    }
}

fn key_code(label: &str) -> Option<i32> {
    match label {
        "KEY_HOMEPAGE" => Some(KEY_HOMEPAGE),
        "BTN_TOOL_FINGER" => Some(BTN_TOOL_FINGER),
        "BTN_TOUCH" => Some(BTN_TOUCH),
        _ => None,
    }
}

fn input_prop_code(label: &str) -> Option<i32> {
    match label {
        "INPUT_PROP_DIRECT" => Some(INPUT_PROP_DIRECT),
        _ => None,
    }
}

fn abs_code(label: &str) -> Option<i32> {
    match label {
        "ABS_X" => Some(ABS_X),
        "ABS_Y" => Some(ABS_Y),
        "ABS_PRESSURE" => Some(ABS_PRESSURE),
        "ABS_MT_SLOT" => Some(ABS_MT_SLOT),
        "ABS_MT_TOUCH_MAJOR" => Some(ABS_MT_TOUCH_MAJOR),
        "ABS_MT_TOUCH_MINOR" => Some(ABS_MT_TOUCH_MINOR),
        "ABS_MT_ORIENTATION" => Some(ABS_MT_ORIENTATION),
        "ABS_MT_POSITION_X" => Some(ABS_MT_POSITION_X),
        "ABS_MT_POSITION_Y" => Some(ABS_MT_POSITION_Y),
        "ABS_MT_TOOL_TYPE" => Some(ABS_MT_TOOL_TYPE),
        "ABS_MT_TRACKING_ID" => Some(ABS_MT_TRACKING_ID),
        "ABS_MT_PRESSURE" => Some(ABS_MT_PRESSURE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use proptest::strategy::Strategy;

    use crate::error::{CliError, CliResult};
    use crate::uinput::{
        ABS_MT_POSITION_X, ABS_MT_POSITION_Y, PathSpec, TapSpec, TouchPoint, bus_name,
        find_new_mirrored_touchscreen, parse_touchscreen_profile, parse_touchscreen_profiles,
        path_lines, profile_hash, swipe_path_spec, tap_stream,
    };

    const SAMPLE_GETEVENT: &str = r#"
add device 1: /dev/input/event3
  bus:      001c
  vendor    0000
  product   0000
  version   0000
  name:     "sec_touchscreen"
  location: "sec_touchscreen/input1"
  id:       "google_touchscreen"
  version:  1.0.1
  events:
    KEY (0001): KEY_HOMEPAGE          BTN_TOOL_FINGER       BTN_TOUCH
    ABS (0003): ABS_X                 : value 0, min 0, max 1439, fuzz 0, flat 0, resolution 0
                ABS_Y                 : value 0, min 0, max 3119, fuzz 0, flat 0, resolution 0
                ABS_PRESSURE          : value 0, min 0, max 63, fuzz 0, flat 0, resolution 0
                ABS_MT_SLOT           : value 0, min 0, max 9, fuzz 0, flat 0, resolution 0
                ABS_MT_TOUCH_MAJOR    : value 0, min 0, max 5100, fuzz 0, flat 0, resolution 0
                ABS_MT_TOUCH_MINOR    : value 0, min 0, max 5100, fuzz 0, flat 0, resolution 0
                ABS_MT_ORIENTATION    : value 0, min -4096, max 4096, fuzz 0, flat 0, resolution 0
                ABS_MT_POSITION_X     : value 0, min 0, max 1439, fuzz 0, flat 0, resolution 0
                ABS_MT_POSITION_Y     : value 0, min 0, max 3119, fuzz 0, flat 0, resolution 0
                ABS_MT_TOOL_TYPE      : value 0, min 0, max 0, fuzz 0, flat 0, resolution 0
                ABS_MT_TRACKING_ID    : value 0, min 0, max 65535, fuzz 0, flat 0, resolution 0
                ABS_MT_PRESSURE       : value 0, min 0, max 63, fuzz 0, flat 0, resolution 0
  input props:
    INPUT_PROP_DIRECT
"#;

    const UNSUPPORTED_NON_TOUCH_GETEVENT: &str = r#"
add device 2: /dev/input/event2
  bus:      0000
  vendor    0000
  product   0000
  version   0000
  name:     "distance sensor"
  location: ""
  id:       ""
  version:  1.0.1
  events:
    ABS (0003): ABS_DISTANCE          : value 0, min 0, max 255, fuzz 0, flat 0, resolution 0
  input props:
    <none>
"#;

    #[test]
    fn parser_selects_direct_touchscreen_profile() {
        let profile = parse_touchscreen_profile(SAMPLE_GETEVENT);

        assert!(profile.is_ok(), "sample touchscreen profile should parse");
        let parsed = profile.ok();
        assert_eq!(
            parsed
                .as_ref()
                .map(|parsed_profile| parsed_profile.event_path.as_str()),
            Some("/dev/input/event3"),
            "event path should be parsed"
        );
        assert_eq!(
            parsed
                .as_ref()
                .map(|parsed_profile| parsed_profile.name.as_str()),
            Some("sec_touchscreen"),
            "device name should be parsed"
        );
        assert!(
            parsed
                .as_ref()
                .is_some_and(|parsed_profile| parsed_profile.has_abs_axis(ABS_MT_POSITION_X)),
            "x axis should be present"
        );
        assert!(
            parsed
                .as_ref()
                .is_some_and(|parsed_profile| parsed_profile.has_abs_axis(ABS_MT_POSITION_Y)),
            "y axis should be present"
        );
    }

    #[test]
    fn parser_ignores_unsupported_non_touch_bus() {
        let getevent = format!("{SAMPLE_GETEVENT}\n{UNSUPPORTED_NON_TOUCH_GETEVENT}");
        let profile = parse_touchscreen_profile(&getevent);

        assert!(profile.is_ok(), "non-touch devices should not fail parsing");
        let parsed = profile.ok();
        assert_eq!(
            parsed
                .as_ref()
                .map(|parsed_profile| parsed_profile.event_path.as_str()),
            Some("/dev/input/event3"),
            "the direct touchscreen should still be selected"
        );
    }

    #[test]
    fn tap_stream_contains_register_and_protocol_b_events() {
        let profile = parse_touchscreen_profile(SAMPLE_GETEVENT);
        assert!(profile.is_ok(), "sample profile should parse");
        let stream = profile.and_then(|parsed| tap_stream(&parsed, TapSpec::new(145, 2387)));

        assert!(stream.is_ok(), "tap stream should render");
        let rendered = stream.unwrap_or_default();
        assert!(
            rendered.contains("\"command\":\"register\""),
            "stream should register a device"
        );
        assert!(
            !rendered.contains("\"name\":\"ABS_"),
            "register abs_info entries should only contain fields accepted by AOSP uinput"
        );
        assert!(
            rendered.contains("\"command\":\"inject\""),
            "stream should inject events"
        );
        assert!(
            rendered.contains("57,100"),
            "stream should include tracking id down"
        );
        assert!(
            rendered.contains("57,-1"),
            "stream should include tracking id release"
        );
    }

    #[test]
    fn profile_hash_ignores_current_axis_values() {
        let original = parse_touchscreen_profile(SAMPLE_GETEVENT);
        assert!(original.is_ok(), "sample profile should parse");
        let changed_values = SAMPLE_GETEVENT.replace(
            "ABS_MT_POSITION_X     : value 0, min 0, max 1439",
            "ABS_MT_POSITION_X     : value 321, min 0, max 1439",
        );
        let changed = parse_touchscreen_profile(&changed_values);
        assert!(changed.is_ok(), "changed-value profile should parse");

        let original_hash = original.and_then(|profile| profile_hash(&profile));
        let changed_hash = changed.and_then(|profile| profile_hash(&profile));

        assert_eq!(
            original_hash.ok(),
            changed_hash.ok(),
            "profile hash should describe stable capability metadata, not live axis values"
        );
    }

    #[test]
    fn finds_new_mirrored_touchscreen_profile() -> CliResult<()> {
        let before_profiles = parse_touchscreen_profiles(SAMPLE_GETEVENT)?;
        let physical = before_profiles
            .first()
            .cloned()
            .ok_or_else(|| CliError::new("physical profile should be present"))?;
        let virtual_getevent = SAMPLE_GETEVENT.replace(
            "add device 1: /dev/input/event3",
            "add device 5: /dev/input/event4",
        );
        let after_getevent = format!("{SAMPLE_GETEVENT}\n{virtual_getevent}");
        let after_profiles = parse_touchscreen_profiles(&after_getevent)?;

        let detected = find_new_mirrored_touchscreen(&before_profiles, &after_profiles, &physical);
        let detected_event_path = detected.map(|profile| profile.event_path);

        if detected_event_path != Some(String::from("/dev/input/event4")) {
            return Err(CliError::new(format!(
                "new mirrored event node should be selected, got {detected_event_path:?}"
            )));
        }
        Ok(())
    }

    proptest::proptest! {
        #[test]
        fn known_bus_ids_map_to_labels(bus in known_bus_id()) {
            let hex = format!("{bus:04x}");
            let label = bus_name(&hex);

            proptest::prop_assert!(
                label.is_ok(),
                "known bus id should map to an AOSP uinput label"
            );
        }

        #[test]
        fn generated_swipe_paths_preserve_endpoints(
            from_x in 0_i32..=1439_i32,
            from_y in 0_i32..=3119_i32,
            to_x in 0_i32..=1439_i32,
            to_y in 0_i32..=3119_i32,
            duration_ms in 0_u64..=5_000_u64,
            steps in 1_u16..=64_u16,
        ) {
            let from = TouchPoint::new(from_x, from_y);
            let to = TouchPoint::new(to_x, to_y);
            let spec = swipe_path_spec(from, to, duration_ms, steps);

            proptest::prop_assert!(spec.is_ok(), "generated swipe should be valid");
            if let Ok(parsed_spec) = spec {
                proptest::prop_assert_eq!(parsed_spec.points.first().copied(), Some(from));
                proptest::prop_assert_eq!(parsed_spec.points.last().copied(), Some(to));
                proptest::prop_assert_eq!(
                    parsed_spec.points.len(),
                    usize::from(steps).saturating_add(1_usize)
                );
                proptest::prop_assert_eq!(parsed_spec.duration_ms, duration_ms);
            }
        }

        #[test]
        fn in_range_paths_render_to_uinput_stream(
            from_x in 0_i32..=1439_i32,
            from_y in 0_i32..=3119_i32,
            to_x in 0_i32..=1439_i32,
            to_y in 0_i32..=3119_i32,
            duration_ms in 0_u64..=5_000_u64,
        ) {
            let profile = parse_touchscreen_profile(SAMPLE_GETEVENT);
            proptest::prop_assert!(profile.is_ok(), "sample profile should parse");
            if let Ok(parsed_profile) = profile {
                let spec = PathSpec::new(
                    vec![TouchPoint::new(from_x, from_y), TouchPoint::new(to_x, to_y)],
                    duration_ms,
                );
                let rendered = path_lines(&parsed_profile, &spec);

                proptest::prop_assert!(rendered.is_ok(), "in-range path should render");
                if let Ok(lines) = rendered {
                    proptest::prop_assert!(
                        lines.iter().any(|line| line.contains("\"command\":\"inject\"")),
                        "path stream should contain inject commands"
                    );
                    proptest::prop_assert!(
                        lines.iter().any(|line| line.contains("\"command\":\"delay\"")),
                        "path stream should contain delay commands"
                    );
                }
            }
        }
    }

    fn known_bus_id() -> impl Strategy<Value = i32> {
        proptest::sample::select(vec![0x03, 0x05, 0x06, 0x18, 0x19, 0x1c])
    }
}
