//! AOSP uinput command-stream support.

use serde::Serialize;
use serde_json::{Value, json};

use crate::app::App;
use crate::error::{CliError, CliResult};
use crate::process::{FailureMode, run_process_with_stdin};

const UINPUT_COMMAND: &str = "/system/bin/uinput";
const DEVICE_ID: i64 = 1;
const DEFAULT_HOLD_MS: u64 = 70;
const DEVICE_SETTLE_MS: u64 = 1_000;
const DEVICE_TAIL_MS: u64 = 1_000;
const TRACKING_ID: i32 = 100;
const DEFAULT_TOUCH_MAJOR: i32 = 120;
const DEFAULT_TOUCH_MINOR: i32 = 80;
const DEFAULT_PRESSURE: i32 = 30;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TapSpec {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) hold_ms: u64,
}

impl TapSpec {
    pub(crate) const fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            hold_ms: DEFAULT_HOLD_MS,
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
    let output = run_process_with_stdin(
        app.adb_program(),
        &args,
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

pub(crate) fn discover_touchscreen_profile(app: &App) -> CliResult<TouchscreenProfile> {
    let output = app.adb_shell(
        vec![String::from("getevent"), String::from("-il")],
        FailureMode::RequireSuccess,
    )?;
    parse_touchscreen_profile(output.stdout())
}

fn parse_touchscreen_profile(text: &str) -> CliResult<TouchscreenProfile> {
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

    devices
        .into_iter()
        .filter(|device| device.prop_bits.contains(&INPUT_PROP_DIRECT))
        .filter(|device| device.has_abs_axis(ABS_MT_POSITION_X))
        .filter(|device| device.has_abs_axis(ABS_MT_POSITION_Y))
        .filter_map(with_supported_bus_name)
        .max_by_key(profile_score)
        .ok_or_else(|| CliError::new("no direct touchscreen profile was found in getevent -il"))
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

fn tap_stream(profile: &TouchscreenProfile, spec: TapSpec) -> CliResult<String> {
    let press_events = down_events(profile, spec);
    let release_events = up_events();
    let lines = [
        register_command_line(profile)?,
        delay_command_line(DEVICE_SETTLE_MS)?,
        inject_command_line(&press_events)?,
        delay_command_line(spec.hold_ms)?,
        inject_command_line(&release_events)?,
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
    vec![
        EV_ABS,
        ABS_MT_SLOT,
        0,
        EV_ABS,
        ABS_MT_TRACKING_ID,
        TRACKING_ID,
        EV_ABS,
        ABS_MT_POSITION_X,
        spec.x,
        EV_ABS,
        ABS_MT_POSITION_Y,
        spec.y,
        EV_ABS,
        ABS_MT_TOUCH_MAJOR,
        axis_value(profile, ABS_MT_TOUCH_MAJOR, DEFAULT_TOUCH_MAJOR),
        EV_ABS,
        ABS_MT_TOUCH_MINOR,
        axis_value(profile, ABS_MT_TOUCH_MINOR, DEFAULT_TOUCH_MINOR),
        EV_ABS,
        ABS_MT_PRESSURE,
        axis_value(profile, ABS_MT_PRESSURE, DEFAULT_PRESSURE),
        EV_ABS,
        ABS_MT_ORIENTATION,
        0,
        EV_ABS,
        ABS_X,
        spec.x,
        EV_ABS,
        ABS_Y,
        spec.y,
        EV_ABS,
        ABS_PRESSURE,
        axis_value(profile, ABS_PRESSURE, DEFAULT_PRESSURE),
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
    ensure_axis_coordinate(profile, ABS_MT_POSITION_X, spec.x, "x")?;
    ensure_axis_coordinate(profile, ABS_MT_POSITION_Y, spec.y, "y")
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

impl TouchscreenProfile {
    fn has_abs_axis(&self, code: i32) -> bool {
        self.abs_axes.iter().any(|axis| axis.code == code)
    }

    fn axis(&self, code: i32) -> Option<&AbsAxis> {
        self.abs_axes.iter().find(|axis| axis.code == code)
    }
}

fn axis_value(profile: &TouchscreenProfile, code: i32, preferred: i32) -> i32 {
    profile.axis(code).map_or(preferred, |axis| {
        preferred.clamp(axis.minimum, axis.maximum)
    })
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

    use crate::uinput::{
        ABS_MT_POSITION_X, ABS_MT_POSITION_Y, TapSpec, bus_name, parse_touchscreen_profile,
        tap_stream,
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
    }

    fn known_bus_id() -> impl Strategy<Value = i32> {
        proptest::sample::select(vec![0x03, 0x05, 0x06, 0x18, 0x19, 0x1c])
    }
}
