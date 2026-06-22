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
    /// Record a bounded research run with IME logs and ADB touch events.
    Record {
        /// External run id to write into each session record.
        #[arg(long)]
        run_id: String,
        /// Local experiment output directory.
        #[arg(long)]
        out: PathBuf,
        /// Optional capture duration. If omitted, press Enter on stdin to stop.
        #[arg(long)]
        duration_ms: Option<u64>,
        /// Start a persistent uinput controller during the record run.
        #[arg(long)]
        with_input_controller: bool,
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
    /// Manage a stateful input dynamics session.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
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
    /// Run the local input controller process.
    #[command(hide = true)]
    Controller {
        #[command(subcommand)]
        command: ControllerCommand,
    },
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
    /// Derive touch gestures and dismissal inferences.
    Dismissals {
        /// Experiment run directory.
        #[arg(long)]
        run_dir: PathBuf,
        /// Local derivation policy JSON used for analysis thresholds.
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Normalized `adb/getevent.jsonl` path. Defaults under `--run-dir`.
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
    /// Start IME logging and a persistent uinput controller.
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
}

#[derive(Debug, Subcommand)]
pub(crate) enum ControllerCommand {
    /// Run the local input controller server.
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
        /// External run id for runtime provenance.
        #[arg(long)]
        run_id: String,
        /// Serialized input profile runtime configuration.
        #[arg(long)]
        input_profile_runtime_json: Option<String>,
    },
}
