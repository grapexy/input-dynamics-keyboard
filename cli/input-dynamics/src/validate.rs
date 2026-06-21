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
        "session_start_provenance_count": counts.session_start_provenance_count,
        "target_package_seen": counts.target_package_seen,
        "invalid_schema_count": counts.invalid_schema_count,
        "missing_required_field_count": counts.missing_required_field_count,
        "invalid_required_field_count": counts.invalid_required_field_count,
        "invalid_provenance_count": counts.invalid_provenance_count,
        "touch_sequence_record_count": counts.touch_sequence_record_count,
        "missing_press_id_count": counts.missing_press_id_count,
        "missing_gesture_id_count": counts.missing_gesture_id_count,
        "session_id_mismatch_count": counts.session_id_mismatch_count,
        "event_order_violation_count": counts.event_order_violation_count,
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
    session_start_provenance_count: u64,
    target_package_seen: bool,
    invalid_schema_count: u64,
    missing_required_field_count: u64,
    invalid_required_field_count: u64,
    invalid_provenance_count: u64,
    touch_sequence_record_count: u64,
    missing_press_id_count: u64,
    missing_gesture_id_count: u64,
    session_id_mismatch_count: u64,
    event_order_violation_count: u64,
    password_record_count: u64,
    first_session_id: Option<String>,
    seen_session_start: bool,
    seen_session_stop: bool,
}

impl ValidationCounts {
    fn update(&mut self, value: &Value) -> CliResult<()> {
        increment(&mut self.selected_count)?;
        self.validate_required_fields(value)?;
        self.validate_touch_sequence_ids(value)?;
        self.validate_session_id(value)?;
        self.validate_event_order(value)?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
            increment(&mut self.invalid_schema_count)?;
        }
        match value.get("event").and_then(Value::as_str) {
            Some("session_start") => {
                increment(&mut self.session_start_count)?;
                self.validate_session_start_provenance(value)?;
            }
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
            && self.session_start_provenance_count == self.session_start_count
            && self.target_package_seen
            && self.invalid_schema_count == 0
            && self.missing_required_field_count == 0
            && self.invalid_required_field_count == 0
            && self.invalid_provenance_count == 0
            && self.missing_press_id_count == 0
            && self.missing_gesture_id_count == 0
            && self.session_id_mismatch_count == 0
            && self.event_order_violation_count == 0
            && self.password_record_count == 0
    }

    fn validate_required_fields(&mut self, value: &Value) -> CliResult<()> {
        self.require_non_empty_string(value, "session_id")?;
        self.require_non_empty_string(value, "event")?;
        self.require_non_negative_integer(value, "t_wall_ms")?;
        self.require_non_negative_integer(value, "t_uptime_ms")?;

        if let Some(target_package) = value.get("target_package") {
            if !is_non_empty_string(target_package) {
                increment(&mut self.invalid_required_field_count)?;
            }
        }

        if let Some(password_field) = value.get("password_field") {
            if !password_field.is_boolean() {
                increment(&mut self.invalid_required_field_count)?;
            }
        }

        Ok(())
    }

    fn validate_touch_sequence_ids(&mut self, value: &Value) -> CliResult<()> {
        let Some(event) = value.get("event").and_then(Value::as_str) else {
            return Ok(());
        };
        if !requires_touch_sequence_id(event) {
            return Ok(());
        }

        increment(&mut self.touch_sequence_record_count)?;
        if !value.get("press_id").is_some_and(is_non_negative_integer) {
            increment(&mut self.missing_press_id_count)?;
        }
        if !value.get("gesture_id").is_some_and(is_non_negative_integer) {
            increment(&mut self.missing_gesture_id_count)?;
        }
        Ok(())
    }

    fn validate_session_start_provenance(&mut self, value: &Value) -> CliResult<()> {
        let actor_valid = value.get("input_actor").is_some_and(is_non_empty_string);
        let cadence_valid = value
            .get("input_cadence_policy")
            .is_some_and(is_non_empty_string);
        let controller_valid = value
            .get("input_controller")
            .is_some_and(|controller| controller.is_null() || is_non_empty_string(controller));

        if actor_valid && cadence_valid && controller_valid {
            increment(&mut self.session_start_provenance_count)?;
        } else {
            increment(&mut self.invalid_provenance_count)?;
        }
        Ok(())
    }

    fn validate_session_id(&mut self, value: &Value) -> CliResult<()> {
        let Some(session_id) = value.get("session_id").and_then(Value::as_str) else {
            return Ok(());
        };
        if let Some(first_session_id) = self.first_session_id.as_deref() {
            if first_session_id != session_id {
                increment(&mut self.session_id_mismatch_count)?;
            }
        } else {
            self.first_session_id = Some(String::from(session_id));
        }
        Ok(())
    }

    fn validate_event_order(&mut self, value: &Value) -> CliResult<()> {
        match value.get("event").and_then(Value::as_str) {
            Some("session_start") => {
                if self.seen_session_start || self.seen_session_stop {
                    increment(&mut self.event_order_violation_count)?;
                }
                self.seen_session_start = true;
            }
            Some("session_stop") => {
                if !self.seen_session_start || self.seen_session_stop {
                    increment(&mut self.event_order_violation_count)?;
                }
                self.seen_session_stop = true;
            }
            Some(_) if !self.seen_session_start || self.seen_session_stop => {
                increment(&mut self.event_order_violation_count)?;
            }
            Some(_) | None => {}
        }
        Ok(())
    }

    fn require_non_empty_string(&mut self, value: &Value, field: &str) -> CliResult<()> {
        match value.get(field) {
            Some(field_value) => {
                if !is_non_empty_string(field_value) {
                    increment(&mut self.invalid_required_field_count)?;
                }
            }
            None => increment(&mut self.missing_required_field_count)?,
        }
        Ok(())
    }

    fn require_non_negative_integer(&mut self, value: &Value, field: &str) -> CliResult<()> {
        match value.get(field) {
            Some(field_value) => {
                let valid = field_value.as_u64().is_some()
                    || field_value.as_i64().is_some_and(|number| number >= 0);
                if !valid {
                    increment(&mut self.invalid_required_field_count)?;
                }
            }
            None => increment(&mut self.missing_required_field_count)?,
        }
        Ok(())
    }
}

fn is_non_empty_string(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

fn requires_touch_sequence_id(event: &str) -> bool {
    matches!(
        event,
        "pointer_sample"
            | "key_down"
            | "key_up"
            | "key_commit"
            | "key_repeat"
            | "key_long_press"
            | "key_cancel"
    )
}

fn is_non_negative_integer(value: &Value) -> bool {
    value.as_u64().is_some() || value.as_i64().is_some_and(|number| number >= 0)
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
            "session_id": "session-1",
            "event": "session_start",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 1_u64,
            "input_actor": "human",
            "input_controller": null,
            "input_cadence_policy": "manual",
            "target_package": "org.example.input",
            "password_field": false
        });
        let key_record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "key_down",
            "t_wall_ms": 2_u64,
            "t_uptime_ms": 2_u64,
            "press_id": 1_u64,
            "gesture_id": 1_u64,
            "target_package": "org.example.input",
            "password_field": false
        });
        let stop_record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "session_stop",
            "t_wall_ms": 3_u64,
            "t_uptime_ms": 3_u64,
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
    fn validation_counts_reject_session_start_without_provenance() {
        let mut counts = ValidationCounts::default();
        let start_record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "session_start",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 1_u64,
            "target_package": "org.example.input",
            "password_field": false
        });

        let update_result = counts.update(&start_record);

        assert!(
            update_result.is_ok(),
            "session_start should update counts even when provenance is missing"
        );
        assert!(
            !counts.is_valid(),
            "missing provenance should fail validation"
        );
        assert_eq!(
            counts.invalid_provenance_count, 1,
            "invalid provenance count should increment"
        );
    }

    #[test]
    fn validation_counts_reject_touch_record_without_press_ids() {
        let mut counts = ValidationCounts::default();
        let record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "pointer_sample",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 1_u64,
            "target_package": "org.example.input",
            "password_field": false
        });

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "touch sequence record should update counts"
        );
        assert!(
            !counts.is_valid(),
            "missing touch sequence ids should fail validation"
        );
        assert_eq!(
            counts.touch_sequence_record_count, 1,
            "touch sequence record count should increment"
        );
        assert_eq!(
            counts.missing_press_id_count, 1,
            "missing press id count should increment"
        );
        assert_eq!(
            counts.missing_gesture_id_count, 1,
            "missing gesture id count should increment"
        );
    }

    #[test]
    fn validation_counts_reject_password_record() {
        let mut counts = ValidationCounts::default();
        let password_record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "key_down",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 1_u64,
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

    #[test]
    fn validation_counts_reject_malformed_minimal_record() {
        let mut counts = ValidationCounts::default();
        let malformed_record = json!({
            "schema": "input_dynamics_event.v1",
            "event": "key_down",
            "target_package": "org.example.input",
            "password_field": false
        });

        let update_result = counts.update(&malformed_record);

        assert!(update_result.is_ok(), "malformed record should be counted");
        assert!(
            !counts.is_valid(),
            "missing required fields should fail validation"
        );
        assert_eq!(
            counts.missing_required_field_count, 3,
            "session_id and timestamps should be required"
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
