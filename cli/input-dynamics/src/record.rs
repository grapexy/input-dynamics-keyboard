//! Scientific run capture orchestration.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::app::{App, LOG_DIR};
use crate::commands::{path_string, pull_logs};
use crate::error::{CliError, CliResult};
use crate::process::{FailureMode, spawn_process_to_files};
use crate::validate::validate_logs;

pub(crate) struct RecordConfig {
    pub(crate) run_id: String,
    pub(crate) out: PathBuf,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) input_actor: String,
    pub(crate) input_controller: Option<String>,
    pub(crate) input_cadence_policy: String,
}

struct RecordPaths {
    root: PathBuf,
    ime: PathBuf,
    adb: PathBuf,
    derived: PathBuf,
    manifest: PathBuf,
    validation: PathBuf,
    getevent_raw: PathBuf,
    getevent_stderr: PathBuf,
    ime_pull_tmp: PathBuf,
}

struct GeteventCapture {
    child: Option<Child>,
}

impl Drop for GeteventCapture {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _kill_result = child.kill();
            let _wait_result = child.wait();
        }
    }
}

impl GeteventCapture {
    fn start(app: &App, paths: &RecordPaths) -> CliResult<Self> {
        let args = vec![
            String::from("shell"),
            String::from("getevent"),
            String::from("-lt"),
        ];
        let child = spawn_process_to_files(
            app.adb_program(),
            &args,
            &paths.getevent_raw,
            &paths.getevent_stderr,
        )?;
        Ok(Self { child: Some(child) })
    }

    fn stop(&mut self) -> CliResult<Value> {
        let Some(mut child) = self.child.take() else {
            return Ok(json!({"ok": true, "already_stopped": true}));
        };
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        let status = child.wait()?;
        Ok(json!({
            "ok": true,
            "already_stopped": false,
            "status_code": status.code(),
            "success": status.success(),
        }))
    }
}

pub(crate) fn record_run(app: &App, config: &RecordConfig) -> CliResult<Value> {
    let paths = prepare_paths(&config.out)?;
    let host_start_wall_ms = epoch_millis()?;
    let pre_stop = app.broadcast("STOP", Vec::new())?;
    let clear = app.broadcast("CLEAR_LOGS", Vec::new())?;
    ensure_command_ok(&clear, "clear logs before record")?;
    let start = start_record_session(app, config)?;
    ensure_command_ok(&start, "start record session")?;
    let mut capture = GeteventCapture::start(app, &paths)?;
    let wait = wait_for_stop(config.duration_ms)?;
    let capture_stop = capture.stop()?;
    let stop = app.broadcast("STOP", Vec::new())?;
    let pull = pull_logs(app, &paths.ime_pull_tmp)?;
    let ime_files = stage_ime_logs(&paths.ime_pull_tmp, &paths.ime)?;
    let validation = validate_logs(&paths.ime, Some(config.run_id.as_str()))?;
    write_json_file(&paths.validation, &validation)?;
    let host_stop_wall_ms = epoch_millis()?;
    let manifest_parts = ManifestParts {
        host_start_wall_ms,
        host_stop_wall_ms,
        start,
        wait,
        capture_stop,
        pre_stop,
        clear,
        stop,
        pull,
        validation: validation.clone(),
        ime_files,
    };
    let manifest = manifest_json(app, config, &paths, &manifest_parts)?;
    write_json_file(&paths.manifest, &manifest)?;

    Ok(json!({
        "ok": validation.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "package_name": app.package(),
        "external_run_id": config.run_id.as_str(),
        "output_dir": path_string(&paths.root)?,
        "manifest": path_string(&paths.manifest)?,
        "validation": validation,
    }))
}

fn start_record_session(app: &App, config: &RecordConfig) -> CliResult<Value> {
    let mut extras = vec![
        String::from("--es"),
        String::from("run_id"),
        config.run_id.clone(),
        String::from("--es"),
        String::from("input_actor"),
        config.input_actor.clone(),
        String::from("--es"),
        String::from("input_cadence_policy"),
        config.input_cadence_policy.clone(),
    ];
    if let Some(controller) = config.input_controller.as_ref() {
        extras.extend([
            String::from("--es"),
            String::from("input_controller"),
            controller.clone(),
        ]);
    }
    let enable = app.broadcast("ENABLE", Vec::new())?;
    if enable.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(CliError::new("failed to enable logging before record"));
    }
    app.broadcast("START", extras)
}

fn ensure_command_ok(value: &Value, action: &str) -> CliResult<()> {
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(CliError::new(format!("{action} failed: {value}")))
}

fn wait_for_stop(maybe_duration_ms: Option<u64>) -> CliResult<Value> {
    if let Some(capture_duration_ms) = maybe_duration_ms {
        thread::sleep(Duration::from_millis(capture_duration_ms));
        return Ok(json!({
            "stop_mode": "duration_ms",
            "duration_ms": capture_duration_ms,
        }));
    }
    let mut line = String::new();
    let bytes = io::stdin().read_line(&mut line)?;
    Ok(json!({
        "stop_mode": "stdin_enter",
        "stdin_bytes": bytes,
    }))
}

fn prepare_paths(out: &Path) -> CliResult<RecordPaths> {
    let root = out.to_path_buf();
    let ime = root.join("ime");
    let adb = root.join("adb");
    let derived = root.join("derived");
    let ime_pull_tmp = root.join("ime-pull-tmp");
    fs::create_dir_all(&ime)?;
    fs::create_dir_all(&adb)?;
    fs::create_dir_all(&derived)?;
    if ime_pull_tmp.exists() {
        fs::remove_dir_all(&ime_pull_tmp)?;
    }
    Ok(RecordPaths {
        manifest: root.join("manifest.json"),
        validation: root.join("validation.json"),
        getevent_raw: adb.join("getevent.raw.log"),
        getevent_stderr: adb.join("getevent.stderr.log"),
        root,
        ime,
        adb,
        derived,
        ime_pull_tmp,
    })
}

fn stage_ime_logs(pull_dir: &Path, ime_dir: &Path) -> CliResult<Vec<String>> {
    let pulled_log_dir = pull_dir.join(LOG_DIR);
    let mut staged = Vec::new();
    for entry_result in fs::read_dir(&pulled_log_dir)? {
        let entry = entry_result?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let destination = ime_dir.join(entry.file_name());
        fs::copy(entry.path(), &destination)?;
        staged.push(path_string(&destination)?);
    }
    staged.sort();
    fs::remove_dir_all(pull_dir)?;
    Ok(staged)
}

struct ManifestParts {
    host_start_wall_ms: u64,
    host_stop_wall_ms: u64,
    start: Value,
    wait: Value,
    capture_stop: Value,
    pre_stop: Value,
    clear: Value,
    stop: Value,
    pull: Value,
    validation: Value,
    ime_files: Vec<String>,
}

fn manifest_json(
    app: &App,
    config: &RecordConfig,
    paths: &RecordPaths,
    parts: &ManifestParts,
) -> CliResult<Value> {
    Ok(json!({
        "schema": "input_dynamics_record_manifest.v1",
        "external_run_id": config.run_id.as_str(),
        "package_name": app.package(),
        "host_start_wall_ms": parts.host_start_wall_ms,
        "host_stop_wall_ms": parts.host_stop_wall_ms,
        "output_dir": path_string(&paths.root)?,
        "ime_dir": path_string(&paths.ime)?,
        "adb_dir": path_string(&paths.adb)?,
        "derived_dir": path_string(&paths.derived)?,
        "getevent_raw_log": path_string(&paths.getevent_raw)?,
        "getevent_stderr_log": path_string(&paths.getevent_stderr)?,
        "ime_files": &parts.ime_files,
        "device": device_json(app),
        "commands": {
            "start": parts.start,
            "wait": parts.wait,
            "capture_stop": parts.capture_stop,
            "pre_stop": parts.pre_stop,
            "clear": parts.clear,
            "stop": parts.stop,
            "pull": parts.pull,
            "validation": parts.validation,
        },
    }))
}

fn device_json(app: &App) -> Value {
    json!({
        "serial": adb_value(app, &[String::from("get-serialno")]),
        "model": adb_value(app, &getprop_args("ro.product.model")),
        "manufacturer": adb_value(app, &getprop_args("ro.product.manufacturer")),
        "api_level": adb_value(app, &getprop_args("ro.build.version.sdk")),
        "release": adb_value(app, &getprop_args("ro.build.version.release")),
    })
}

fn adb_value(app: &App, args: &[String]) -> Value {
    match app.adb(args, FailureMode::AllowFailure) {
        Ok(output) => json!({
            "ok": output.status_code == Some(0_i32),
            "value": output.stdout().trim(),
            "process": output.json(),
        }),
        Err(error) => json!({
            "ok": false,
            "error": error.to_string(),
        }),
    }
}

fn getprop_args(name: &str) -> Vec<String> {
    vec![
        String::from("shell"),
        String::from("getprop"),
        String::from(name),
    ]
}

fn write_json_file(path: &Path, value: &Value) -> CliResult<()> {
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, format!("{text}\n"))?;
    Ok(())
}

fn epoch_millis() -> CliResult<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::new(format!("system clock is before Unix epoch: {error}")))?
        .as_millis();
    u64::try_from(millis).map_err(|error| CliError::new(format!("millis overflow: {error}")))
}
