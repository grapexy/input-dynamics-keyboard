//! Command implementations.

use std::cmp::Reverse;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use input_dynamics_analysis::derivation::{
    DeriveDismissalsConfig, DerivePressesConfig, DeriveRunSummaryConfig, DeriveTimelineConfig,
    DeriveVideoMapConfig, FfprobeInvocation, derive_dismissals as run_derive_dismissals,
    derive_press_summaries as run_derive_press_summaries, derive_run_summary,
    derive_timeline as run_derive_timeline, derive_video_map as run_derive_video_map,
};
use input_dynamics_analysis::getevent::{GETEVENT_SCHEMA, NormalizeStats, normalize_file};
use serde_json::{Value, json};

use crate::app::{App, LOG_DIR};
use crate::args::{
    Commands, ControllerCommand, DeriveCommand, EdgeSide, GeteventCommand, HideKeyboardMethod,
    KeyboardCommand, ObserveCommand, PressKey, RecordingCommand, SessionCommand, TouchCommand,
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
use crate::record::{RecordConfig, VideoMode, record_run};
use crate::recording::inspect_recording;
use crate::session_lifecycle::{
    HumanSessionStart, SessionStatusRequest, SessionStopRequest, session_status,
    start_human_session, stop_session,
};
use crate::session_state::schema::{COMMAND_RESULT_SCHEMA, INPUT_CONTROLLER_CLI};
use crate::uinput::{self, PathSpec, TapSpec, TouchPoint};
use crate::validate::validate_logs;

const LAYOUT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const LAYOUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const COMMAND_MIGRATION_SCHEMA: &str = "input_dynamics_command_migration.v1";
const DIAGNOSTIC_CONTROLLER_START_HINT: &str = "input-dynamics controller start --run-id <id>";

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
        } => Ok(session(app, session_command)),
        Commands::Keyboard {
            command: keyboard_command,
        } => keyboard(app, &keyboard_command),
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
        no_video,
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
        video_mode: if no_video {
            VideoMode::Disabled
        } else {
            VideoMode::Enabled
        },
        input_actor: input_actor.clone(),
        input_controller: input_controller.clone(),
        input_cadence_policy: input_cadence_policy.clone(),
    };
    record_run(app, &config)
}

fn derive(command: DeriveCommand) -> CliResult<Value> {
    match command {
        DeriveCommand::Presses {
            recording_dir,
            ime_jsonl,
            output,
        } => run_derive_press_summaries(&DerivePressesConfig {
            recording_dir,
            ime_jsonl,
            output,
        })
        .map_err(CliError::from),
        DeriveCommand::Summary {
            recording_dir,
            press_summaries_jsonl,
            output,
        } => derive_run_summary(&DeriveRunSummaryConfig {
            recording_dir,
            press_summaries_jsonl,
            output,
        })
        .map_err(CliError::from),
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
        DeriveCommand::VideoMap {
            recording_dir,
            output_dir,
            ffprobe,
        } => derive_video_map_command(&recording_dir, output_dir, &ffprobe),
    }
}

fn derive_video_map_command(
    recording_dir: &Path,
    output_dir: Option<PathBuf>,
    ffprobe: &str,
) -> CliResult<Value> {
    let video_path = recording_dir.join("video").join("screen.mp4");
    preflight_video_map_sources(recording_dir, &video_path)?;
    let version_args = ffprobe_version_args();
    let version_output = run_process(ffprobe, &version_args, FailureMode::RequireSuccess)
        .map_err(|error| ffprobe_error(ffprobe, &version_args, &error))?;
    let probe_args = ffprobe_frame_args(&video_path)?;
    let probe_output = run_process(ffprobe, &probe_args, FailureMode::AllowFailure)
        .map_err(|error| ffprobe_error(ffprobe, &probe_args, &error))?;
    if probe_output.status_code != Some(0_i32) {
        return Err(CliError::with_details(
            format!(
                "ffprobe failed for {}\nstatus: {:?}\nstderr: {}",
                video_path.display(),
                probe_output.status_code,
                probe_output.stderr().trim()
            ),
            json!({
                "error_kind": "ffprobe_probe_failed",
                "program": ffprobe,
                "args": probe_args,
                "status_code": probe_output.status_code,
                "stderr": probe_output.stderr().trim(),
                "video_path": path_string(&video_path)?,
                "suggested_action": "recording_inspect",
                "suggested_kind": "recording_inspect",
                "suggested_command": format!(
                    "input-dynamics recording inspect --dir {}",
                    shellish_path(recording_dir)?
                ),
            }),
        ));
    }
    run_derive_video_map(&DeriveVideoMapConfig {
        recording_dir: recording_dir.to_path_buf(),
        output_dir,
        ffprobe_json: probe_output.stdout().to_owned(),
        ffprobe: FfprobeInvocation {
            executable_path: ffprobe.to_owned(),
            version_first_line: first_line(version_output.stdout()),
            args: probe_args,
            status_code: probe_output.status_code,
            stderr: probe_output.stderr().trim().to_owned(),
        },
    })
    .map_err(CliError::from)
}

fn preflight_video_map_sources(recording_dir: &Path, video_path: &Path) -> CliResult<()> {
    let inspect_command = format!(
        "input-dynamics recording inspect --dir {}",
        shellish_path(recording_dir)?
    );
    let derive_timeline_command = format!(
        "input-dynamics derive timeline --recording-dir {}",
        shellish_path(recording_dir)?
    );
    require_local_file(
        &recording_dir.join("manifest.json"),
        "manifest",
        "recording_inspect",
        &inspect_command,
    )?;
    require_local_file(
        video_path,
        "video file",
        "recording_inspect",
        &inspect_command,
    )?;
    require_local_file(
        &recording_dir.join("video").join("timing.json"),
        "video timing metadata",
        "recording_inspect",
        &inspect_command,
    )?;
    require_local_file(
        &recording_dir
            .join("derived")
            .join("timeline")
            .join("index.json"),
        "timeline index",
        "derive_timeline",
        &derive_timeline_command,
    )?;
    require_local_file(
        &recording_dir
            .join("derived")
            .join("timeline")
            .join("events.jsonl"),
        "timeline events",
        "derive_timeline",
        &derive_timeline_command,
    )?;
    Ok(())
}

fn require_local_file(
    path: &Path,
    description: &str,
    suggested_action: &str,
    suggested_command: &str,
) -> CliResult<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(CliError::with_details(
            format!(
                "video_map_missing_source: missing {description}: {}",
                path.display()
            ),
            json!({
                "error_kind": "video_map_missing_source",
                "source_description": description,
                "path": path_string(path).unwrap_or_else(|_error| path.display().to_string()),
                "suggested_action": suggested_action,
                "suggested_kind": suggested_action,
                "suggested_command": suggested_command,
            }),
        ))
    }
}

fn ffprobe_version_args() -> Vec<String> {
    vec!["-version".to_owned()]
}

fn ffprobe_frame_args(video_path: &Path) -> CliResult<Vec<String>> {
    Ok(vec![
        "-v".to_owned(),
        "error".to_owned(),
        "-select_streams".to_owned(),
        "v:0".to_owned(),
        "-show_streams".to_owned(),
        "-show_frames".to_owned(),
        "-show_entries".to_owned(),
        "stream=index,codec_type,codec_name,width,height,duration,nb_frames,avg_frame_rate,r_frame_rate,time_base:frame=media_type,key_frame,pts,pts_time,best_effort_timestamp,best_effort_timestamp_time,duration,duration_time,pkt_size,width,height,pict_type".to_owned(),
        "-of".to_owned(),
        "json".to_owned(),
        path_string(video_path)?,
    ])
}

fn ffprobe_error(ffprobe: &str, args: &[String], error: &CliError) -> CliError {
    CliError::with_details(
        format!(
            "failed to run {} {}; install FFmpeg or enter the Nix development shell: {error}",
            ffprobe,
            args.join(" ")
        ),
        json!({
            "error_kind": "ffprobe_unavailable_or_version_failed",
            "program": ffprobe,
            "args": args,
            "suggested_action": "install_ffmpeg_or_enter_nix_shell",
        }),
    )
}

fn shellish_path(path: &Path) -> CliResult<String> {
    let text = path_string(path)?;
    Ok(shellish_text(&text))
}

fn shellish_text(text: &str) -> String {
    if text
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_./:".contains(character))
    {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_owned()
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

fn session(app: &App, command: SessionCommand) -> Value {
    match command {
        start @ SessionCommand::Start { .. } => session_start(app, start),
        SessionCommand::Status { run_id } => session_status(app, &SessionStatusRequest { run_id })
            .unwrap_or_else(|error| session_operation_error("session status", &error)),
        SessionCommand::Stop { run_id } => stop_session(app, &SessionStopRequest { run_id })
            .unwrap_or_else(|error| session_operation_error("session stop", &error)),
    }
}

struct MovedCommand {
    deprecated_argv: Vec<String>,
    moved_argv: Vec<String>,
    action: &'static str,
}

struct ControllerStartMigration {
    run_id: String,
    input_actor: Option<String>,
    input_controller: Option<String>,
    input_cadence_policy: Option<String>,
    input_profile: Option<PathBuf>,
    input_profile_seed: Option<u64>,
}

struct RejectedSessionFlagInputs<'a> {
    mode: Option<&'a str>,
    with_input_controller: bool,
    no_input_controller: bool,
    input_controller: Option<&'a str>,
    input_cadence_policy: Option<&'a str>,
}

struct SessionStartValidation<'a> {
    rejected: RejectedSessionFlagInputs<'a>,
    evidence: SessionStartEvidence,
    profile: SessionStartProfile<'a>,
}

struct SessionStartEvidence {
    with_evidence: bool,
    full_accessibility_evidence: bool,
    no_video: bool,
}

struct SessionStartProfile<'a> {
    input_profile: Option<&'a Path>,
    input_profile_seed: Option<u64>,
}

struct SessionWorkflowUnavailableInput<'a> {
    run_id: &'a str,
    out: &'a Path,
    actor: &'a str,
    with_evidence: bool,
    full_accessibility_evidence: bool,
    no_video: bool,
    input_profile: Option<&'a Path>,
    input_profile_seed: Option<u64>,
}

fn input_actor_is_umbrella(input_actor: Option<&str>) -> bool {
    matches!(input_actor, Some("human" | "agent"))
}

fn rejected_session_start_flags(inputs: &RejectedSessionFlagInputs<'_>) -> Vec<String> {
    let mut rejected = Vec::new();
    if inputs.mode.is_some() {
        rejected.push(String::from("--mode"));
    }
    if inputs.with_input_controller {
        rejected.push(String::from("--with-input-controller"));
    }
    if inputs.no_input_controller {
        rejected.push(String::from("--no-input-controller"));
    }
    if inputs.input_controller.is_some() {
        rejected.push(String::from("--input-controller"));
    }
    if inputs.input_cadence_policy.is_some() {
        rejected.push(String::from("--input-cadence-policy"));
    }
    rejected
}

fn session_start_out_required(run_id: &str, actor: Option<&str>) -> Value {
    let details = json!({
        "run_id": run_id,
        "input_actor": actor,
        "suggested_next_command": {
            "argv": ["input-dynamics", "session", "start", "--input-actor", actor.unwrap_or("<human|agent>"), "--run-id", run_id, "--out", "<run-dir>"],
            "reason": "--out selects reserved session start handling and prevents routing an intended human/agent run to diagnostic controller start",
        },
    });
    session_command_error(
        "session start",
        "session_start_out_required",
        "session start requires --out when --input-actor is human or agent; controller-era session start moved to controller start",
        &details,
    )
}

fn session_input_actor_required(run_id: &str, out: &Path) -> Value {
    let details = json!({
        "run_id": run_id,
        "out": out.to_string_lossy(),
        "suggested_next_command": {
            "argv": ["input-dynamics", "session", "start", "--input-actor", "human", "--run-id", run_id, "--out", out.to_string_lossy().as_ref()],
            "reason": "choose who is producing input for the recording run",
        },
    });
    session_command_error(
        "session start",
        "session_input_actor_required",
        "session start requires --input-actor human or --input-actor agent",
        &details,
    )
}

fn session_input_actor_invalid(run_id: &str, out: &Path, actor: &str) -> Value {
    let details = json!({
        "run_id": run_id,
        "out": out.to_string_lossy(),
        "input_actor": actor,
        "allowed_input_actors": ["human", "agent"],
    });
    session_command_error(
        "session start",
        "session_input_actor_invalid",
        "session input actor must be human or agent",
        &details,
    )
}

fn unsupported_session_flags(
    run_id: &str,
    out: &Path,
    actor: &str,
    rejected_flags: &[String],
) -> Value {
    let details = json!({
        "run_id": run_id,
        "out": out.to_string_lossy(),
        "input_actor": actor,
        "rejected_flags": rejected_flags,
        "suggested_next_command": {
            "argv": ["input-dynamics", "session", "start", "--input-actor", actor, "--run-id", run_id, "--out", out.to_string_lossy().as_ref()],
            "reason": "--input-actor is the single normal control for input provenance",
        },
    });
    session_command_error(
        "session start",
        "unsupported_session_flag",
        "session start derives controller and cadence settings from --input-actor",
        &details,
    )
}

fn session_input_profile_not_allowed(
    run_id: &str,
    out: &Path,
    input_profile: Option<&Path>,
    input_profile_seed: Option<u64>,
) -> Value {
    let details = json!({
        "run_id": run_id,
        "out": out.to_string_lossy(),
        "input_actor": "human",
        "input_profile": input_profile.map(|path| path.to_string_lossy().to_string()),
        "input_profile_seed": input_profile_seed,
        "suggested_next_command": {
            "argv": ["input-dynamics", "session", "start", "--input-actor", "human", "--run-id", run_id, "--out", out.to_string_lossy().as_ref()],
            "reason": "human sessions use manual cadence and do not have generated input profile provenance",
        },
    });
    session_command_error(
        "session start",
        "session_input_profile_not_allowed",
        "human sessions cannot use input profiles or profile seeds",
        &details,
    )
}

fn session_workflow_unavailable(input: &SessionWorkflowUnavailableInput<'_>) -> Value {
    let input_controller = derived_session_input_controller(input.actor);
    let input_cadence_policy = derived_session_cadence_policy(input.actor);
    let profile_source = derived_session_profile_source(input.actor, input.input_profile);
    let input_profile = input
        .input_profile
        .map(|path| path.to_string_lossy().to_string());
    let details = json!({
        "run_id": input.run_id,
        "out": input.out.to_string_lossy(),
        "input_actor": input.actor,
        "input_controller": input_controller,
        "input_cadence_policy": input_cadence_policy,
        "with_evidence": input.with_evidence,
        "full_accessibility_evidence": input.full_accessibility_evidence,
        "no_video": input.no_video,
        "input_profile": input_profile,
        "input_profile_seed": input.input_profile_seed,
        "profile_provenance": null,
        "input_provenance": {
            "input_actor": input.actor,
            "input_controller": input_controller,
            "input_cadence_policy": input_cadence_policy,
            "profile_source": profile_source,
            "input_profile": input_profile,
            "input_profile_seed": input.input_profile_seed,
        },
        "availability": "reserved",
        "reason_code": "not_available",
        "suggested_next_command": {
            "argv": ["input-dynamics", "record", "--run-id", input.run_id, "--out", input.out.to_string_lossy().as_ref(), "--duration-ms", "<positive-ms>"],
            "reason": "use record for bounded capture in this build",
        },
    });
    session_command_error(
        "session start",
        "session_workflow_unavailable",
        "session start is reserved in this build and did not mutate state",
        &details,
    )
}

fn derived_session_input_controller(actor: &str) -> Option<&'static str> {
    (actor == "agent").then_some(INPUT_CONTROLLER_CLI)
}

fn derived_session_cadence_policy(actor: &str) -> &'static str {
    if actor == "agent" {
        "input_profile"
    } else {
        "manual"
    }
}

fn derived_session_profile_source(
    actor: &str,
    input_profile: Option<&Path>,
) -> Option<&'static str> {
    if actor != "agent" {
        None
    } else if input_profile.is_some() {
        Some("local")
    } else {
        Some("bundled")
    }
}

fn session_command_error(command: &str, error_code: &str, message: &str, details: &Value) -> Value {
    json!({
        "schema": COMMAND_RESULT_SCHEMA,
        "ok": false,
        "command": command,
        "error_code": error_code,
        "message": message,
        "details": details,
        "mutated": false,
    })
}

fn moved_command_value(moved_command: &MovedCommand) -> Value {
    let message = format!(
        "controller-only session commands moved to input-dynamics controller {}",
        moved_command.action
    );
    json!({
        "schema": COMMAND_MIGRATION_SCHEMA,
        "ok": false,
        "error_code": "command_moved",
        "command": moved_command.deprecated_argv,
        "command_normalized": true,
        "message": message,
        "deprecated_command": {
            "argv": moved_command.deprecated_argv,
            "normalized": true,
            "defaults_included": true,
        },
        "moved_to": {
            "argv": moved_command.moved_argv,
        },
        "suggested_next_command": {
            "argv": moved_command.moved_argv,
            "reason": "diagnostic controller lifecycle moved out of the reserved session namespace",
        },
        "diagnostic_only": true,
        "mutated": false,
    })
}

fn session_start(app: &App, command: SessionCommand) -> Value {
    let SessionCommand::Start {
        run_id,
        out,
        input_actor,
        mode,
        with_input_controller,
        no_input_controller,
        input_controller,
        input_cadence_policy,
        with_evidence,
        full_accessibility_evidence,
        no_video,
        input_profile,
        input_profile_seed,
    } = command
    else {
        return moved_session_command(command);
    };
    let Some(out_path) = out else {
        return session_start_without_out(&ControllerStartMigration {
            run_id,
            input_actor,
            input_controller,
            input_cadence_policy,
            input_profile,
            input_profile_seed,
        });
    };
    session_start_with_out(
        app,
        &run_id,
        &out_path,
        input_actor.as_deref(),
        &SessionStartValidation {
            rejected: RejectedSessionFlagInputs {
                mode: mode.as_deref(),
                with_input_controller,
                no_input_controller,
                input_controller: input_controller.as_deref(),
                input_cadence_policy: input_cadence_policy.as_deref(),
            },
            evidence: SessionStartEvidence {
                with_evidence,
                full_accessibility_evidence,
                no_video,
            },
            profile: SessionStartProfile {
                input_profile: input_profile.as_deref(),
                input_profile_seed,
            },
        },
    )
}

fn session_start_without_out(config: &ControllerStartMigration) -> Value {
    if input_actor_is_umbrella(config.input_actor.as_deref()) {
        session_start_out_required(&config.run_id, config.input_actor.as_deref())
    } else {
        moved_command_value(&moved_controller_start_argv(config))
    }
}

fn session_start_with_out(
    app: &App,
    run_id: &str,
    out_path: &Path,
    input_actor: Option<&str>,
    validation: &SessionStartValidation<'_>,
) -> Value {
    let Some(actor) = input_actor else {
        return session_input_actor_required(run_id, out_path);
    };
    if !matches!(actor, "human" | "agent") {
        return session_input_actor_invalid(run_id, out_path, actor);
    }
    let rejected_flags = rejected_session_start_flags(&validation.rejected);
    if !rejected_flags.is_empty() {
        return unsupported_session_flags(run_id, out_path, actor, &rejected_flags);
    }
    if actor == "human"
        && (validation.profile.input_profile.is_some()
            || validation.profile.input_profile_seed.is_some())
    {
        return session_input_profile_not_allowed(
            run_id,
            out_path,
            validation.profile.input_profile,
            validation.profile.input_profile_seed,
        );
    }
    if actor == "human" {
        return start_human_session(
            app,
            &HumanSessionStart {
                run_id: String::from(run_id),
                out: out_path.to_path_buf(),
                with_evidence: validation.evidence.with_evidence,
                full_accessibility_evidence: validation.evidence.full_accessibility_evidence,
                video_enabled: !validation.evidence.no_video,
            },
        )
        .unwrap_or_else(|error| session_operation_error("session start", &error));
    }
    session_workflow_unavailable(&SessionWorkflowUnavailableInput {
        run_id,
        out: out_path,
        actor,
        with_evidence: validation.evidence.with_evidence,
        full_accessibility_evidence: validation.evidence.full_accessibility_evidence,
        no_video: validation.evidence.no_video,
        input_profile: validation.profile.input_profile,
        input_profile_seed: validation.profile.input_profile_seed,
    })
}

fn moved_session_command(command: SessionCommand) -> Value {
    let moved_command = moved_session_argv(command);
    moved_command_value(&moved_command)
}

fn moved_session_argv(command: SessionCommand) -> MovedCommand {
    match command {
        SessionCommand::Start {
            run_id,
            input_actor,
            input_controller,
            input_cadence_policy,
            input_profile,
            input_profile_seed,
            ..
        } => moved_controller_start_argv(&ControllerStartMigration {
            run_id,
            input_actor,
            input_controller,
            input_cadence_policy,
            input_profile,
            input_profile_seed,
        }),
        SessionCommand::Status { .. } => MovedCommand {
            deprecated_argv: command_argv("session", "status"),
            moved_argv: command_argv("controller", "status"),
            action: "status",
        },
        SessionCommand::Stop { .. } => MovedCommand {
            deprecated_argv: command_argv("session", "stop"),
            moved_argv: command_argv("controller", "stop"),
            action: "stop",
        },
    }
}

fn session_operation_error(command: &str, error: &CliError) -> Value {
    let mut value = error.to_json();
    if let Some(object) = value.as_object_mut() {
        object.insert(String::from("schema"), json!(COMMAND_RESULT_SCHEMA));
        object.insert(String::from("command"), json!(command));
        object
            .entry(String::from("mutated"))
            .or_insert_with(|| json!(false));
    }
    value
}

fn moved_controller_start_argv(config: &ControllerStartMigration) -> MovedCommand {
    MovedCommand {
        deprecated_argv: controller_start_argv("session", config),
        moved_argv: controller_start_argv("controller", config),
        action: "start",
    }
}

fn controller_start_argv(namespace: &str, config: &ControllerStartMigration) -> Vec<String> {
    let mut argv = vec![
        String::from("input-dynamics"),
        String::from(namespace),
        String::from("start"),
        String::from("--run-id"),
        config.run_id.clone(),
        String::from("--input-actor"),
        config
            .input_actor
            .clone()
            .unwrap_or_else(|| String::from("agent_adb")),
        String::from("--input-controller"),
        config
            .input_controller
            .clone()
            .unwrap_or_else(|| String::from(INPUT_CONTROLLER_CLI)),
        String::from("--input-cadence-policy"),
        config
            .input_cadence_policy
            .clone()
            .unwrap_or_else(|| String::from("input_profile")),
    ];
    if let Some(profile) = config.input_profile.as_deref() {
        argv.extend([
            String::from("--input-profile"),
            profile.to_string_lossy().into_owned(),
        ]);
    }
    if let Some(seed) = config.input_profile_seed {
        argv.extend([String::from("--input-profile-seed"), seed.to_string()]);
    }
    argv
}

fn command_argv(namespace: &str, action: &str) -> Vec<String> {
    vec![
        String::from("input-dynamics"),
        String::from(namespace),
        String::from(action),
    ]
}

fn controller_not_active_error(context: &str) -> CliError {
    CliError::with_details(
        format!("no active diagnostic input controller; run `{DIAGNOSTIC_CONTROLLER_START_HINT}`"),
        json!({
            "error_code": "controller_not_active",
            "context": context,
            "suggested_next_command": {
                "argv": ["input-dynamics", "controller", "start", "--run-id", "<id>"],
                "reason": "diagnostic controller lifecycle is required for live input commands during the session migration",
            },
            "diagnostic_only": true,
            "mutated": false,
        }),
    )
}

fn controller_not_ready_error(lock_state: &str) -> CliError {
    CliError::with_details(
        format!("diagnostic input controller is not ready; session_lock.state={lock_state}"),
        json!({
            "error_code": "controller_not_ready",
            "session_lock_state": lock_state,
            "suggested_next_command": {
                "argv": ["input-dynamics", "controller", "status"],
                "reason": "inspect diagnostic controller readiness before issuing live input",
            },
            "diagnostic_only": true,
            "mutated": false,
        }),
    )
}

fn controller_start_lifecycle(app: &App, config: &SessionStartConfig<'_>) -> CliResult<Value> {
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

fn controller_lifecycle_status(app: &App) -> CliResult<Value> {
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

fn controller_lifecycle_stop(app: &App) -> CliResult<Value> {
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

fn keyboard(app: &App, command: &KeyboardCommand) -> CliResult<Value> {
    match command {
        &KeyboardCommand::EnsureVisible => ensure_keyboard_visible(app),
    }
}

fn ensure_keyboard_visible(app: &App) -> CliResult<Value> {
    let before = app.broadcast("KEYBOARD_LAYOUT", Vec::new())?;
    if layout_visible(&before)? {
        ensure_visible_layout_has_loggable_scope(&before)?;
        return Ok(json!({
            "ok": true,
            "package_name": app.package(),
            "already_visible": true,
            "action": "none",
            "layout": before,
        }));
    }

    let input_status = controller::status(app)?;
    if !input_status
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(controller_not_active_error("keyboard_ensure_visible"));
    }

    let accessibility =
        observe::accessibility(app, None, observe::AccessibilityDetail::Compressed)?;
    let xml = accessibility
        .get("xml")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::new("accessibility XML was not present"))?;
    let editable = keyboard_visible_target_node(xml)?;
    let action = if editable.focused {
        "tap_focused_editable"
    } else {
        "tap_single_editable"
    };
    let center = editable.bounds.center()?;
    let tap_output = controller::tap(
        app,
        controller::ControllerTapSpec {
            fallback: TapSpec::new(center.x, center.y),
            key_context: None,
            inter_key_delay_sampling: InterKeyDelaySampling::Skip,
        },
    )?;
    let after = wait_for_layout(app, LayoutWait::Visible)?;
    let scope = wait_for_input_scope_ready(app)?;
    let visible_after = layout_visible(&after).unwrap_or(false);
    let tap_ok = tap_output
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ok = tap_ok && visible_after;
    let error = if ok {
        Value::Null
    } else {
        json!("keyboard did not become visible after tapping the editable field")
    };

    Ok(json!({
        "ok": ok,
        "package_name": app.package(),
        "already_visible": false,
        "action": action,
        "editable": editable.to_json(center),
        "tap": tap_output,
        "layout": after,
        "input_scope": scope,
        "error": error,
    }))
}

fn run_controller_command(app: &App, command: ControllerCommand) -> CliResult<Value> {
    match command {
        ControllerCommand::Start {
            run_id,
            input_actor,
            input_controller,
            input_cadence_policy,
            input_profile,
            input_profile_seed,
        } => controller_start_lifecycle(
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
        ControllerCommand::Status => controller_lifecycle_status(app),
        ControllerCommand::Stop => controller_lifecycle_stop(app),
        ControllerCommand::Run {
            socket,
            state,
            uinput_stdout,
            uinput_stderr,
            events,
            final_state,
            controller_invocation_id,
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
                events,
                final_state,
                controller_invocation_id,
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
    if !layout_visible(&before)? {
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

fn wait_for_input_scope_ready(app: &App) -> CliResult<Value> {
    let start = Instant::now();
    loop {
        let status = app.broadcast("STATUS", Vec::new())?;
        if status
            .get("input_scope_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(status);
        }
        if start.elapsed() >= LAYOUT_WAIT_TIMEOUT {
            return Err(input_scope_not_ready_error(
                "wait_for_input_scope_ready",
                &status,
            ));
        }
        std::thread::sleep(LAYOUT_POLL_INTERVAL);
    }
}

fn ensure_visible_layout_has_loggable_scope(status: &Value) -> CliResult<()> {
    if !status
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(());
    }
    if status
        .get("input_scope_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(());
    }
    Err(input_scope_not_ready_error(
        "visible_layout_has_loggable_scope",
        status,
    ))
}

fn ensure_logged_input_ready(app: &App) -> CliResult<Value> {
    let input = controller::status(app)?;
    if !input
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(controller_not_active_error("logged_input"));
    }
    if !input
        .get("ready_for_input")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let lock_state = input
            .pointer("/session_lock/state")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        return Err(controller_not_ready_error(lock_state));
    }

    let ime = app.broadcast("STATUS", Vec::new())?;
    if ime.get("active").and_then(Value::as_bool) != Some(true) {
        return Err(input_scope_not_ready_error("ime_logging_inactive", &ime));
    }
    if ime
        .get("input_scope_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(json!({
            "ime": ime,
            "input": input,
        }));
    }
    Err(input_scope_not_ready_error("logged_input_scope", &ime))
}

fn input_scope_state(status: &Value) -> &str {
    status
        .get("input_scope_state")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn input_scope_not_ready_error(context: &str, status: &Value) -> CliError {
    CliError::with_details(
        format!(
            "logging input scope is not ready; input_scope_state={}",
            input_scope_state(status)
        ),
        json!({
            "error_code": "input_scope_not_ready",
            "context": context,
            "ime_active": status.get("active").and_then(Value::as_bool).unwrap_or(false),
            "input_scope_ready": status
                .get("input_scope_ready")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "input_scope_state": input_scope_state(status),
            "ime_status": status,
            "suggested_next_command": {
                "argv": ["input-dynamics", "controller", "status"],
                "reason": "inspect diagnostic controller and IME readiness before issuing live input",
            },
            "diagnostic_only": true,
            "mutated": false,
        }),
    )
}

fn layout_matches(status: &Value, wait: LayoutWait) -> CliResult<bool> {
    let visible = layout_visible(status)?;
    Ok(match wait {
        LayoutWait::Current => true,
        LayoutWait::Visible => visible,
        LayoutWait::Hidden => !visible,
    })
}

fn layout_visible(status: &Value) -> CliResult<bool> {
    Ok(keyboard_layout_visible(keyboard_layout_value(status)?))
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
    let readiness = ensure_logged_input_ready(app)?;

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
        "readiness": readiness,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScreenBounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditableNode {
    class_name: String,
    bounds: ScreenBounds,
    focused: bool,
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

impl ScreenBounds {
    fn center(self) -> CliResult<TouchPoint> {
        Ok(TouchPoint::new(
            midpoint(self.left, self.right, "x")?,
            midpoint(self.top, self.bottom, "y")?,
        ))
    }

    fn to_json(self) -> Value {
        json!({
            "left": self.left,
            "top": self.top,
            "right": self.right,
            "bottom": self.bottom,
        })
    }
}

impl EditableNode {
    fn to_json(&self, center: TouchPoint) -> Value {
        json!({
            "class": self.class_name,
            "password": false,
            "focused": self.focused,
            "bounds": self.bounds.to_json(),
            "tap": {
                "x": center.x,
                "y": center.y,
            },
        })
    }
}

fn midpoint(start: i32, end: i32, axis: &str) -> CliResult<i32> {
    let span = end
        .checked_sub(start)
        .ok_or_else(|| CliError::new(format!("{axis} bounds overflow")))?;
    let half = span
        .checked_div(2_i32)
        .ok_or_else(|| CliError::new(format!("{axis} bounds cannot be halved")))?;
    start
        .checked_add(half)
        .ok_or_else(|| CliError::new(format!("{axis} center overflow")))
}

fn keyboard_visible_target_node(xml: &str) -> CliResult<EditableNode> {
    let mut focused_password_editable = false;
    let mut fallback_candidate = None;
    let mut fallback_count = 0_u64;
    for raw_node in xml.split("<node").skip(1) {
        let node = raw_node
            .split_once('>')
            .map_or(raw_node, |(attributes, _)| attributes);
        let focused = xml_bool_attribute(node, "focused");
        let enabled = !matches!(xml_bool_attribute(node, "enabled"), Some(false));
        if !enabled {
            continue;
        }

        let class_name = xml_attribute(node, "class").unwrap_or_default();
        if !editable_class_name(class_name) {
            continue;
        }

        if xml_bool_attribute(node, "password") == Some(true) {
            focused_password_editable = true;
            continue;
        }

        let bounds_text = xml_attribute(node, "bounds")
            .ok_or_else(|| CliError::new("editable node is missing bounds"))?;
        let candidate = EditableNode {
            class_name: String::from(class_name),
            bounds: parse_screen_bounds(bounds_text)?,
            focused: focused == Some(true),
        };
        if candidate.focused {
            return Ok(candidate);
        }
        fallback_count = fallback_count.saturating_add(1);
        if fallback_candidate.is_none() {
            fallback_candidate = Some(candidate);
        }
    }

    if focused_password_editable {
        Err(CliError::new(
            "focused editable field is password-protected; refusing to show keyboard",
        ))
    } else if fallback_count == 1 {
        fallback_candidate.ok_or_else(|| {
            CliError::new("internal error: single editable candidate was not retained")
        })
    } else if fallback_count > 1 {
        Err(CliError::new(
            "multiple non-password editable fields were found; focus one before running `input-dynamics keyboard ensure-visible`",
        ))
    } else {
        Err(CliError::new(
            "no non-password editable field was found; focus an editable field first",
        ))
    }
}

fn editable_class_name(class_name: &str) -> bool {
    class_name.ends_with("EditText")
        || class_name.ends_with("AutoCompleteTextView")
        || class_name.ends_with("MultiAutoCompleteTextView")
}

fn xml_bool_attribute(node: &str, name: &str) -> Option<bool> {
    match xml_attribute(node, name)? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn xml_attribute<'a>(node: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=\"");
    let (_, rest) = node.split_once(marker.as_str())?;
    let (value, _) = rest.split_once('"')?;
    Some(value)
}

fn parse_screen_bounds(value: &str) -> CliResult<ScreenBounds> {
    let parts = value
        .split(['[', ']', ','])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err(CliError::new(format!(
            "bounds should contain four values: {value}"
        )));
    }
    let left = parse_bound(parts.first().copied(), "left")?;
    let top = parse_bound(parts.get(1).copied(), "top")?;
    let right = parse_bound(parts.get(2).copied(), "right")?;
    let bottom = parse_bound(parts.get(3).copied(), "bottom")?;
    let bounds = ScreenBounds {
        left,
        top,
        right,
        bottom,
    };
    if bounds.right < bounds.left || bounds.bottom < bounds.top {
        return Err(CliError::new(format!("bounds are inverted: {value}")));
    }
    Ok(bounds)
}

fn parse_bound(raw: Option<&str>, field: &str) -> CliResult<i32> {
    let text = raw.ok_or_else(|| CliError::new(format!("bounds are missing {field}")))?;
    text.parse::<i32>()
        .map_err(|error| CliError::new(format!("invalid bounds {field}: {text}: {error}")))
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

    let readiness = ensure_logged_input_ready(app)?;
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
        "readiness": readiness,
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
    if keyboard_layout_visible(layout) {
        Ok(())
    } else {
        let reason = layout
            .get("unavailable_reason")
            .and_then(Value::as_str)
            .unwrap_or("keyboard_view_not_visible");
        Err(CliError::new(format!(
            "keyboard is hidden ({reason}); run `input-dynamics keyboard ensure-visible` or focus a non-password editable field first"
        )))
    }
}

fn keyboard_layout_visible(layout: &Value) -> bool {
    layout
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && layout
            .get("keyboard_view_visible")
            .and_then(Value::as_bool)
            .unwrap_or(false)
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
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use clap::Parser;
    use proptest::strategy::Strategy;
    use serde_json::Value;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    use crate::app::App;
    use crate::args::{Cli, Commands, PressKey, SessionCommand};
    use crate::commands::{
        EditableNode, ScreenBounds, TypeKeyTarget, controller_not_active_error,
        controller_not_ready_error, derive_video_map_command, ime_is_registered,
        input_scope_not_ready_error, is_debug_apk, keyboard_layout_visible,
        keyboard_visible_target_node, latest_release_tag_from_json, moved_session_command,
        planned_type_step, press_key_code, session, type_key_target,
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0_u64);

    #[test]
    fn old_session_start_returns_command_moved_without_mutation() {
        let result = moved_session_command(SessionCommand::Start {
            run_id: String::from("run-test"),
            out: None,
            input_actor: Some(String::from("agent_adb")),
            mode: None,
            with_input_controller: false,
            no_input_controller: false,
            input_controller: Some(String::from("input-dynamics-cli")),
            input_cadence_policy: Some(String::from("input_profile")),
            with_evidence: false,
            full_accessibility_evidence: false,
            no_video: false,
            input_profile: Some(PathBuf::from("profiles/custom.json")),
            input_profile_seed: Some(7_u64),
        });

        assert_command_moved(&result, "start");
        assert_eq!(
            result.pointer("/moved_to/argv/1").and_then(Value::as_str),
            Some("controller"),
            "old session start should move to controller start"
        );
        assert_eq!(
            result
                .pointer("/deprecated_command/normalized")
                .and_then(Value::as_bool),
            Some(true),
            "deprecated command argv should be marked as normalized"
        );
        assert!(
            result
                .pointer("/moved_to/argv")
                .and_then(Value::as_array)
                .is_some_and(|argv| argv.iter().any(|value| value == "profiles/custom.json")),
            "moved command should preserve input profile path"
        );
    }

    #[test]
    fn umbrella_session_start_returns_unavailable_without_mutation() {
        let app = test_app();
        let out = unique_temp_dir("session-parser-unavailable");
        let result = session(
            &app,
            SessionCommand::Start {
                run_id: String::from("run-test"),
                out: Some(out.clone()),
                input_actor: Some(String::from("agent")),
                mode: None,
                with_input_controller: false,
                no_input_controller: false,
                input_controller: None,
                input_cadence_policy: None,
                with_evidence: true,
                full_accessibility_evidence: false,
                no_video: false,
                input_profile: None,
                input_profile_seed: Some(7_u64),
            },
        );

        assert_session_error(&result, "session_workflow_unavailable");
        assert_eq!(
            result
                .pointer("/details/input_actor")
                .and_then(Value::as_str),
            Some("agent"),
            "result should preserve actor"
        );
        assert_eq!(
            result
                .pointer("/details/input_controller")
                .and_then(Value::as_str),
            Some("input-dynamics-cli"),
            "agent result should expose derived controller provenance"
        );
        assert_eq!(
            result
                .pointer("/details/input_cadence_policy")
                .and_then(Value::as_str),
            Some("input_profile"),
            "agent result should expose derived cadence provenance"
        );
        assert_eq!(
            result
                .pointer("/details/input_provenance/profile_source")
                .and_then(Value::as_str),
            Some("bundled"),
            "agent parser-only result should preserve profile source without loading profile files"
        );
        assert_eq!(
            result.pointer("/details/profile_provenance"),
            Some(&Value::Null),
            "parser-only unavailable result should not claim loaded profile provenance"
        );
        assert!(
            !out.exists(),
            "parser-only unavailable result must not create the output directory"
        );
    }

    #[test]
    fn umbrella_session_start_rejects_missing_actor_and_human_profile_without_mutation() {
        let app = test_app();
        let missing_actor_out = unique_temp_dir("session-parser-missing-actor");
        let profile_out = unique_temp_dir("session-parser-human-profile");
        let missing_actor = session(
            &app,
            SessionCommand::Start {
                run_id: String::from("run-test"),
                out: Some(missing_actor_out.clone()),
                input_actor: None,
                mode: None,
                with_input_controller: false,
                no_input_controller: false,
                input_controller: None,
                input_cadence_policy: None,
                with_evidence: false,
                full_accessibility_evidence: false,
                no_video: false,
                input_profile: None,
                input_profile_seed: None,
            },
        );
        let human_profile = session(
            &app,
            SessionCommand::Start {
                run_id: String::from("run-test"),
                out: Some(profile_out.clone()),
                input_actor: Some(String::from("human")),
                mode: None,
                with_input_controller: false,
                no_input_controller: false,
                input_controller: None,
                input_cadence_policy: None,
                with_evidence: false,
                full_accessibility_evidence: false,
                no_video: false,
                input_profile: None,
                input_profile_seed: Some(7_u64),
            },
        );

        assert_session_error(&missing_actor, "session_input_actor_required");
        assert_session_error(&human_profile, "session_input_profile_not_allowed");
        for out in [missing_actor_out, profile_out] {
            assert!(
                !out.exists(),
                "validation-only session start must not create output directory"
            );
        }
    }

    #[test]
    fn umbrella_actor_without_out_does_not_move_to_controller() {
        let app = test_app();
        let result = session(
            &app,
            SessionCommand::Start {
                run_id: String::from("run-test"),
                out: None,
                input_actor: Some(String::from("human")),
                mode: None,
                with_input_controller: false,
                no_input_controller: false,
                input_controller: None,
                input_cadence_policy: None,
                with_evidence: false,
                full_accessibility_evidence: false,
                no_video: false,
                input_profile: None,
                input_profile_seed: None,
            },
        );

        assert_session_error(&result, "session_start_out_required");
    }

    #[test]
    fn umbrella_session_start_rejects_controller_flags_without_mutation() {
        let app = test_app();
        let out = unique_temp_dir("session-parser-rejected-flags");
        let result = session(
            &app,
            SessionCommand::Start {
                run_id: String::from("run-test"),
                out: Some(out.clone()),
                input_actor: Some(String::from("agent")),
                mode: Some(String::from("agent")),
                with_input_controller: true,
                no_input_controller: false,
                input_controller: Some(String::from("input-dynamics-cli")),
                input_cadence_policy: None,
                with_evidence: false,
                full_accessibility_evidence: false,
                no_video: false,
                input_profile: None,
                input_profile_seed: None,
            },
        );

        assert_session_error(&result, "unsupported_session_flag");
        assert!(
            result
                .pointer("/details/rejected_flags")
                .and_then(Value::as_array)
                .is_some_and(
                    |flags| flags.iter().any(|flag| flag == "--with-input-controller")
                        && flags.iter().any(|flag| flag == "--input-controller")
                ),
            "result should identify rejected flags"
        );
        assert!(
            !out.exists(),
            "rejected-flag validation must not create output directory"
        );
    }

    #[test]
    fn umbrella_session_start_rejects_invalid_actors_without_mutation() {
        for actor in ["agent_adb", "robot"] {
            let out = unique_temp_dir(&format!("session-parser-invalid-actor-{actor}"));
            let result = parsed_session_result(vec![
                String::from("input-dynamics"),
                String::from("session"),
                String::from("start"),
                String::from("--input-actor"),
                String::from(actor),
                String::from("--run-id"),
                String::from("run-test"),
                String::from("--out"),
                path_string_lossy(&out),
            ]);

            assert_session_error(&result, "session_input_actor_invalid");
            assert_eq!(
                result.pointer("/details/allowed_input_actors"),
                Some(&json!(["human", "agent"])),
                "invalid actor response should expose the allowed vocabulary"
            );
            assert!(
                !out.exists(),
                "invalid actor validation must not create output directory"
            );
        }
    }

    #[test]
    fn umbrella_session_start_rejects_all_controller_flags_from_argv() {
        struct RejectedFlagCase {
            expected_flag: &'static str,
            args: &'static [&'static str],
        }

        let cases = [
            RejectedFlagCase {
                expected_flag: "--mode",
                args: &["--mode", "agent"],
            },
            RejectedFlagCase {
                expected_flag: "--with-input-controller",
                args: &["--with-input-controller"],
            },
            RejectedFlagCase {
                expected_flag: "--no-input-controller",
                args: &["--no-input-controller"],
            },
            RejectedFlagCase {
                expected_flag: "--input-controller",
                args: &["--input-controller", "input-dynamics-cli"],
            },
            RejectedFlagCase {
                expected_flag: "--input-cadence-policy",
                args: &["--input-cadence-policy", "input_profile"],
            },
        ];

        for case in cases {
            let out = unique_temp_dir(&format!(
                "session-parser-rejected-{}",
                case.expected_flag.trim_start_matches("--")
            ));
            let mut argv = vec![
                String::from("input-dynamics"),
                String::from("session"),
                String::from("start"),
                String::from("--input-actor"),
                String::from("agent"),
                String::from("--run-id"),
                String::from("run-test"),
                String::from("--out"),
                path_string_lossy(&out),
            ];
            argv.extend(case.args.iter().copied().map(String::from));

            let result = parsed_session_result(argv);

            assert_session_error(&result, "unsupported_session_flag");
            assert!(
                result
                    .pointer("/details/rejected_flags")
                    .and_then(Value::as_array)
                    .is_some_and(|flags| flags.iter().any(|flag| flag == case.expected_flag)),
                "result should identify rejected flag {}",
                case.expected_flag
            );
            assert!(
                !out.exists(),
                "rejected flag validation must not create output directory"
            );
        }
    }

    #[test]
    fn umbrella_session_start_human_profile_path_is_rejected_without_reading() {
        let out = unique_temp_dir("session-parser-human-profile-path");
        let profile = unique_temp_dir("missing-human-profile").join("profile.json");
        let result = parsed_session_result(vec![
            String::from("input-dynamics"),
            String::from("session"),
            String::from("start"),
            String::from("--input-actor"),
            String::from("human"),
            String::from("--run-id"),
            String::from("run-test"),
            String::from("--out"),
            path_string_lossy(&out),
            String::from("--input-profile"),
            path_string_lossy(&profile),
        ]);

        assert_session_error(&result, "session_input_profile_not_allowed");
        assert_eq!(
            result
                .pointer("/details/input_profile")
                .and_then(Value::as_str),
            Some(path_string_lossy(&profile).as_str()),
            "rejection payload should preserve the requested profile path"
        );
        assert!(
            !out.exists() && !profile.exists(),
            "human profile rejection must not create output or profile paths"
        );
    }

    #[test]
    fn umbrella_session_start_agent_profile_path_is_not_read_while_unavailable() {
        let out = unique_temp_dir("session-parser-agent-profile-path");
        let profile = unique_temp_dir("missing-agent-profile").join("profile.json");
        let result = parsed_session_result(vec![
            String::from("input-dynamics"),
            String::from("session"),
            String::from("start"),
            String::from("--input-actor"),
            String::from("agent"),
            String::from("--run-id"),
            String::from("run-test"),
            String::from("--out"),
            path_string_lossy(&out),
            String::from("--input-profile"),
            path_string_lossy(&profile),
            String::from("--input-profile-seed"),
            String::from("7"),
        ]);

        assert_session_error(&result, "session_workflow_unavailable");
        assert_eq!(
            result
                .pointer("/details/input_profile")
                .and_then(Value::as_str),
            Some(path_string_lossy(&profile).as_str()),
            "unavailable result should preserve the requested profile path without reading it"
        );
        assert_eq!(
            result
                .pointer("/details/input_provenance/profile_source")
                .and_then(Value::as_str),
            Some("local"),
            "agent result should classify explicit profile source without loading it"
        );
        assert_eq!(
            result
                .pointer("/details/input_profile_seed")
                .and_then(Value::as_u64),
            Some(7_u64),
            "unavailable result should preserve profile seed"
        );
        assert!(
            !out.exists() && !profile.exists(),
            "agent unavailable result must not create output or read/create profile paths"
        );
    }

    #[test]
    fn old_session_start_through_session_branch_preserves_legacy_defaults() {
        let result = parsed_session_result(vec![
            String::from("input-dynamics"),
            String::from("session"),
            String::from("start"),
            String::from("--run-id"),
            String::from("run-test"),
        ]);

        assert_command_moved(&result, "start");
        assert_eq!(
            result.pointer("/moved_to/argv"),
            Some(&json!([
                "input-dynamics",
                "controller",
                "start",
                "--run-id",
                "run-test",
                "--input-actor",
                "agent_adb",
                "--input-controller",
                "input-dynamics-cli",
                "--input-cadence-policy",
                "input_profile"
            ])),
            "legacy no-out session start should normalize moved controller defaults"
        );

        let agent_adb = parsed_session_result(vec![
            String::from("input-dynamics"),
            String::from("session"),
            String::from("start"),
            String::from("--run-id"),
            String::from("run-test"),
            String::from("--input-actor"),
            String::from("agent_adb"),
        ]);

        assert_command_moved(&agent_adb, "start");
        assert!(
            agent_adb
                .pointer("/moved_to/argv")
                .and_then(Value::as_array)
                .is_some_and(|argv| argv.windows(2).any(|pair| pair
                    == [
                        Value::String(String::from("--input-actor")),
                        Value::String(String::from("agent_adb"))
                    ])),
            "legacy no-out session start should preserve explicit agent_adb actor"
        );
    }

    #[test]
    fn readiness_errors_point_to_diagnostic_controller_namespace() {
        let inactive = controller_not_active_error("test").to_json();
        let not_ready = controller_not_ready_error("starting").to_json();

        assert_error_points_to_controller_start(&inactive);
        assert!(
            !not_ready.to_string().contains("session start"),
            "not-ready error should not recommend old session start: {not_ready}"
        );
        assert_eq!(
            not_ready.get("error_code").and_then(Value::as_str),
            Some("controller_not_ready"),
            "not-ready error should be branchable"
        );
    }

    #[test]
    fn input_scope_errors_point_to_controller_status() {
        let status = json!({
            "active": true,
            "input_scope_ready": false,
            "input_scope_state": "none",
        });

        let error = input_scope_not_ready_error("test_scope", &status).to_json();

        assert_eq!(
            error.get("error_code").and_then(Value::as_str),
            Some("input_scope_not_ready"),
            "input-scope readiness errors should be branchable"
        );
        assert_eq!(
            error
                .pointer("/details/suggested_next_command/argv/1")
                .and_then(Value::as_str),
            Some("controller"),
            "input-scope readiness errors should send agents to controller diagnostics"
        );
        assert_eq!(
            error.pointer("/details/mutated").and_then(Value::as_bool),
            Some(false),
            "readiness inspection failures should be non-mutating"
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn derive_video_map_uses_injected_ffprobe_and_records_provenance() {
        let root = unique_temp_dir("commands-video-map");
        let setup_result = create_video_map_command_fixture(&root);
        assert!(setup_result.is_ok(), "fixture setup should succeed");
        let Ok(ffprobe_path) = create_fake_ffprobe(&root) else {
            let _cleanup = fs::remove_dir_all(&root);
            return;
        };

        let result = derive_video_map_command(&root, None, &ffprobe_path.display().to_string());

        assert!(
            result.is_ok(),
            "fake ffprobe command should derive video map"
        );
        let args_log = fs::read_to_string(root.join("ffprobe-args.log"));
        assert!(args_log.is_ok(), "fake ffprobe args log should be readable");
        let Ok(args_text) = args_log else {
            let _cleanup = fs::remove_dir_all(&root);
            return;
        };
        assert!(
            args_text.contains("-version"),
            "command should probe ffprobe version"
        );
        assert!(
            args_text.contains("-show_frames"),
            "command should request frame metadata"
        );
        let index_result = read_json(&root.join("derived").join("video_map").join("index.json"));
        assert!(index_result.is_ok(), "video map index should be readable");
        let Ok(index) = index_result else {
            let _cleanup = fs::remove_dir_all(&root);
            return;
        };
        assert_eq!(
            index
                .pointer("/ffprobe/version_first_line")
                .and_then(Value::as_str),
            Some("ffprobe version fake"),
            "index should preserve version provenance"
        );
        assert!(
            index
                .pointer("/ffprobe/args")
                .and_then(Value::as_array)
                .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("-show_frames"))),
            "index should preserve frame-probe argv"
        );
        let _cleanup = fs::remove_dir_all(&root);
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

    #[test]
    fn planned_type_step_rejects_hidden_keyboard_layout() {
        let layout = hidden_layout_result();

        let result = planned_type_step(&layout, 0, 'a');

        assert!(
            result.is_err(),
            "hidden keyboard should fail before any key is pressed"
        );
        let error = result
            .err()
            .map_or(String::new(), |error| error.to_string());
        assert!(
            error.contains("keyboard is hidden"),
            "error should name hidden keyboard state: {error}"
        );
    }

    #[test]
    fn keyboard_layout_visibility_requires_view_visible() {
        let layout = json!({
            "available": true,
            "keyboard_view_visible": false
        });

        assert!(
            !keyboard_layout_visible(&layout),
            "available layout without visible view should not pass semantic preflight"
        );
    }

    #[test]
    fn keyboard_visible_target_prefers_focused_non_password_edit_text() {
        let xml = r#"
            <hierarchy>
              <node index="0" class="android.widget.TextView" focused="false" enabled="true" password="false" bounds="[0,0][10,10]" />
              <node index="1" class="android.widget.EditText" focused="true" enabled="true" password="false" bounds="[168,145][1244,397]" />
            </hierarchy>
        "#;

        assert_eq!(
            keyboard_visible_target_node(xml).ok(),
            Some(EditableNode {
                class_name: String::from("android.widget.EditText"),
                bounds: ScreenBounds {
                    left: 168,
                    top: 145,
                    right: 1244,
                    bottom: 397,
                },
                focused: true,
            }),
            "focused non-password editable node should be extracted without text"
        );
    }

    #[test]
    fn keyboard_visible_target_uses_single_unfocused_edit_text() {
        let xml = r#"
            <hierarchy>
              <node class="android.widget.EditText" focused="false" enabled="true" password="false" bounds="[168,145][1244,397]" />
            </hierarchy>
        "#;

        assert_eq!(
            keyboard_visible_target_node(xml).ok(),
            Some(EditableNode {
                class_name: String::from("android.widget.EditText"),
                bounds: ScreenBounds {
                    left: 168,
                    top: 145,
                    right: 1244,
                    bottom: 397,
                },
                focused: false,
            }),
            "single visible non-password editable node should be usable after IME dismissal"
        );
    }

    #[test]
    fn keyboard_visible_target_rejects_password_edit_text() {
        let xml = r#"
            <hierarchy>
              <node class="android.widget.EditText" focused="true" enabled="true" password="true" bounds="[0,0][100,100]" />
            </hierarchy>
        "#;

        let error = keyboard_visible_target_node(xml)
            .err()
            .map_or(String::new(), |error| error.to_string());

        assert!(
            error.contains("password-protected"),
            "password focused fields should be refused: {error}"
        );
    }

    #[test]
    fn keyboard_visible_target_requires_editable_node() {
        let xml = r#"
            <hierarchy>
              <node class="android.widget.TextView" focused="true" enabled="true" password="false" bounds="[0,0][100,100]" />
            </hierarchy>
        "#;

        let error = keyboard_visible_target_node(xml)
            .err()
            .map_or(String::new(), |error| error.to_string());

        assert!(
            error.contains("no non-password editable field"),
            "non-editable focused nodes should not be used: {error}"
        );
    }

    #[test]
    fn keyboard_visible_target_rejects_multiple_unfocused_edit_texts() {
        let xml = r#"
            <hierarchy>
              <node class="android.widget.EditText" focused="false" enabled="true" password="false" bounds="[0,0][100,100]" />
              <node class="android.widget.EditText" focused="false" enabled="true" password="false" bounds="[0,200][100,300]" />
            </hierarchy>
        "#;

        let error = keyboard_visible_target_node(xml)
            .err()
            .map_or(String::new(), |error| error.to_string());

        assert!(
            error.contains("multiple non-password editable fields"),
            "ambiguous editable fields should require explicit focus: {error}"
        );
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

    fn unique_temp_dir(label: &str) -> PathBuf {
        let counter = TEMP_COUNTER.fetch_add(1_u64, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "input-dynamics-{label}-{}-{counter}",
            std::process::id()
        ))
    }

    fn test_app() -> App {
        App::new(
            String::from("adb"),
            String::from("org.inputdynamics.ime.debug"),
            Some(String::from("test-device")),
        )
    }

    fn parsed_session_result(argv: Vec<String>) -> Value {
        let cli = match Cli::try_parse_from(argv) {
            Ok(cli) => cli,
            Err(error) => {
                return json!({
                    "test_error": "test argv did not parse",
                    "message": error.to_string(),
                });
            }
        };
        if let Commands::Session { command } = cli.command {
            session(&test_app(), command)
        } else {
            json!({
                "test_error": "test argv did not parse as session command",
            })
        }
    }

    fn path_string_lossy(path: &Path) -> String {
        path.to_string_lossy().to_string()
    }

    fn assert_session_error(value: &Value, error_code: &str) {
        assert_eq!(
            value.get("schema").and_then(Value::as_str),
            Some("input_dynamics_session_command_result.v1"),
            "session command error should use the stable parser-only schema"
        );
        assert_eq!(
            value.get("ok").and_then(Value::as_bool),
            Some(false),
            "session command error should not be ok"
        );
        assert_eq!(
            value.get("command").and_then(Value::as_str),
            Some("session start"),
            "session command error should identify the command"
        );
        assert_eq!(
            value.get("error_code").and_then(Value::as_str),
            Some(error_code),
            "session command error code should match"
        );
        assert_eq!(
            value.get("mutated").and_then(Value::as_bool),
            Some(false),
            "parser-only session command errors must be non-mutating"
        );
    }

    fn assert_command_moved(value: &Value, action: &str) {
        assert_eq!(
            value.get("schema").and_then(Value::as_str),
            Some("input_dynamics_command_migration.v1"),
            "moved session command should use migration schema"
        );
        assert_eq!(
            value.get("ok").and_then(Value::as_bool),
            Some(false),
            "moved command should be a handled unsuccessful JSON result"
        );
        assert_eq!(
            value.get("error_code").and_then(Value::as_str),
            Some("command_moved"),
            "moved command should have stable error code"
        );
        assert_eq!(
            value.get("mutated").and_then(Value::as_bool),
            Some(false),
            "moved command should be non-mutating"
        );
        assert_eq!(
            value
                .pointer("/deprecated_command/argv/1")
                .and_then(Value::as_str),
            Some("session"),
            "deprecated command should identify old namespace"
        );
        assert_eq!(
            value.pointer("/moved_to/argv/2").and_then(Value::as_str),
            Some(action),
            "moved command should identify replacement action"
        );
        assert_eq!(
            value.pointer("/suggested_next_command/argv"),
            value.pointer("/moved_to/argv"),
            "top-level suggested command should match moved argv"
        );
        assert!(
            value.to_string().contains("controller"),
            "moved command should point to controller namespace"
        );
    }

    fn assert_error_points_to_controller_start(value: &Value) {
        assert!(
            value.to_string().contains("controller start"),
            "error should recommend controller start: {value}"
        );
        assert!(
            !value.to_string().contains("session start"),
            "error should not recommend old session start: {value}"
        );
        assert_eq!(
            value.get("error_code").and_then(Value::as_str),
            Some("controller_not_active"),
            "inactive-controller error should be branchable"
        );
    }

    fn create_video_map_command_fixture(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(root.join("ime"))?;
        fs::create_dir_all(root.join("video"))?;
        let ime_path = root.join("ime").join("session-test.jsonl");
        fs::write(
            &ime_path,
            "{\"schema\":\"input_dynamics_event.v1\",\"event\":\"session_start\"}\n",
        )?;
        fs::write(
            root.join("manifest.json"),
            r#"{"schema":"input_dynamics_record_manifest.v1","external_run_id":"run-test"}"#,
        )?;
        fs::write(root.join("video").join("screen.mp4"), b"synthetic-video")?;
        fs::write(
            root.join("video").join("timing.json"),
            r#"{
                "schema":"input_dynamics_video_capture.v1",
                "start":{
                    "before":{"t_elapsed_realtime_ns":1000000000,"t_uptime_ns":100000000,"device_wall_ms":10000},
                    "after":{"t_elapsed_realtime_ns":1000000000,"t_uptime_ns":100000000,"device_wall_ms":10000}
                },
                "stop":{
                    "before":{"t_elapsed_realtime_ns":1100000000,"t_uptime_ns":200000000,"device_wall_ms":10100},
                    "after":{"t_elapsed_realtime_ns":1100000000,"t_uptime_ns":200000000,"device_wall_ms":10100}
                }
            }"#,
        )?;
        fs::create_dir_all(root.join("derived").join("timeline"))?;
        fs::write(
            root.join("derived").join("timeline").join("index.json"),
            serde_json::to_string(&json!({
                "schema": "input_dynamics_timeline_index.v1",
                "event_count": 1_u64,
                "sources": [
                    {
                        "kind": "ime_jsonl",
                        "path": "ime/session-test.jsonl",
                        "exists": true,
                        "required": true,
                        "record_count": 1_u64,
                        "fingerprint": test_file_fingerprint(&ime_path)?,
                    }
                ]
            }))?,
        )?;
        fs::write(
            root.join("derived").join("timeline").join("events.jsonl"),
            "{\"schema\":\"input_dynamics_timeline_event.v1\",\"timeline_event_id\":\"timeline:000001\",\"event\":\"key_down\",\"record_kind\":\"ime_event\",\"clock_domain\":\"android_uptime_ms\",\"source_time\":{\"source_clock_domain\":\"android_uptime_ms\",\"source_time_status\":\"canonical_event_time_metadata\",\"source_time_ms\":150}}\n",
        )?;
        Ok(())
    }

    #[cfg(unix)]
    fn create_fake_ffprobe(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let script = root.join("fake-ffprobe");
        let log = root.join("ffprobe-args.log");
        let video_path = root.join("video").join("screen.mp4");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = "-version" ]; then
  echo "ffprobe version fake"
  exit 0
fi
expected='-v error -select_streams v:0 -show_streams -show_frames -show_entries stream=index,codec_type,codec_name,width,height,duration,nb_frames,avg_frame_rate,r_frame_rate,time_base:frame=media_type,key_frame,pts,pts_time,best_effort_timestamp,best_effort_timestamp_time,duration,duration_time,pkt_size,width,height,pict_type -of json {}'
if [ "$*" != "$expected" ]; then
  echo "unexpected ffprobe args: $*" >&2
  exit 64
fi
cat <<'JSON'
{}
JSON
"#,
                log.display(),
                video_path.display(),
                fake_ffprobe_json()
            ),
        )?;
        let mut permissions = fs::metadata(&script)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions)?;
        Ok(script)
    }

    fn fake_ffprobe_json() -> &'static str {
        r#"{
            "streams": [
                {
                    "index": 0,
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 100,
                    "height": 200,
                    "duration": "0.033333",
                    "nb_frames": "1",
                    "avg_frame_rate": "30/1",
                    "r_frame_rate": "30/1",
                    "time_base": "1/90000"
                }
            ],
            "frames": [
                {
                    "media_type": "video",
                    "key_frame": 1,
                    "pts": 0,
                    "pts_time": "0.000000",
                    "duration": 3000,
                    "duration_time": "0.033333",
                    "pkt_size": "123",
                    "width": 100,
                    "height": 200,
                    "pict_type": "I"
                }
            ]
        }"#
    }

    fn test_file_fingerprint(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
        let bytes = fs::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        Ok(json!({
            "algorithm": "sha256",
            "sha256": format!("sha256:{digest:x}"),
        }))
    }

    fn read_json(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    fn sample_layout_result() -> Value {
        json!({
            "keyboard_layout": {
                "available": true,
                "keyboard_view_visible": true,
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

    fn hidden_layout_result() -> Value {
        json!({
            "keyboard_layout": {
                "available": false,
                "keyboard_view_visible": false,
                "unavailable_reason": "keyboard_view_not_shown"
            }
        })
    }

    fn control_character() -> impl Strategy<Value = char> {
        (0_u32..=0x1f).prop_map(|code| char::from_u32(code).unwrap_or('\0'))
    }
}
