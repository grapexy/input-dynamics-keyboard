//! Path helpers for capture-session runtime and run-local state.

#![cfg_attr(not(test), allow(dead_code))]

use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSessionPaths {
    pub(crate) lock: PathBuf,
    pub(crate) current: PathBuf,
    pub(crate) runs_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunSessionPaths {
    pub(crate) session_dir: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) finalization: PathBuf,
    pub(crate) lock_snapshot: PathBuf,
}

impl RuntimeSessionPaths {
    pub(crate) fn from_base_dir(base_dir: &Path, package_name: &str, device_serial: &str) -> Self {
        let prefix = capture_session_prefix(package_name, device_serial);
        Self {
            lock: runtime_file(base_dir, &prefix, "capture-session.lock.json"),
            current: runtime_file(base_dir, &prefix, "capture-session.current.json"),
            runs_dir: base_dir.join(format!("{prefix}.capture-session.runs")),
        }
    }
}

impl RunSessionPaths {
    pub(crate) fn from_run_dir(run_dir: &Path) -> Self {
        let session_dir = run_dir.join("session");
        Self {
            state: session_dir.join("state.json"),
            finalization: session_dir.join("finalization.json"),
            lock_snapshot: session_dir.join("lock.snapshot.json"),
            session_dir,
        }
    }
}

pub(crate) fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        String::from("default")
    } else {
        sanitized
    }
}

fn capture_session_prefix(package_name: &str, device_serial: &str) -> String {
    format!(
        "{}.{}",
        path_key_component(package_name),
        path_key_component(device_serial)
    )
}

fn runtime_file(base_dir: &Path, prefix: &str, suffix: &str) -> PathBuf {
    base_dir.join(format!("{prefix}.{suffix}"))
}

fn path_key_component(value: &str) -> String {
    format!("{}-{}", sanitize_path_component(value), short_hash(value))
}

fn short_hash(value: &str) -> String {
    let mut output = String::new();
    for byte in Sha256::digest(value.as_bytes()).iter().take(6) {
        match write!(&mut output, "{byte:02x}") {
            Ok(()) | Err(_) => {}
        }
    }
    output
}
