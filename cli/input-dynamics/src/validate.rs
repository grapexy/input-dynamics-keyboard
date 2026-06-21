//! JSONL validation for pulled input dynamics logs.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::commands::path_string;
use crate::error::{CliError, CliResult};

const SCHEMA: &str = "input_dynamics_event.v1";

pub(crate) fn validate_logs(path: &Path, run_id: Option<&str>) -> CliResult<Value> {
    let mut files = Vec::new();
    collect_jsonl_files(path, &mut files)?;

    let mut counts = ValidationCounts::default();

    for file in &files {
        let reader = BufReader::new(File::open(file)?);
        for line_result in reader.lines() {
            let line = line_result?;
            if line.trim().is_empty() {
                continue;
            }
            increment(&mut counts.record_count)?;
            let value: Value = serde_json::from_str(&line)?;
            if !record_matches_run_id(&value, run_id) {
                continue;
            }
            counts.update(&value)?;
        }
    }

    let valid = counts.is_valid();

    Ok(json!({
        "ok": valid,
        "path": path_string(path)?,
        "run_id": run_id,
        "file_count": files.len(),
        "record_count": counts.record_count,
        "selected_record_count": counts.selected_count,
        "session_start_count": counts.session_start_count,
        "session_stop_count": counts.session_stop_count,
        "target_package_seen": counts.target_package_seen,
        "invalid_schema_count": counts.invalid_schema_count,
        "password_record_count": counts.password_record_count,
    }))
}

fn collect_jsonl_files(path: &Path, files: &mut Vec<PathBuf>) -> CliResult<()> {
    let metadata = fs::metadata(path)?;
    if metadata.is_file() {
        if path.extension().and_then(OsStr::to_str) == Some("jsonl") {
            files.push(path.to_path_buf());
        }
        return Ok(());
    }
    if metadata.is_dir() {
        for entry_result in fs::read_dir(path)? {
            let entry = entry_result?;
            collect_jsonl_files(&entry.path(), files)?;
        }
        return Ok(());
    }
    Err(CliError::new(format!(
        "path is neither a file nor a directory: {}",
        path.display()
    )))
}

#[derive(Debug, Default)]
struct ValidationCounts {
    record_count: u64,
    selected_count: u64,
    session_start_count: u64,
    session_stop_count: u64,
    target_package_seen: bool,
    invalid_schema_count: u64,
    password_record_count: u64,
}

impl ValidationCounts {
    fn update(&mut self, value: &Value) -> CliResult<()> {
        increment(&mut self.selected_count)?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
            increment(&mut self.invalid_schema_count)?;
        }
        match value.get("event").and_then(Value::as_str) {
            Some("session_start") => increment(&mut self.session_start_count)?,
            Some("session_stop") => increment(&mut self.session_stop_count)?,
            Some(_) | None => {}
        }
        if value.get("target_package").is_some() {
            self.target_package_seen = true;
        }
        if value.get("password_field").and_then(Value::as_bool) == Some(true) {
            increment(&mut self.password_record_count)?;
        }
        Ok(())
    }

    const fn is_valid(&self) -> bool {
        self.selected_count > 0
            && self.session_start_count > 0
            && self.session_stop_count > 0
            && self.target_package_seen
            && self.invalid_schema_count == 0
            && self.password_record_count == 0
    }
}

fn record_matches_run_id(value: &Value, run_id: Option<&str>) -> bool {
    match run_id {
        Some(expected_run_id) => {
            value.get("external_run_id").and_then(Value::as_str) == Some(expected_run_id)
        }
        None => true,
    }
}

fn increment(value: &mut u64) -> CliResult<()> {
    *value = value
        .checked_add(1)
        .ok_or_else(|| CliError::new("counter overflow"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use proptest::strategy::Strategy;
    use serde_json::json;

    use crate::validate::{ValidationCounts, record_matches_run_id};

    #[test]
    fn validation_counts_accept_normal_session() {
        let mut counts = ValidationCounts::default();
        let start_record = json!({
            "schema": "input_dynamics_event.v1",
            "event": "session_start",
            "target_package": "org.example.input",
            "password_field": false
        });
        let key_record = json!({
            "schema": "input_dynamics_event.v1",
            "event": "key_down",
            "target_package": "org.example.input",
            "password_field": false
        });
        let stop_record = json!({
            "schema": "input_dynamics_event.v1",
            "event": "session_stop",
            "target_package": "org.example.input",
            "password_field": false
        });

        let start_result = counts.update(&start_record);
        let key_result = counts.update(&key_record);
        let stop_result = counts.update(&stop_record);

        assert!(start_result.is_ok(), "session_start should update counts");
        assert!(key_result.is_ok(), "key event should update counts");
        assert!(stop_result.is_ok(), "session_stop should update counts");
        assert!(counts.is_valid(), "normal session should validate");
    }

    #[test]
    fn validation_counts_reject_password_record() {
        let mut counts = ValidationCounts::default();
        let password_record = json!({
            "schema": "input_dynamics_event.v1",
            "event": "key_down",
            "target_package": "org.example.input",
            "password_field": true
        });

        let update_result = counts.update(&password_record);

        assert!(update_result.is_ok(), "password record should be counted");
        assert!(
            !counts.is_valid(),
            "password record should fail validation boundary"
        );
        assert_eq!(
            counts.password_record_count, 1,
            "password record count should increment"
        );
    }

    proptest::proptest! {
        #[test]
        fn run_id_filter_selects_only_matching_external_id(
            run_id in non_empty_text(),
            other_run_id in non_empty_text(),
        ) {
            let record = json!({
                "external_run_id": run_id
            });
            let same_id = record
                .get("external_run_id")
                .and_then(serde_json::Value::as_str);
            let expected_other_match = same_id == Some(other_run_id.as_str());

            proptest::prop_assert!(
                record_matches_run_id(&record, same_id),
                "matching run id should be selected"
            );
            proptest::prop_assert_eq!(
                record_matches_run_id(&record, Some(other_run_id.as_str())),
                expected_other_match,
                "run id filter should be exact"
            );
            proptest::prop_assert!(
                record_matches_run_id(&record, None),
                "missing filter should select all records"
            );
        }
    }

    fn non_empty_text() -> impl Strategy<Value = String> {
        ".{1,64}"
    }
}
