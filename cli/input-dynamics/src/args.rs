//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::app::{DEFAULT_PACKAGE, DEFAULT_REPO};
use crate::ratio::{RatioPpm, SignedRatioPpm};

#[derive(Debug, Parser)]
#[command(
    name = "input-dynamics",
    version,
    about = "Operate Input Dynamics Keyboard over local ADB"
)]
pub(crate) struct Cli {
    /// ADB executable to run.
    #[arg(long, global = true, default_value = "adb")]
    pub(crate) adb: String,

    /// Android package to control.
    #[arg(long, global = true, default_value = DEFAULT_PACKAGE)]
    pub(crate) package: String,

    /// ADB device serial to target. Required when multiple devices are connected.
    #[arg(long, global = true)]
    pub(crate) serial: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    /// Check host tools, device visibility, and IME registration.
    Doctor,
    /// Download or install an APK.
    Install {
        /// APK path to install. If omitted, the latest debug APK is downloaded with gh.
        #[arg(long)]
        apk: Option<PathBuf>,
        /// GitHub repository containing release APK assets.
        #[arg(long, default_value = DEFAULT_REPO)]
        repo: String,
        /// Directory used for downloaded APK assets.
        #[arg(long, default_value = "/tmp/input-dynamics-keyboard")]
        dir: PathBuf,
    },
    /// Enable and select the IME.
    SelectIme,
    /// Enable input dynamics logging.
    EnableLogging,
    /// Disable input dynamics logging.
    DisableLogging,
    /// Start a logging session.
    Start {
        /// External run id to write into each session record.
        #[arg(long)]
        run_id: String,
        /// Session-level input actor provenance.
        #[arg(long, default_value = "human")]
        input_actor: String,
        /// Session-level controller provenance.
        #[arg(long)]
        input_controller: Option<String>,
        /// Session-level cadence provenance.
        #[arg(long, default_value = "manual")]
        input_cadence_policy: String,
    },
    /// Stop the active logging session.
    Stop,
    /// Read current status.
    Status,
    /// Read keyboard layout status.
    Layout {
        /// Wait until the keyboard layout is visible.
        #[arg(long, conflicts_with = "wait_hidden")]
        wait_visible: bool,
        /// Wait until the keyboard layout is hidden.
        #[arg(long, conflicts_with = "wait_visible")]
        wait_hidden: bool,
    },
    /// Observe current device, screen, accessibility, and keyboard state.
    Observe {
        #[command(subcommand)]
        command: ObserveCommand,
    },
    /// Hide the currently visible soft keyboard.
    HideKeyboard {
        /// Dismissal method to attempt.
        #[arg(long, value_enum, default_value = "edge-back")]
        method: HideKeyboardMethod,
        /// Screen edge to use for edge-back dismissal.
        #[arg(long, value_enum, default_value = "right")]
        side: EdgeSide,
        /// Start Y coordinate as a display-height ratio, for example 0.54.
        #[arg(long)]
        start_y_ratio: Option<RatioPpm>,
        /// Inward X travel as a display-width ratio, for example 0.28.
        #[arg(long)]
        distance_ratio: Option<RatioPpm>,
        /// End Y drift as a signed display-height ratio, for example 0.02.
        #[arg(long)]
        end_y_drift_ratio: Option<SignedRatioPpm>,
        /// Edge inset as a display-width ratio, for example 0.002.
        #[arg(long)]
        edge_margin_ratio: Option<RatioPpm>,
        /// Gesture duration.
        #[arg(long)]
        duration_ms: Option<u64>,
        /// Number of generated move intervals.
        #[arg(long)]
        steps: Option<u16>,
    },
    /// List log files.
    ListLogs,
    /// Clear log files when no session is active.
    ClearLogs,
    /// Pull log files to a local directory.
    Pull {
        /// Local output directory.
        #[arg(long)]
        out: PathBuf,
    },
    /// Validate pulled JSONL logs.
    Validate {
        /// JSONL file or directory containing JSONL files.
        path: PathBuf,
        /// Optional external run id to validate.
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Normalize Android getevent captures.
    Getevent {
        #[command(subcommand)]
        command: GeteventCommand,
    },
    /// Derive higher-level analysis artifacts from recorded run data.
    Derive {
        #[command(subcommand)]
        command: DeriveCommand,
    },
    /// Inspect local recording directories.
    Recording {
        #[command(subcommand)]
        command: RecordingCommand,
    },
    /// Transitional bounded foreground capture with IME logs, ADB touch events, and screen video.
    ///
    /// Requires --duration-ms; missing duration returns a structured JSON error.
    Record {
        /// External run id to write into each session record.
        #[arg(long)]
        run_id: String,
        /// Local experiment output directory.
        #[arg(long)]
        out: PathBuf,
        /// Positive capture duration. Required for every transitional record invocation.
        #[arg(long)]
        duration_ms: Option<u64>,
        /// Start a persistent uinput controller during the record run.
        #[arg(long)]
        with_input_controller: bool,
        /// Capture start/end observation evidence bundles.
        #[arg(long)]
        with_evidence: bool,
        /// Use full accessibility hierarchy dumps for --with-evidence.
        #[arg(long, requires = "with_evidence")]
        full_accessibility_evidence: bool,
        /// Disable default screen video capture for diagnostics or CI.
        #[arg(long)]
        no_video: bool,
        /// Session-level input actor provenance.
        #[arg(long, default_value = "human")]
        input_actor: String,
        /// Session-level controller provenance.
        #[arg(long)]
        input_controller: Option<String>,
        /// Session-level cadence provenance.
        #[arg(long, default_value = "manual")]
        input_cadence_policy: String,
    },
    /// Transitional parser for moved controller-only session commands.
    #[command(hide = true)]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Manage keyboard visibility readiness.
    Keyboard {
        #[command(subcommand)]
        command: KeyboardCommand,
    },
    /// Tap a key from the current layout by label or code.
    Tap {
        /// Key label to tap.
        #[arg(long, conflicts_with = "code")]
        label: Option<String>,
        /// Key code to tap.
        #[arg(long, conflicts_with = "label", allow_hyphen_values = true)]
        code: Option<i64>,
    },
    /// Press a common semantic key from the current layout.
    Press {
        /// Semantic key to press.
        key: PressKey,
    },
    /// Type text through the active uinput session using visible layout keys.
    Type {
        /// Text to type. Unsupported characters fail before any key is pressed.
        text: String,
        /// Deterministic delay between typed keys.
        #[arg(long, default_value_t = 40)]
        inter_key_delay_ms: u64,
    },
    /// Send touchscreen input through AOSP uinput.
    Touch {
        #[command(subcommand)]
        command: TouchCommand,
    },
    /// Manage the diagnostic local input controller.
    Controller {
        #[command(subcommand)]
        command: ControllerCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum KeyboardCommand {
    /// Ensure the focused non-password editable field has a visible keyboard.
    EnsureVisible,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ObserveCommand {
    /// Dump the current accessibility hierarchy.
    Accessibility {
        /// Local XML output path. If omitted, XML is included in JSON output.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Request the full hierarchy instead of Android's compressed dump.
        #[arg(long)]
        full: bool,
    },
    /// Capture a current screenshot.
    Screenshot {
        /// Local PNG output path.
        #[arg(long)]
        out: PathBuf,
    },
    /// Read the keyboard layout through the IME control surface.
    Layout {
        /// Wait until the keyboard layout is visible.
        #[arg(long, conflicts_with = "wait_hidden")]
        wait_visible: bool,
        /// Wait until the keyboard layout is hidden.
        #[arg(long, conflicts_with = "wait_visible")]
        wait_hidden: bool,
    },
    /// Read a compact current state snapshot.
    State {
        /// Include an accessibility hierarchy summary.
        #[arg(long)]
        with_accessibility: bool,
        /// Capture a screenshot to this local PNG path and include its path.
        #[arg(long)]
        screenshot_out: Option<PathBuf>,
        /// Request the full accessibility hierarchy when --with-accessibility is used.
        #[arg(long)]
        full_accessibility: bool,
    },
    /// Capture status, layout, accessibility, and screenshot evidence into a directory.
    All {
        /// Local evidence output directory.
        #[arg(long)]
        out_dir: PathBuf,
        /// Request the full accessibility hierarchy instead of Android's compressed dump.
        #[arg(long)]
        full_accessibility: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum GeteventCommand {
    /// Normalize raw `adb shell getevent -lt` output to JSONL.
    Normalize {
        /// Raw getevent input file.
        #[arg(long)]
        input: PathBuf,
        /// Normalized JSONL output file.
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum DeriveCommand {
    /// Derive per-press summaries from IME JSONL records.
    Presses {
        /// Recording directory created by an input-dynamics capture workflow.
        #[arg(long)]
        recording_dir: PathBuf,
        /// IME session JSONL path. Defaults to the single `ime/session-*.jsonl`.
        #[arg(long)]
        ime_jsonl: Option<PathBuf>,
        /// Output path for derived press summaries. Defaults under `--recording-dir`.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Derive a run-level summary from press summaries.
    Summary {
        /// Recording directory created by an input-dynamics capture workflow.
        #[arg(long)]
        recording_dir: PathBuf,
        /// Derived press summary JSONL path. Defaults under `--recording-dir`.
        #[arg(long)]
        press_summaries_jsonl: Option<PathBuf>,
        /// Output path for run summary JSON. Defaults under `--recording-dir`.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Derive touch gestures and dismissal inferences.
    Dismissals {
        /// Recording directory created by an input-dynamics capture workflow.
        #[arg(long)]
        recording_dir: PathBuf,
        /// Local derivation policy JSON used for analysis thresholds.
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Normalized `adb/getevent.jsonl` path. Defaults under `--recording-dir`.
        #[arg(long)]
        getevent_jsonl: Option<PathBuf>,
        /// IME session JSONL path. Defaults to the single `ime/session-*.jsonl`.
        #[arg(long)]
        ime_jsonl: Option<PathBuf>,
        /// Output path for derived touch gestures.
        #[arg(long)]
        touch_gestures_output: Option<PathBuf>,
        /// Output path for dismissal inferences.
        #[arg(long)]
        dismissals_output: Option<PathBuf>,
    },
    /// Derive a cross-source recording timeline.
    Timeline {
        /// Recording directory created by an input-dynamics capture workflow.
        #[arg(long)]
        recording_dir: PathBuf,
        /// IME session JSONL path. Defaults to the single `ime/session-*.jsonl`.
        #[arg(long)]
        ime_jsonl: Option<PathBuf>,
        /// Derived touch gesture JSONL path. Defaults under `--recording-dir`.
        #[arg(long)]
        touch_gestures_jsonl: Option<PathBuf>,
        /// Derived dismissal inference JSONL path. Defaults under `--recording-dir`.
        #[arg(long)]
        dismissals_jsonl: Option<PathBuf>,
        /// Timeline output directory. Defaults to `derived/timeline`.
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    /// Derive encoded video frame metadata and event-frame windows.
    VideoMap {
        /// Recording directory created by an input-dynamics capture workflow.
        #[arg(long)]
        recording_dir: PathBuf,
        /// Video-map output directory. Defaults to `derived/video_map`.
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// ffprobe executable to run.
        #[arg(long, default_value = "ffprobe")]
        ffprobe: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RecordingCommand {
    /// Inspect a recording directory without modifying it.
    Inspect {
        /// Recording directory created by an input-dynamics capture workflow.
        #[arg(long)]
        dir: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum PressKey {
    /// Delete/backspace.
    Delete,
    /// Enter/action.
    Enter,
    /// Space.
    Space,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum HideKeyboardMethod {
    /// Use a touchscreen edge-back gesture.
    EdgeBack,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum EdgeSide {
    /// Start at the left screen edge and move inward.
    Left,
    /// Start at the right screen edge and move inward.
    Right,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TouchCommand {
    /// Check AOSP uinput availability and physical touchscreen profile.
    Doctor,
    /// Tap absolute screen coordinates through AOSP uinput.
    Tap {
        /// Absolute screen X coordinate.
        #[arg(long)]
        x: i32,
        /// Absolute screen Y coordinate.
        #[arg(long)]
        y: i32,
        /// Touch hold duration.
        #[arg(long, default_value_t = 70)]
        hold_ms: u64,
    },
    /// Swipe absolute screen coordinates through the active uinput session.
    Swipe {
        /// Start X coordinate.
        #[arg(long)]
        from_x: i32,
        /// Start Y coordinate.
        #[arg(long)]
        from_y: i32,
        /// End X coordinate.
        #[arg(long)]
        to_x: i32,
        /// End Y coordinate.
        #[arg(long)]
        to_y: i32,
        /// Gesture duration.
        #[arg(long, default_value_t = 100)]
        duration_ms: u64,
        /// Number of generated move intervals.
        #[arg(long, default_value_t = 12)]
        steps: u16,
    },
    /// Send an absolute point path through the active uinput session.
    Path {
        /// JSON array of points, either [{"x":1,"y":2}] or [[1,2]].
        #[arg(long, conflicts_with = "points_file")]
        points_json: Option<String>,
        /// File containing a JSON point array.
        #[arg(long, conflicts_with = "points_json")]
        points_file: Option<PathBuf>,
        /// Gesture duration.
        #[arg(long, default_value_t = 100)]
        duration_ms: u64,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SessionCommand {
    /// Moved diagnostic controller start parser; use `controller start`.
    #[command(hide = true)]
    Start {
        /// External run id to preserve in the moved-command recovery argv.
        #[arg(long)]
        run_id: String,
        /// Input actor provenance to preserve in the moved-command recovery argv.
        #[arg(long, default_value = "agent_adb")]
        input_actor: String,
        /// Controller provenance to preserve in the moved-command recovery argv.
        #[arg(long, default_value = "input-dynamics-cli")]
        input_controller: String,
        /// Cadence provenance to preserve in the moved-command recovery argv.
        #[arg(long, default_value = "input_profile")]
        input_cadence_policy: String,
        /// Local input profile JSON file to preserve in the moved-command recovery argv.
        #[arg(long)]
        input_profile: Option<PathBuf>,
        /// Explicit input profile seed for reproducible sampled input.
        #[arg(long)]
        input_profile_seed: Option<u64>,
    },
    /// Moved diagnostic controller status parser; use `controller status`.
    #[command(hide = true)]
    Status,
    /// Moved diagnostic controller stop parser; use `controller stop`.
    #[command(hide = true)]
    Stop,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ControllerCommand {
    /// Start IME logging and a persistent diagnostic uinput controller.
    Start {
        /// External run id to write into each session record.
        #[arg(long)]
        run_id: String,
        /// Session-level input actor provenance.
        #[arg(long, default_value = "agent_adb")]
        input_actor: String,
        /// Session-level controller provenance.
        #[arg(long, default_value = "input-dynamics-cli")]
        input_controller: String,
        /// Session-level cadence provenance.
        #[arg(long, default_value = "input_profile")]
        input_cadence_policy: String,
        /// Local input profile JSON file. If omitted, agent-controlled sessions use the bundled baseline profile.
        #[arg(long)]
        input_profile: Option<PathBuf>,
        /// Explicit input profile seed for reproducible sampled input.
        #[arg(long)]
        input_profile_seed: Option<u64>,
    },
    /// Read IME and local input-controller status.
    Status,
    /// Stop the persistent input controller and IME logging.
    Stop,
    /// Run the local input controller server.
    #[command(hide = true)]
    Run {
        /// Unix socket path for local command IPC.
        #[arg(long)]
        socket: PathBuf,
        /// Runtime state JSON path.
        #[arg(long)]
        state: PathBuf,
        /// ADB uinput stdout log path.
        #[arg(long)]
        uinput_stdout: PathBuf,
        /// ADB uinput stderr log path.
        #[arg(long)]
        uinput_stderr: PathBuf,
        /// Unified controller event journal path.
        #[arg(long)]
        events: PathBuf,
        /// Final controller state snapshot path.
        #[arg(long)]
        final_state: PathBuf,
        /// Controller invocation id for diagnostics.
        #[arg(long)]
        controller_invocation_id: String,
        /// External run id for runtime provenance.
        #[arg(long)]
        run_id: String,
        /// Serialized input profile runtime configuration.
        #[arg(long)]
        input_profile_runtime_json: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, Commands, ControllerCommand, SessionCommand};

    #[test]
    fn controller_lifecycle_commands_parse() {
        let start = Cli::try_parse_from([
            "input-dynamics",
            "controller",
            "start",
            "--run-id",
            "run-test",
        ]);
        let status = Cli::try_parse_from(["input-dynamics", "controller", "status"]);
        let stop = Cli::try_parse_from(["input-dynamics", "controller", "stop"]);

        assert!(
            matches!(
                start,
                Ok(Cli {
                    command: Commands::Controller {
                        command: ControllerCommand::Start { .. }
                    },
                    ..
                })
            ),
            "controller start should parse"
        );
        assert!(
            matches!(
                status,
                Ok(Cli {
                    command: Commands::Controller {
                        command: ControllerCommand::Status
                    },
                    ..
                })
            ),
            "controller status should parse"
        );
        assert!(
            matches!(
                stop,
                Ok(Cli {
                    command: Commands::Controller {
                        command: ControllerCommand::Stop
                    },
                    ..
                })
            ),
            "controller stop should parse"
        );
    }

    #[test]
    fn internal_controller_run_still_parses() {
        let parsed = Cli::try_parse_from([
            "input-dynamics",
            "controller",
            "run",
            "--socket",
            "/tmp/input-dynamics.sock",
            "--state",
            "/tmp/input-dynamics-state.json",
            "--uinput-stdout",
            "/tmp/uinput.stdout.log",
            "--uinput-stderr",
            "/tmp/uinput.stderr.log",
            "--events",
            "/tmp/controller.events.jsonl",
            "--final-state",
            "/tmp/final-state.json",
            "--controller-invocation-id",
            "controller-test",
            "--run-id",
            "run-test",
        ]);

        assert!(
            matches!(
                parsed,
                Ok(Cli {
                    command: Commands::Controller {
                        command: ControllerCommand::Run { .. }
                    },
                    ..
                })
            ),
            "hidden internal controller run should remain parseable"
        );
    }

    #[test]
    fn old_session_commands_still_parse_for_structured_migration_errors() {
        let start = Cli::try_parse_from([
            "input-dynamics",
            "session",
            "start",
            "--run-id",
            "run-test",
            "--input-profile",
            "profiles/custom.json",
            "--input-profile-seed",
            "7",
        ]);
        let status = Cli::try_parse_from(["input-dynamics", "session", "status"]);
        let stop = Cli::try_parse_from(["input-dynamics", "session", "stop"]);

        assert!(
            matches!(
                start,
                Ok(Cli {
                    command: Commands::Session {
                        command: SessionCommand::Start { .. }
                    },
                    ..
                })
            ),
            "old session start should parse so command handling can return JSON"
        );
        assert!(
            matches!(
                status,
                Ok(Cli {
                    command: Commands::Session {
                        command: SessionCommand::Status
                    },
                    ..
                })
            ),
            "old session status should parse so command handling can return JSON"
        );
        assert!(
            matches!(
                stop,
                Ok(Cli {
                    command: Commands::Session {
                        command: SessionCommand::Stop
                    },
                    ..
                })
            ),
            "old session stop should parse so command handling can return JSON"
        );
    }

    #[test]
    fn controller_help_is_visible_but_internal_run_is_hidden() {
        let top_help = Cli::command().render_help().to_string();
        let controller_help = Cli::command()
            .find_subcommand_mut("controller")
            .map(|command| command.render_help().to_string())
            .unwrap_or_default();

        assert!(
            top_help.contains("controller"),
            "top-level help should expose diagnostic controller namespace"
        );
        assert!(
            controller_help.contains("start")
                && controller_help.contains("status")
                && controller_help.contains("stop"),
            "controller help should expose start/status/stop"
        );
        assert!(
            !controller_help.contains("run"),
            "controller help should hide internal run server command"
        );
    }

    #[test]
    fn session_help_does_not_promote_old_controller_lifecycle() {
        let session_help = Cli::command()
            .find_subcommand_mut("session")
            .map(|command| command.render_help().to_string())
            .unwrap_or_default();
        let direct_start_help = session_subcommand_help("start");
        let direct_status_help = session_subcommand_help("status");
        let direct_stop_help = session_subcommand_help("stop");

        assert!(
            !session_help.contains("Start IME logging")
                && !session_help.contains("Read IME and local input-controller status")
                && !session_help.contains("Stop the persistent input controller"),
            "session help should not present old controller-only lifecycle as normal"
        );
        for help in [direct_start_help, direct_status_help, direct_stop_help] {
            assert!(
                help.contains("Moved diagnostic controller"),
                "direct hidden session subcommand help should explain migration: {help}"
            );
            assert!(
                !help.contains("Start IME logging")
                    && !help.contains("Read IME and local input-controller status")
                    && !help.contains("Stop the persistent input controller"),
                "direct hidden session subcommand help should not promote old lifecycle: {help}"
            );
        }
    }

    fn session_subcommand_help(name: &str) -> String {
        let mut command = Cli::command();
        command
            .find_subcommand_mut("session")
            .and_then(|session| session.find_subcommand_mut(name))
            .map(|subcommand| subcommand.render_help().to_string())
            .unwrap_or_default()
    }
}
