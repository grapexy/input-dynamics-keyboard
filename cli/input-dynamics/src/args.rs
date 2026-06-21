//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::app::{DEFAULT_PACKAGE, DEFAULT_REPO};

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
    /// Hide the currently visible soft keyboard.
    HideKeyboard,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum PressKey {
    /// Delete/backspace.
    Delete,
    /// Enter/action.
    Enter,
    /// Space.
    Space,
}

#[derive(Clone, Copy, Debug, Subcommand)]
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
        #[arg(long, default_value = "manual")]
        input_cadence_policy: String,
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
    },
}
