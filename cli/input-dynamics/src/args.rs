//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
    },
    /// Stop the active logging session.
    Stop,
    /// Read current status.
    Status,
    /// Read keyboard layout status.
    Layout,
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
    /// Tap a key from the current layout by label or code.
    Tap {
        /// Key label to tap.
        #[arg(long, conflicts_with = "code")]
        label: Option<String>,
        /// Key code to tap.
        #[arg(long, conflicts_with = "label")]
        code: Option<i64>,
    },
}
