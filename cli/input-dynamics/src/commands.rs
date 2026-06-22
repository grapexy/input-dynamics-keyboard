//! Command implementations.

use std::cmp::Reverse;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use input_dynamics_analysis::derivation::{
    DeriveDismissalsConfig, DeriveTimelineConfig, derive_dismissals as run_derive_dismissals,
    derive_timeline as run_derive_timeline,
};
use input_dynamics_analysis::getevent::{GETEVENT_SCHEMA, NormalizeStats, normalize_file};
use serde_json::{Value, json};

use crate::app::{App, LOG_DIR};
use crate::args::{
    Commands, ControllerCommand, DeriveCommand, EdgeSide, GeteventCommand, HideKeyboardMethod,
    ObserveCommand, PressKey, RecordingCommand, SessionCommand, TouchCommand,
};
use crate::controller::{self, RunConfig, SessionStartPermit};
use crate::coordinate_frame::screen_config_from_run_manifest;
use crate::derivation_policy;
use crate::error::{CliError, CliResult};
use crate::layout::{json_number_to_shell_arg, key_matches};
use crate::observe;
use crate::process::{FailureMode, run_process};
use crate::profile::{
    self, InterKeyDelaySampling, KeyProfileContext, ProfileProvenance, RuntimeProfile,
};
use crate::ratio::{RatioPpm, SignedRatioPpm};
use crate::record::{RecordConfig, record_run};
use crate::recording::inspect_recording;
use crate::uinput::{self, PathSpec, TapSpec, TouchPoint};
use crate::validate::validate_logs;

const LAYOUT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const LAYOUT_POLL_INTERVAL: Duration = Duration::from_millis(50);

struct LoggingStartConfig<'a> {
    run_id: &'a str,
    input_actor: &'a str,
    input_controller: Option<&'a str>,
    input_cadence_policy: &'a str,
    input_profile: Option<&'a ProfileProvenance>,
}

struct SessionStartConfig<'a> {
    run_id: &'a str,
    input_actor: &'a str,
    input_controller: &'a str,
    input_cadence_policy: &'a str,
    input_profile_path: Option<&'a Path>,
    input_profile_seed: Option<u64>,
}

pub(crate) fn run_command(app: &App, cli_command: Commands) -> CliResult<Value> {
    match cli_command {
        Commands::Doctor => doctor(app),
        Commands::Install { apk, repo, dir } => install(app, apk.as_deref(), &repo, &dir),
        Commands::SelectIme => select_ime(app),
        Commands::EnableLogging => app.broadcast("ENABLE", Vec::new()),
        Commands::DisableLogging => app.broadcast("DISABLE", Vec::new()),
        Commands::Start {
            run_id,
            input_actor,
            input_controller,
            input_cadence_policy,
        } => start(
            app,
            &LoggingStartConfig {
                run_id: &run_id,
                input_actor: &input_actor,
                input_controller: input_controller.as_deref(),
                input_cadence_policy: &input_cadence_policy,
                input_profile: None,
            },
        ),
        Commands::Stop => app.broadcast("STOP", Vec::new()),
        Commands::Status => app.broadcast("STATUS", Vec::new()),
        Commands::Layout {
            wait_visible,
            wait_hidden,
        } => {
            let wait = if wait_visible {
                LayoutWait::Visible
            } else if wait_hidden {
                LayoutWait::Hidden
            } else {
                LayoutWait::Current
            };
            layout(app, wait)
        }
        Commands::Observe { command } => observe_command(app, command),
        ref hide_command @ Commands::HideKeyboard { .. } => {
            hide_keyboard_command(app, hide_command)
        }
        Commands::ListLogs => app.broadcast("LIST_LOGS", Vec::new()),
        Commands::ClearLogs => app.broadcast("CLEAR_LOGS", Vec::new()),
        Commands::Pull { out } => pull_logs(app, &out),
        Commands::Validate { path, run_id } => validate_logs(&path, run_id.as_deref()),
        Commands::Getevent { command } => getevent(command),
        Commands::Derive { command } => derive(command),
        Commands::Recording { command } => recording(command),
        ref record_command @ Commands::Record { .. } => run_record_command(app, record_command),
        Commands::Session {
            command: session_command,
        } => session(app, session_command),
        Commands::Tap { label, code } => tap_key(app, label.as_deref(), code),
        Commands::Press { key } => press_key(app, key),
        Commands::Type {
            text,
            inter_key_delay_ms,
        } => type_text(app, &text, inter_key_delay_ms),
        Commands::Touch {
            command: touch_command,
        } => touch(app, &touch_command),
        Commands::Controller {
            command: controller_command,
        } => run_controller_command(app, controller_command),
    }
}

fn recording(command: RecordingCommand) -> CliResult<Value> {
    match command {
        RecordingCommand::Inspect { dir } => inspect_recording(&dir),
    }
}

fn observe_command(app: &App, command: ObserveCommand) -> CliResult<Value> {
    match command {
        ObserveCommand::Accessibility { out, full } => observe::accessibility(
            app,
            out.as_deref(),
            if full {
                observe::AccessibilityDetail::Full
            } else {
                observe::AccessibilityDetail::Compressed
            },
        ),
        ObserveCommand::Screenshot { out } => observe::screenshot(app, &out),
        ObserveCommand::Layout {
            wait_visible,
            wait_hidden,
        } => {
            let wait = if wait_visible {
                LayoutWait::Visible
            } else if wait_hidden {
                LayoutWait::Hidden
            } else {
                LayoutWait::Current
            };
            layout(app, wait)
        }
        ObserveCommand::State {
            with_accessibility,
            screenshot_out,
            full_accessibility,
        } => observe::state(
            app,
            observe::StateOptions {
                include_accessibility: with_accessibility,
                screenshot_out: screenshot_out.as_deref(),
                accessibility_detail: if full_accessibility {
                    observe::AccessibilityDetail::Full
                } else {
                    observe::AccessibilityDetail::Compressed
                },
            },
        ),
        ObserveCommand::All {
            out_dir,
            full_accessibility,
        } => observe::all(
            app,
            &out_dir,
            if full_accessibility {
                observe::AccessibilityDetail::Full
            } else {
                observe::AccessibilityDetail::Compressed
            },
        ),
    }
}

fn run_record_command(app: &App, command: &Commands) -> CliResult<Value> {
    let &Commands::Record {
        ref run_id,
        ref out,
        duration_ms,
        with_input_controller,
        with_evidence,
        full_accessibility_evidence,
        ref input_actor,
        ref input_controller,
        ref input_cadence_policy,
    } = command
    else {
        return Err(CliError::new("expected record command"));
    };
    let config = RecordConfig {
        run_id: run_id.clone(),
        out: out.clone(),
        duration_ms,
        with_input_controller,
        with_evidence,
        full_accessibility_evidence,
        input_actor: input_actor.clone(),
        input_controller: input_controller.clone(),
        input_cadence_policy: input_cadence_policy.clone(),
    };
    record_run(app, &config)
}

fn derive(command: DeriveCommand) -> CliResult<Value> {
    match command {
        DeriveCommand::Dismissals {
            recording_dir,
            policy,
            getevent_jsonl,
            ime_jsonl,
            touch_gestures_output,
            dismissals_output,
        } => {
            let screen = screen_config_from_run_manifest(&recording_dir)?;
            let loaded_policy = derivation_policy::load(policy.as_deref())?;
            run_derive_dismissals(&DeriveDismissalsConfig {
                recording_dir,
                getevent_jsonl,
                ime_jsonl,
                touch_gestures_output,
                dismissals_output,
                screen,
                policy: loaded_policy.policy,
                policy_summary: Some(loaded_policy.summary),
            })
            .map_err(CliError::from)
        }
        DeriveCommand::Timeline {
            recording_dir,
            ime_jsonl,
            touch_gestures_jsonl,
            dismissals_jsonl,
            output_dir,
        } => run_derive_timeline(&DeriveTimelineConfig {
            recording_dir,
            ime_jsonl,
            touch_gestures_jsonl,
            dismissals_jsonl,
            output_dir,
        })
        .map_err(CliError::from),
    }
}

fn getevent(command: GeteventCommand) -> CliResult<Value> {
    match command {
        GeteventCommand::Normalize { input, output } => {
            let stats = normalize_file(&input, &output)?;
            Ok(json!({
                "ok": true,
                "schema": GETEVENT_SCHEMA,
                "input": path_string(&input)?,
                "output": path_string(&output)?,
                "stats": normalize_stats_json(&stats),
            }))
        }
    }
}

pub(crate) fn normalize_stats_json(stats: &NormalizeStats) -> Value {
    json!({
        "line_count": stats.lines,
        "record_count": stats.records,
        "device_count": stats.devices,
        "input_event_count": stats.input_events,
        "touch_frame_count": stats.touch_frames,
        "unparsed_line_count": stats.unparsed_lines,
    })
}

fn doctor(app: &App) -> CliResult<Value> {
    let adb_devices = app.adb_host(&[String::from("devices")], FailureMode::AllowFailure)?;
    let device = app.device_selection_json(adb_devices.stdout());
    let device_selected = device.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let ime_list = if device_selected {
        app.adb_shell(
            vec![
                String::from("ime"),
                String::from("list"),
                String::from("-s"),
            ],
            FailureMode::AllowFailure,
        )
        .ok()
    } else {
        None
    };
    let gh_version = run_process(
        "gh",
        &[String::from("--version")],
        FailureMode::AllowFailure,
    )?;
    let device_connected = device
        .get("connected_device_count")
        .and_then(Value::as_u64)
        .is_some_and(|count| count > 0);
    let ime_list_json = ime_list.as_ref().map_or_else(
        || {
            json!({
                "skipped": true,
                "reason": "no unambiguous adb device selected",
            })
        },
        crate::process::ProcessOutput::json,
    );
    let ime_registered = ime_list
        .as_ref()
        .is_some_and(|output| ime_is_registered(output.stdout(), &app.ime_component()));
    let gh_available = gh_version.status_code == Some(0_i32);
    let ime_list_ok = ime_list
        .as_ref()
        .is_some_and(|output| output.status_code == Some(0_i32));

    Ok(json!({
        "ok": adb_devices.status_code == Some(0_i32)
            && device_selected
            && ime_list_ok
            && ime_registered
            && gh_available,
        "package_name": app.package(),
        "ime_component": app.ime_component(),
        "requested_serial": app.serial(),
        "device_connected": device_connected,
        "device": device,
        "ime_registered": ime_registered,
        "gh_available": gh_available,
        "adb_devices": adb_devices.json(),
        "ime_list": ime_list_json,
        "gh_version": gh_version.json(),
    }))
}

fn install(app: &App, apk: Option<&Path>, repo: &str, dir: &Path) -> CliResult<Value> {
    let apk_path = if let Some(path) = apk {
        path.to_path_buf()
    } else {
        let download_dir = fresh_download_dir(dir)?;
        let release_tag = latest_release_tag(repo)?;
        let gh_args = vec![
            String::from("release"),
            String::from("download"),
            release_tag,
            String::from("--repo"),
            String::from(repo),
            String::from("--pattern"),
            String::from("*debug.apk"),
            String::from("--dir"),
            path_string(&download_dir)?,
            String::from("--clobber"),
        ];
        let _download = run_process("gh", &gh_args, FailureMode::RequireSuccess)?;
        latest_debug_apk(&download_dir)?
    };

    let install_output = app.adb(
        &[
            String::from("install"),
            String::from("-r"),
            path_string(&apk_path)?,
        ],
        FailureMode::RequireSuccess,
    )?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "apk": path_string(&apk_path)?,
        "install": install_output.json(),
    }))
}

fn select_ime(app: &App) -> CliResult<Value> {
    let component = app.ime_component();
    let enable_output = app.adb_shell(
        vec![
            String::from("ime"),
            String::from("enable"),
            component.clone(),
        ],
        FailureMode::RequireSuccess,
    )?;
    let set_output = app.adb_shell(
        vec![String::from("ime"), String::from("set"), component.clone()],
        FailureMode::RequireSuccess,
    )?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "ime_component": component,
        "enable": enable_output.json(),
        "set": set_output.json(),
    }))
}

fn start(app: &App, config: &LoggingStartConfig<'_>) -> CliResult<Value> {
    let mut extras = vec![
        String::from("--es"),
        String::from("run_id"),
        String::from(config.run_id),
        String::from("--es"),
        String::from("input_actor"),
        String::from(config.input_actor),
        String::from("--es"),
        String::from("input_cadence_policy"),
        String::from(config.input_cadence_policy),
    ];
    if let Some(controller) = config.input_controller {
        extras.extend([
            String::from("--es"),
            String::from("input_controller"),
            String::from(controller),
        ]);
    }
    if let Some(provenance) = config.input_profile {
        extras.extend(provenance.broadcast_extras());
    }
    let enable = app.broadcast("ENABLE", Vec::new())?;
    if enable.get("ok").and_then(Value::as_bool) == Some(false) {
        return Ok(json!({
            "ok": false,
            "package_name": app.package(),
            "error": "failed to enable logging before starting session",
            "enable": enable,
        }));
    }
    app.broadcast("START", extras)
}

fn session(app: &App, command: SessionCommand) -> CliResult<Value> {
    match command {
        SessionCommand::Start {
            run_id,
            input_actor,
            input_controller,
            input_cadence_policy,
            input_profile,
            input_profile_seed,
        } => session_start(
            app,
            &SessionStartConfig {
                run_id: &run_id,
                input_actor: &input_actor,
                input_controller: &input_controller,
                input_cadence_policy: &input_cadence_policy,
                input_profile_path: input_profile.as_deref(),
                input_profile_seed,
            },
        ),
        SessionCommand::Status => session_status(app),
        SessionCommand::Stop => session_stop(app),
    }
}

fn session_start(app: &App, config: &SessionStartConfig<'_>) -> CliResult<Value> {
    let mut session_lock = match controller::acquire_session_start(app, config.run_id)? {
        SessionStartPermit::Acquired(session_lock) => session_lock,
        SessionStartPermit::Busy(status) => return Ok(status),
    };
    let input_profile = profile::load_for_session(
        config.input_actor,
        config.input_controller,
        config.input_profile_path,
        config.input_profile_seed,
    )?;
    let profile_provenance = input_profile.as_ref().map(RuntimeProfile::provenance);
    let select = select_ime(app)?;
    let ime = start(
        app,
        &LoggingStartConfig {
            run_id: config.run_id,
            input_actor: config.input_actor,
            input_controller: Some(config.input_controller),
            input_cadence_policy: config.input_cadence_policy,
            input_profile: profile_provenance.as_ref(),
        },
    )?;
    if ime.get("ok").and_then(Value::as_bool) == Some(false) {
        return Ok(json!({
            "ok": false,
            "package_name": app.package(),
            "error": "failed to start IME logging session",
            "select_ime": select,
            "ime": ime,
        }));
    }

    match controller::start(app, config.run_id, input_profile.as_ref()) {
        Ok(mut input) => {
            let input_ok = input.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let stop_after_input_failure = if input_ok {
                session_lock.activate(&input)?;
                input = controller::status(app)?;
                Value::Null
            } else {
                app.broadcast("STOP", Vec::new()).unwrap_or_else(|error| {
                    json!({
                        "ok": false,
                        "error": error.to_string(),
                    })
                })
            };
            Ok(json!({
                "ok": input_ok,
                "package_name": app.package(),
                "run_id": config.run_id,
                "select_ime": select,
                "ime": ime,
                "input": input,
                "stop_after_input_failure": stop_after_input_failure,
            }))
        }
        Err(error) => {
            let stop_result = app.broadcast("STOP", Vec::new()).ok();
            Err(CliError::new(format!(
                "failed to start input controller after IME session start: {error}; IME stop attempted: {}",
                stop_result.as_ref().map_or("unavailable", |_| "available")
            )))
        }
    }
}

fn session_status(app: &App) -> CliResult<Value> {
    let ime = app.broadcast("STATUS", Vec::new())?;
    let input = controller::status(app)?;
    let adb_devices = app.adb_host(&[String::from("devices")], FailureMode::AllowFailure)?;
    let device = app.device_selection_json(adb_devices.stdout());
    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "device": device,
        "ime": ime,
        "input": input,
    }))
}

fn session_stop(app: &App) -> CliResult<Value> {
    let input = controller::stop(app)?;
    let ime = app.broadcast("STOP", Vec::new())?;
    controller::clear_session_lock(app)?;
    Ok(json!({
        "ok": ime.get("ok").and_then(Value::as_bool).unwrap_or(false)
            && input.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "package_name": app.package(),
        "input": input,
        "ime": ime,
    }))
}

fn run_controller_command(app: &App, command: ControllerCommand) -> CliResult<Value> {
    match command {
        ControllerCommand::Run {
            socket,
            state,
            uinput_stdout,
            uinput_stderr,
            run_id,
            input_profile_runtime_json,
        } => {
            let input_profile = input_profile_runtime_json
                .as_deref()
                .map(profile::parse_runtime_json)
                .transpose()?;
            let config = RunConfig {
                socket,
                state,
                uinput_stdout,
                uinput_stderr,
                run_id,
                input_profile,
            };
            controller::run(app, &config)
        }
    }
}

fn layout(app: &App, wait: LayoutWait) -> CliResult<Value> {
    match wait {
        LayoutWait::Current => app.broadcast("KEYBOARD_LAYOUT", Vec::new()),
        LayoutWait::Visible | LayoutWait::Hidden => wait_for_layout(app, wait),
    }
}

struct EdgeBackGestureConfig {
    method: HideKeyboardMethod,
    side: EdgeSide,
    start_y_ratio: Option<RatioPpm>,
    distance_ratio: Option<RatioPpm>,
    end_y_drift_ratio: Option<SignedRatioPpm>,
    edge_margin_ratio: Option<RatioPpm>,
    duration_ms: Option<u64>,
    steps: Option<u16>,
}

fn hide_keyboard_command(app: &App, command: &Commands) -> CliResult<Value> {
    let &Commands::HideKeyboard {
        method,
        side,
        start_y_ratio,
        distance_ratio,
        end_y_drift_ratio,
        edge_margin_ratio,
        duration_ms,
        steps,
    } = command
    else {
        return Err(CliError::new("expected hide-keyboard command"));
    };
    hide_keyboard(
        app,
        &EdgeBackGestureConfig {
            method,
            side,
            start_y_ratio,
            distance_ratio,
            end_y_drift_ratio,
            edge_margin_ratio,
            duration_ms,
            steps,
        },
    )
}

fn hide_keyboard(app: &App, config: &EdgeBackGestureConfig) -> CliResult<Value> {
    let before = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
    if !layout_available(&before)? {
        return Ok(json!({
            "ok": true,
            "package_name": app.package(),
            "already_hidden": true,
            "method": hide_keyboard_method_name(config.method),
            "side": edge_side_name(config.side),
            "layout": before,
        }));
    }

    let gesture = edge_back_gesture(app, config)?;
    let hide_output = controller::path(app, gesture.path.clone())?;
    let after = wait_for_layout(app, LayoutWait::Hidden)?;
    let hidden = layout_is_hidden_result(&after);

    Ok(json!({
        "ok": hide_output.get("ok").and_then(Value::as_bool).unwrap_or(false) && hidden,
        "package_name": app.package(),
        "already_hidden": false,
        "method": hide_keyboard_method_name(config.method),
        "side": edge_side_name(config.side),
        "gesture": gesture.to_json(),
        "hide": hide_output,
        "layout": after,
    }))
}

fn edge_back_gesture(app: &App, config: &EdgeBackGestureConfig) -> CliResult<EdgeBackGesture> {
    match config.method {
        HideKeyboardMethod::EdgeBack => edge_back_path(app, config),
    }
}

fn edge_back_path(app: &App, config: &EdgeBackGestureConfig) -> CliResult<EdgeBackGesture> {
    let profile = uinput::discover_touchscreen_profile(app)?;
    let defaults = default_edge_back_profile(config.side);
    let edge_margin = config
        .edge_margin_ratio
        .unwrap_or(defaults.edge_margin_ratio);
    let start_y = config.start_y_ratio.unwrap_or(defaults.start_y_ratio);
    let distance = config.distance_ratio.unwrap_or(defaults.distance_ratio);
    let drift = config
        .end_y_drift_ratio
        .unwrap_or(defaults.end_y_drift_ratio);
    let duration_ms = config.duration_ms.unwrap_or(defaults.duration_ms);
    let steps = config.steps.unwrap_or(defaults.steps);
    let start_x = edge_start_x_ratio(config.side, edge_margin)?;
    let end_x = edge_end_x_ratio(config.side, start_x, distance)?;
    let end_y = start_y.checked_add_signed(drift)?;
    let start = TouchPoint::new(
        uinput::x_coordinate_from_ratio(&profile, start_x)?,
        uinput::y_coordinate_from_ratio(&profile, start_y)?,
    );
    let end = TouchPoint::new(
        uinput::x_coordinate_from_ratio(&profile, end_x)?,
        uinput::y_coordinate_from_ratio(&profile, end_y)?,
    );
    let path = uinput::swipe_path_spec(start, end, duration_ms, steps)?;
    Ok(EdgeBackGesture {
        side: config.side,
        start_x_ratio: start_x,
        start_y_ratio: start_y,
        end_x_ratio: end_x,
        end_y_ratio: end_y,
        distance_ratio: distance,
        end_y_drift_ratio: drift,
        edge_margin_ratio: edge_margin,
        duration_ms,
        steps,
        path,
    })
}

#[derive(Clone)]
struct EdgeBackGesture {
    side: EdgeSide,
    start_x_ratio: RatioPpm,
    start_y_ratio: RatioPpm,
    end_x_ratio: RatioPpm,
    end_y_ratio: RatioPpm,
    distance_ratio: RatioPpm,
    end_y_drift_ratio: SignedRatioPpm,
    edge_margin_ratio: RatioPpm,
    duration_ms: u64,
    steps: u16,
    path: PathSpec,
}

impl EdgeBackGesture {
    fn to_json(&self) -> Value {
        json!({
            "side": edge_side_name(self.side),
            "start_x_ratio": ratio_json(self.start_x_ratio),
            "start_y_ratio": ratio_json(self.start_y_ratio),
            "end_x_ratio": ratio_json(self.end_x_ratio),
            "end_y_ratio": ratio_json(self.end_y_ratio),
            "distance_ratio": ratio_json(self.distance_ratio),
            "end_y_drift_ratio": signed_ratio_json(self.end_y_drift_ratio),
            "edge_margin_ratio": ratio_json(self.edge_margin_ratio),
            "duration_ms": self.duration_ms,
            "steps": self.steps,
            "path": self.path,
        })
    }
}

#[derive(Clone, Copy)]
struct EdgeBackDefaults {
    start_y_ratio: RatioPpm,
    distance_ratio: RatioPpm,
    end_y_drift_ratio: SignedRatioPpm,
    edge_margin_ratio: RatioPpm,
    duration_ms: u64,
    steps: u16,
}

const fn default_edge_back_profile(side: EdgeSide) -> EdgeBackDefaults {
    match side {
        EdgeSide::Left => EdgeBackDefaults {
            start_y_ratio: RatioPpm::from_ppm(539_300),
            distance_ratio: RatioPpm::from_ppm(281_000),
            end_y_drift_ratio: SignedRatioPpm::from_ppm(21_500),
            edge_margin_ratio: RatioPpm::from_ppm(2_000),
            duration_ms: 110,
            steps: 18,
        },
        EdgeSide::Right => EdgeBackDefaults {
            start_y_ratio: RatioPpm::from_ppm(498_900),
            distance_ratio: RatioPpm::from_ppm(398_000),
            end_y_drift_ratio: SignedRatioPpm::from_ppm(52_900),
            edge_margin_ratio: RatioPpm::from_ppm(2_000),
            duration_ms: 75,
            steps: 12,
        },
    }
}

fn edge_start_x_ratio(side: EdgeSide, edge_margin: RatioPpm) -> CliResult<RatioPpm> {
    match side {
        EdgeSide::Left => Ok(edge_margin),
        EdgeSide::Right => RatioPpm::from_ppm(1_000_000).checked_subtract(edge_margin),
    }
}

fn edge_end_x_ratio(side: EdgeSide, start_x: RatioPpm, distance: RatioPpm) -> CliResult<RatioPpm> {
    match side {
        EdgeSide::Left => start_x.checked_add(distance),
        EdgeSide::Right => start_x.checked_subtract(distance),
    }
}

const fn hide_keyboard_method_name(method: HideKeyboardMethod) -> &'static str {
    match method {
        HideKeyboardMethod::EdgeBack => "edge_back",
    }
}

const fn edge_side_name(side: EdgeSide) -> &'static str {
    match side {
        EdgeSide::Left => "left",
        EdgeSide::Right => "right",
    }
}

fn ratio_json(ratio: RatioPpm) -> Value {
    json!({
        "ppm": ratio.ppm(),
    })
}

fn signed_ratio_json(ratio: SignedRatioPpm) -> Value {
    json!({
        "ppm": ratio.ppm(),
    })
}

fn layout_is_hidden_result(status: &Value) -> bool {
    layout_matches(status, LayoutWait::Hidden).unwrap_or(false)
}

fn wait_for_layout(app: &App, wait: LayoutWait) -> CliResult<Value> {
    let start = Instant::now();
    loop {
        let status = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
        if layout_matches(&status, wait)? {
            return Ok(with_layout_wait_metadata(status, wait, start.elapsed()));
        }
        if start.elapsed() >= LAYOUT_WAIT_TIMEOUT {
            return Ok(json!({
                "ok": false,
                "package_name": app.package(),
                "error": format!("timed out waiting for keyboard layout to be {}", wait.description()),
                "wait": wait.description(),
                "timeout_ms": millis_u64(LAYOUT_WAIT_TIMEOUT),
                "layout": status,
            }));
        }
        std::thread::sleep(LAYOUT_POLL_INTERVAL);
    }
}

fn with_layout_wait_metadata(mut status: Value, wait: LayoutWait, elapsed: Duration) -> Value {
    if let Some(object) = status.as_object_mut() {
        object.insert(String::from("wait"), json!(wait.description()));
        object.insert(String::from("wait_elapsed_ms"), json!(millis_u64(elapsed)));
    }
    status
}

fn layout_matches(status: &Value, wait: LayoutWait) -> CliResult<bool> {
    let available = layout_available(status)?;
    Ok(match wait {
        LayoutWait::Current => true,
        LayoutWait::Visible => available,
        LayoutWait::Hidden => !available,
    })
}

fn layout_available(status: &Value) -> CliResult<bool> {
    let layout = status
        .get("keyboard_layout")
        .ok_or_else(|| CliError::new("keyboard_layout was not present"))?;
    Ok(layout
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn press_key(app: &App, key: PressKey) -> CliResult<Value> {
    let code = press_key_code(key);
    let mut result = tap_key(app, None, Some(code))?;
    if let Some(object) = result.as_object_mut() {
        object.insert(String::from("pressed_key"), json!(press_key_name(key)));
    }
    Ok(result)
}

fn type_text(app: &App, text: &str, inter_key_delay_ms: u64) -> CliResult<Value> {
    let input_status = controller::status(app)?;
    if !input_status
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CliError::new(
            "no active input session; run `input-dynamics session start --run-id <id>`",
        ));
    }

    let layout_result = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
    let plan = planned_type_steps(app, &layout_result, text)?;
    let total_steps = plan.steps.len();
    let mut typed = Vec::with_capacity(total_steps);
    for (step_index, step) in plan.steps.iter().enumerate() {
        let touch_output = controller::tap(
            app,
            controller::ControllerTapSpec::profiled_key(
                TapSpec::new(step.x, step.y),
                step.key_context(),
                InterKeyDelaySampling::Sample,
            ),
        )?;
        let touch_ok = touch_output
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        typed.push(step.to_json(&touch_output));
        if !touch_ok {
            return Ok(json!({
                "ok": false,
                "package_name": app.package(),
                "input_backend": "uinput",
                "error": "touch controller rejected type step",
                "failed_step_index": step_index,
                "typed_count": typed.len(),
                "text_char_count": total_steps,
                "inter_key_delay_ms": inter_key_delay_ms,
                "layout_refresh_count": plan.layout_refresh_count,
                "typed": typed,
            }));
        }
        if step_index < total_steps.saturating_sub(1) {
            let delay_ms = touch_output
                .get("controller")
                .and_then(|controller| controller.get("inter_key_delay_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(inter_key_delay_ms);
            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
        }
    }

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "input_backend": "uinput",
        "text_char_count": total_steps,
        "typed_count": typed.len(),
        "inter_key_delay_ms": inter_key_delay_ms,
        "input_cadence_policy": "input_profile_or_fixed_inter_key_delay",
        "layout_refresh_count": plan.layout_refresh_count,
        "typed": typed,
    }))
}

fn touch(app: &App, command: &TouchCommand) -> CliResult<Value> {
    match *command {
        TouchCommand::Doctor => uinput::doctor(app),
        TouchCommand::Tap { x, y, hold_ms } => {
            let mut spec = TapSpec::new(x, y);
            spec.hold_ms = hold_ms;
            uinput::tap(app, spec)
        }
        TouchCommand::Swipe {
            from_x,
            from_y,
            to_x,
            to_y,
            duration_ms,
            steps,
        } => {
            let spec = uinput::swipe_path_spec(
                TouchPoint::new(from_x, from_y),
                TouchPoint::new(to_x, to_y),
                duration_ms,
                steps,
            )?;
            let result = controller::path(app, spec.clone())?;
            Ok(json!({
                "ok": result.get("ok").and_then(Value::as_bool).unwrap_or(false),
                "input_backend": "uinput",
                "touch": "swipe",
                "path": spec,
                "controller": result,
            }))
        }
        TouchCommand::Path {
            ref points_json,
            ref points_file,
            duration_ms,
        } => {
            let points = read_touch_path(points_json.as_deref(), points_file.as_deref())?;
            let spec = PathSpec::new(points, duration_ms);
            let result = controller::path(app, spec.clone())?;
            Ok(json!({
                "ok": result.get("ok").and_then(Value::as_bool).unwrap_or(false),
                "input_backend": "uinput",
                "touch": "path",
                "path": spec,
                "controller": result,
            }))
        }
    }
}

fn read_touch_path(
    points_json: Option<&str>,
    points_file: Option<&Path>,
) -> CliResult<Vec<TouchPoint>> {
    let text = match (points_json, points_file) {
        (Some(json_text), None) => String::from(json_text),
        (None, Some(path)) => fs::read_to_string(path)?,
        (None, None) => {
            return Err(CliError::new(
                "touch path requires --points-json or --points-file",
            ));
        }
        (Some(_), Some(_)) => {
            return Err(CliError::new(
                "touch path accepts either --points-json or --points-file, not both",
            ));
        }
    };
    parse_touch_points(&text)
}

fn parse_touch_points(text: &str) -> CliResult<Vec<TouchPoint>> {
    let parsed: Value = serde_json::from_str(text)?;
    let array = parsed
        .as_array()
        .ok_or_else(|| CliError::new("touch path JSON must be an array"))?;
    if array.len() < 2 {
        return Err(CliError::new("touch path requires at least two points"));
    }
    array
        .iter()
        .enumerate()
        .map(|(index, value)| parse_touch_point(value, index))
        .collect()
}

fn parse_touch_point(value: &Value, index: usize) -> CliResult<TouchPoint> {
    if let Some(object) = value.as_object() {
        let x = object
            .get("x")
            .ok_or_else(|| CliError::new(format!("touch point {index} is missing x")))
            .and_then(|coordinate| parse_i32_json_coordinate(coordinate, index, "x"))?;
        let y = object
            .get("y")
            .ok_or_else(|| CliError::new(format!("touch point {index} is missing y")))
            .and_then(|coordinate| parse_i32_json_coordinate(coordinate, index, "y"))?;
        return Ok(TouchPoint::new(x, y));
    }

    let Some(array) = value.as_array() else {
        return Err(CliError::new(format!(
            "touch point {index} must be an object or two-item array"
        )));
    };
    if array.len() != 2 {
        return Err(CliError::new(format!(
            "touch point {index} array must contain exactly two coordinates"
        )));
    }
    let mut coordinates = array.iter();
    let x_value = coordinates
        .next()
        .ok_or_else(|| CliError::new(format!("touch point {index} is missing x")))?;
    let y_value = coordinates
        .next()
        .ok_or_else(|| CliError::new(format!("touch point {index} is missing y")))?;
    Ok(TouchPoint::new(
        parse_i32_json_coordinate(x_value, index, "x")?,
        parse_i32_json_coordinate(y_value, index, "y")?,
    ))
}

fn parse_i32_json_coordinate(value: &Value, index: usize, label: &str) -> CliResult<i32> {
    let coordinate = value.as_i64().ok_or_else(|| {
        CliError::new(format!(
            "touch point {index} coordinate {label} must be an integer"
        ))
    })?;
    i32::try_from(coordinate).map_err(|error| {
        CliError::new(format!(
            "touch point {index} coordinate {label} is outside i32 range: {error}"
        ))
    })
}

const fn press_key_code(key: PressKey) -> i64 {
    match key {
        PressKey::Delete => -7,
        PressKey::Enter => 10,
        PressKey::Space => 32,
    }
}

const fn press_key_name(key: PressKey) -> &'static str {
    match key {
        PressKey::Delete => "delete",
        PressKey::Enter => "enter",
        PressKey::Space => "space",
    }
}

#[derive(Clone, Copy)]
enum LayoutWait {
    Current,
    Visible,
    Hidden,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TypeKeyTarget {
    Label(String),
    Code(i64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedTypeStep {
    char_index: usize,
    target: TypeKeyTarget,
    x: i32,
    y: i32,
    key_width_px: i32,
    key_height_px: i32,
    key_code: Option<i64>,
    key_class: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedTypeSteps {
    steps: Vec<PlannedTypeStep>,
    layout_refresh_count: u64,
}

impl LayoutWait {
    const fn description(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Visible => "visible",
            Self::Hidden => "hidden",
        }
    }
}

fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn pull_logs(app: &App, out: &Path) -> CliResult<Value> {
    fs::create_dir_all(out)?;
    let pull_output = app.adb(
        &[
            String::from("pull"),
            app.remote_log_dir(),
            path_string(out)?,
        ],
        FailureMode::AllowFailure,
    )?;

    if pull_output.status_code == Some(0_i32) {
        return Ok(json!({
            "ok": true,
            "package_name": app.package(),
            "remote_log_dir": app.remote_log_dir(),
            "output_dir": path_string(out)?,
            "pull_method": "external_adb_pull",
            "pull": pull_output.json(),
        }));
    }

    let internal_pull = pull_internal_logs(app, out)?;

    Ok(json!({
        "ok": true,
        "package_name": app.package(),
        "remote_log_dir": app.remote_log_dir(),
        "internal_log_dir": App::internal_log_dir(),
        "output_dir": path_string(out)?,
        "pull_method": "run_as_internal_fallback",
        "external_pull": pull_output.json(),
        "internal_pull": internal_pull,
    }))
}

fn tap_key(app: &App, label: Option<&str>, code: Option<i64>) -> CliResult<Value> {
    if label.is_none() && code.is_none() {
        return Err(CliError::new("tap requires --label or --code"));
    }

    let layout_result = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
    let layout = keyboard_layout_value(&layout_result)?;
    ensure_keyboard_layout_available(layout)?;
    let key = resolve_layout_key(layout, label, code)?;
    let x = key_tap_coordinate(key, "x")?;
    let y = key_tap_coordinate(key, "y")?;
    let key_context = key_profile_context(key)?;

    let touch_output = controller::tap(
        app,
        controller::ControllerTapSpec::profiled_key(
            TapSpec::new(x, y),
            key_context,
            InterKeyDelaySampling::Skip,
        ),
    )?;
    let touch_ok = touch_output
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Ok(json!({
        "ok": touch_ok,
        "package_name": app.package(),
        "input_backend": "uinput",
        "key": key,
        "touch": touch_output,
    }))
}

fn planned_type_steps(
    app: &App,
    initial_layout_result: &Value,
    text: &str,
) -> CliResult<PlannedTypeSteps> {
    let mut layout_result = initial_layout_result.clone();
    let mut layout_refresh_count = 0_u64;
    let mut steps = Vec::with_capacity(text.chars().count());
    for (char_index, character) in text.chars().enumerate() {
        let step_result = planned_type_step(&layout_result, char_index, character);
        let step = match step_result {
            Ok(step) => step,
            Err(first_error) => {
                layout_result = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
                layout_refresh_count = layout_refresh_count.saturating_add(1);
                planned_type_step(&layout_result, char_index, character).map_err(
                    |second_error| {
                        CliError::new(format!(
                            "{first_error}; after layout refresh: {second_error}"
                        ))
                    },
                )?
            }
        };
        steps.push(step);
    }
    Ok(PlannedTypeSteps {
        steps,
        layout_refresh_count,
    })
}

fn planned_type_step(
    layout_result: &Value,
    char_index: usize,
    character: char,
) -> CliResult<PlannedTypeStep> {
    let target = type_key_target(character, char_index)?;
    let layout = keyboard_layout_value(layout_result)?;
    ensure_keyboard_layout_available(layout)?;
    let key = match target {
        TypeKeyTarget::Label(ref label) => resolve_layout_key(layout, Some(label.as_str()), None),
        TypeKeyTarget::Code(code) => resolve_layout_key(layout, None, Some(code)),
    }
    .map_err(|error| {
        CliError::new(format!(
            "unsupported character at index {char_index}: {}: {error}",
            character_description(character)
        ))
    })?;
    Ok(PlannedTypeStep {
        char_index,
        target,
        x: key_tap_coordinate(key, "x")?,
        y: key_tap_coordinate(key, "y")?,
        key_width_px: key_i32(key, "key_width_px")?,
        key_height_px: key_i32(key, "key_height_px")?,
        key_code: key.get("key_code").and_then(Value::as_i64),
        key_class: key
            .get("key_class")
            .and_then(Value::as_str)
            .map(String::from),
    })
}

fn type_key_target(character: char, char_index: usize) -> CliResult<TypeKeyTarget> {
    if character == ' ' {
        return Ok(TypeKeyTarget::Code(press_key_code(PressKey::Space)));
    }
    if character.is_control() || character.is_whitespace() {
        return Err(CliError::new(format!(
            "unsupported character at index {char_index}: {}; type supports visible layout keys and ASCII space only",
            character_description(character)
        )));
    }
    Ok(TypeKeyTarget::Label(character.to_string()))
}

fn keyboard_layout_value(layout_result: &Value) -> CliResult<&Value> {
    layout_result
        .get("keyboard_layout")
        .ok_or_else(|| CliError::new("keyboard_layout was not present"))
}

fn ensure_keyboard_layout_available(layout: &Value) -> CliResult<()> {
    let available = layout
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if available {
        Ok(())
    } else {
        Err(CliError::new("keyboard layout is not available"))
    }
}

fn resolve_layout_key<'a>(
    layout: &'a Value,
    label: Option<&str>,
    code: Option<i64>,
) -> CliResult<&'a Value> {
    let keys = layout
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| CliError::new("keyboard_layout.keys was not an array"))?;
    keys.iter()
        .find(|candidate| key_matches(candidate, label, code))
        .ok_or_else(|| CliError::new("requested key was not found in keyboard layout"))
}

fn key_tap_coordinate(key: &Value, axis: &str) -> CliResult<i32> {
    let field = format!("tap_center_screen_{axis}_px");
    let coordinate = key
        .get(&field)
        .and_then(json_number_to_shell_arg)
        .ok_or_else(|| CliError::new(format!("key is missing {field}")))?;
    parse_tap_coordinate(&coordinate, axis)
}

fn key_profile_context(key: &Value) -> CliResult<KeyProfileContext> {
    Ok(KeyProfileContext {
        key_width_px: key_i32(key, "key_width_px")?,
        key_height_px: key_i32(key, "key_height_px")?,
    })
}

fn key_i32(key: &Value, field: &str) -> CliResult<i32> {
    let value = key
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| CliError::new(format!("key is missing {field}")))?;
    i32::try_from(value)
        .map_err(|error| CliError::new(format!("key {field} is outside i32 range: {error}")))
}

fn character_description(character: char) -> String {
    let escaped = character.escape_default().collect::<String>();
    format!("'{escaped}' (U+{:04X})", u32::from(character))
}

impl PlannedTypeStep {
    const fn key_context(&self) -> KeyProfileContext {
        KeyProfileContext {
            key_width_px: self.key_width_px,
            key_height_px: self.key_height_px,
        }
    }

    fn to_json(&self, touch_output: &Value) -> Value {
        json!({
            "char_index": self.char_index,
            "target": self.target.to_json(),
            "key_class": self.key_class,
            "tap": {
                "x": self.x,
                "y": self.y,
            },
            "touch_ok": touch_output
                .get("ok")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

impl TypeKeyTarget {
    fn to_json(&self) -> Value {
        match *self {
            Self::Label(_) => json!({
                "kind": "label",
            }),
            Self::Code(_) => json!({
                "kind": "code",
            }),
        }
    }
}

fn parse_tap_coordinate(value: &str, axis: &str) -> CliResult<i32> {
    value.parse::<i32>().map_err(|error| {
        CliError::new(format!(
            "invalid {axis} tap coordinate from keyboard layout: {value}: {error}"
        ))
    })
}

fn pull_internal_logs(app: &App, out: &Path) -> CliResult<Value> {
    let local_log_dir = out.join(LOG_DIR);
    fs::create_dir_all(&local_log_dir)?;
    let list_output = app.adb_shell(
        vec![
            String::from("run-as"),
            String::from(app.package()),
            String::from("ls"),
            App::internal_log_dir(),
        ],
        FailureMode::RequireSuccess,
    )?;
    let mut pulled_files = Vec::new();
    for line in list_output.stdout().lines() {
        let file_name = line.trim();
        if file_name.is_empty() {
            continue;
        }
        if file_name.contains('/') || file_name == "." || file_name == ".." {
            return Err(CliError::new(format!(
                "unexpected internal log file name: {file_name}"
            )));
        }
        let remote_file = format!("{}/{file_name}", App::internal_log_dir());
        let file_output = app.adb_shell(
            vec![
                String::from("run-as"),
                String::from(app.package()),
                String::from("cat"),
                remote_file,
            ],
            FailureMode::RequireSuccess,
        )?;
        fs::write(local_log_dir.join(file_name), file_output.stdout())?;
        pulled_files.push(String::from(file_name));
    }

    Ok(json!({
        "status_code": 0,
        "file_count": pulled_files.len(),
        "files": pulled_files,
        "list": list_output.json(),
    }))
}

fn fresh_download_dir(base_dir: &Path) -> CliResult<PathBuf> {
    let download_dir = base_dir.join(format!("download-{}", std::process::id()));
    if download_dir.exists() {
        fs::remove_dir_all(&download_dir)?;
    }
    fs::create_dir_all(&download_dir)?;
    Ok(download_dir)
}

fn latest_debug_apk(dir: &Path) -> CliResult<PathBuf> {
    let mut candidates = Vec::new();
    for entry_result in fs::read_dir(dir)? {
        let entry = entry_result?;
        let path = entry.path();
        if !is_debug_apk(&path) {
            continue;
        }
        let metadata = entry.metadata()?;
        let modified = match metadata.modified() {
            Ok(time) => time,
            Err(_) => UNIX_EPOCH,
        };
        candidates.push((modified, path));
    }
    candidates.sort_by_key(|candidate| Reverse(candidate.0));
    candidates
        .first()
        .map(|candidate| candidate.1.clone())
        .ok_or_else(|| CliError::new(format!("no debug APK was found in {}", dir.display())))
}

fn latest_release_tag(repo: &str) -> CliResult<String> {
    let gh_args = vec![
        String::from("release"),
        String::from("list"),
        String::from("--repo"),
        String::from(repo),
        String::from("--json"),
        String::from("tagName,isDraft,isPrerelease,createdAt"),
        String::from("--limit"),
        String::from("50"),
    ];
    let output = run_process("gh", &gh_args, FailureMode::RequireSuccess)?;
    latest_release_tag_from_json(output.stdout())
}

fn latest_release_tag_from_json(json_text: &str) -> CliResult<String> {
    let value: Value = serde_json::from_str(json_text)?;
    let releases = value
        .as_array()
        .ok_or_else(|| CliError::new("gh release list did not return an array"))?;
    for release in releases {
        let is_draft = release
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_draft {
            continue;
        }
        let Some(tag_name) = release.get("tagName").and_then(Value::as_str) else {
            continue;
        };
        if !tag_name.is_empty() {
            return Ok(String::from(tag_name));
        }
    }
    Err(CliError::new("no non-draft GitHub release was found"))
}

fn is_debug_apk(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|file_name| file_name.ends_with("-debug.apk"))
}

pub(crate) fn path_string(path: &Path) -> CliResult<String> {
    path.to_str()
        .map(String::from)
        .ok_or_else(|| CliError::new(format!("path is not valid UTF-8: {}", path.display())))
}

fn ime_is_registered(ime_list_stdout: &str, ime_component: &str) -> bool {
    ime_list_stdout
        .lines()
        .any(|line| line.trim() == ime_component)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use proptest::strategy::Strategy;
    use serde_json::json;

    use crate::args::PressKey;
    use crate::commands::{
        TypeKeyTarget, ime_is_registered, is_debug_apk, latest_release_tag_from_json,
        planned_type_step, press_key_code, type_key_target,
    };

    #[test]
    fn ime_registration_requires_exact_component_line() {
        let ime = "org.inputdynamics.ime.debug/helium314.keyboard.latin.LatinIME";
        let ime_list = format!("other/.Ime\n{ime}\n");

        assert!(
            ime_is_registered(&ime_list, ime),
            "exact IME component should be detected"
        );
        assert!(
            !ime_is_registered("org.inputdynamics.ime.debug/.OtherIme\n", ime),
            "different IME component should not be accepted"
        );
    }

    #[test]
    fn debug_apk_filter_rejects_unrelated_apk_names() {
        assert!(
            is_debug_apk(Path::new("InputDynamicsKeyboard-v0.1.0-debug.apk")),
            "debug release asset should be accepted"
        );
        assert!(
            !is_debug_apk(Path::new("other.apk")),
            "unrelated APK should be rejected"
        );
        assert!(
            !is_debug_apk(Path::new("notdebug.apk")),
            "APK names must use the release debug suffix"
        );
    }

    #[test]
    fn release_tag_parser_accepts_prereleases_and_skips_drafts() {
        let releases = r#"
            [
                {"tagName":"v0.2.0-draft","isDraft":true,"isPrerelease":false,"createdAt":"2026-06-21T01:00:00Z"},
                {"tagName":"v0.1.0","isDraft":false,"isPrerelease":true,"createdAt":"2026-06-21T00:00:00Z"}
            ]
        "#;
        let tag_result = latest_release_tag_from_json(releases);

        assert!(
            tag_result.is_ok(),
            "latest non-draft release tag should parse"
        );
        assert_eq!(
            tag_result.as_deref().ok(),
            Some("v0.1.0"),
            "prereleases should be valid install sources"
        );
    }

    #[test]
    fn type_key_target_maps_space_to_space_code() {
        assert_eq!(
            type_key_target(' ', 0).ok(),
            Some(TypeKeyTarget::Code(press_key_code(PressKey::Space))),
            "space should use the semantic space key code"
        );
    }

    #[test]
    fn planned_type_step_resolves_visible_label_and_space() {
        let layout = sample_layout_result();

        let letter_result = planned_type_step(&layout, 0, 'a');
        assert!(
            letter_result.is_ok(),
            "visible label should resolve to a tap"
        );
        let Ok(letter) = letter_result else {
            return;
        };
        assert_eq!(letter.char_index, 0);
        assert_eq!(letter.target, TypeKeyTarget::Label(String::from("a")));
        assert_eq!((letter.x, letter.y), (10_i32, 20_i32));
        assert_eq!(letter.key_code, Some(97));
        assert_eq!(letter.key_class.as_deref(), Some("letter"));

        let space_result = planned_type_step(&layout, 1, ' ');
        assert!(
            space_result.is_ok(),
            "space should resolve to the semantic space key"
        );
        let Ok(space) = space_result else {
            return;
        };
        assert_eq!(space.char_index, 1);
        assert_eq!(space.target, TypeKeyTarget::Code(32));
        assert_eq!((space.x, space.y), (50_i32, 60_i32));
        assert_eq!(space.key_class.as_deref(), Some("space"));
    }

    proptest::proptest! {
        #[test]
        fn type_key_target_rejects_control_characters(character in control_character()) {
            proptest::prop_assert!(
                type_key_target(character, 0).is_err(),
                "control characters should fail before any key is pressed"
            );
        }
    }

    fn sample_layout_result() -> serde_json::Value {
        json!({
            "keyboard_layout": {
                "available": true,
                "keys": [
                    {
                        "key_label": "a",
                        "key_code": 97,
                        "key_class": "letter",
                        "key_width_px": 100,
                        "key_height_px": 80,
                        "tap_center_screen_x_px": 10.0,
                        "tap_center_screen_y_px": 20
                    },
                    {
                        "key_label": null,
                        "key_code": 32,
                        "key_class": "space",
                        "key_width_px": 300,
                        "key_height_px": 80,
                        "tap_center_screen_x_px": 50,
                        "tap_center_screen_y_px": 60
                    }
                ]
            }
        })
    }

    fn control_character() -> impl Strategy<Value = char> {
        (0_u32..=0x1f).prop_map(|code| char::from_u32(code).unwrap_or('\0'))
    }
}
