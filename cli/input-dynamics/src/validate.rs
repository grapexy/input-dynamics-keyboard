//! JSONL validation for pulled input dynamics logs.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use input_dynamics_analysis::clock::{
    ClockDomain, TimestampPrecision, TimestampSource, millis_to_nanos,
};
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
    let failure_reasons = counts.failure_reasons();
    let diagnostic_reasons = counts.diagnostic_reasons();

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
        "input_scope_record_count": counts.input_scope_record_count,
        "field_enter_count": counts.field_enter_count,
        "key_record_count": counts.key_record_count,
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
        "invalid_timestamp_metadata_count": counts.invalid_timestamp_metadata_count,
        "failure_reasons": failure_reasons,
        "diagnostic_reasons": diagnostic_reasons,
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
    input_scope_record_count: u64,
    field_enter_count: u64,
    key_record_count: u64,
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
    invalid_timestamp_metadata_count: u64,
    first_session_id: Option<String>,
    seen_session_start: bool,
    seen_session_stop: bool,
}

impl ValidationCounts {
    fn update(&mut self, value: &Value) -> CliResult<()> {
        increment(&mut self.selected_count)?;
        self.validate_required_fields(value)?;
        self.validate_timestamp_metadata(value)?;
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
            Some("field_enter") => increment(&mut self.field_enter_count)?,
            Some(event) if event.starts_with("key_") => increment(&mut self.key_record_count)?,
            Some("session_stop") => increment(&mut self.session_stop_count)?,
            Some(_) | None => {}
        }
        if value.get("target_package").is_some() {
            increment(&mut self.input_scope_record_count)?;
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
            && self.invalid_timestamp_metadata_count == 0
    }

    fn failure_reasons(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.selected_count == 0 {
            reasons.push("no_matching_records");
        }
        if self.session_start_count == 0 {
            reasons.push("missing_session_start");
        }
        if self.session_stop_count == 0 {
            reasons.push("missing_session_stop");
        }
        if self.session_start_provenance_count != self.session_start_count {
            reasons.push("invalid_session_start_provenance");
        }
        if !self.target_package_seen {
            reasons.push("no_input_scope_records");
        }
        if self.invalid_schema_count > 0 {
            reasons.push("invalid_schema_records");
        }
        if self.missing_required_field_count > 0 {
            reasons.push("missing_required_fields");
        }
        if self.invalid_required_field_count > 0 {
            reasons.push("invalid_required_fields");
        }
        if self.invalid_provenance_count > 0 {
            reasons.push("invalid_provenance_fields");
        }
        if self.missing_press_id_count > 0 {
            reasons.push("missing_press_id");
        }
        if self.missing_gesture_id_count > 0 {
            reasons.push("missing_gesture_id");
        }
        if self.session_id_mismatch_count > 0 {
            reasons.push("session_id_mismatch");
        }
        if self.event_order_violation_count > 0 {
            reasons.push("event_order_violation");
        }
        if self.password_record_count > 0 {
            reasons.push("password_field_records");
        }
        if self.invalid_timestamp_metadata_count > 0 {
            reasons.push("invalid_timestamp_metadata");
        }
        reasons
    }

    fn diagnostic_reasons(&self) -> Vec<&'static str> {
        let mut reasons = self.failure_reasons();
        let lifecycle_count = self
            .session_start_count
            .checked_add(self.session_stop_count);
        if self.selected_count > 0 && lifecycle_count == Some(self.selected_count) {
            reasons.push("only_session_lifecycle_records");
        }
        if self.field_enter_count == 0 {
            reasons.push("no_field_enter_records");
        }
        if self.key_record_count == 0 {
            reasons.push("no_key_records");
        }
        if self.touch_sequence_record_count == 0 {
            reasons.push("no_touch_sequence_records");
        }
        reasons
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

    fn validate_timestamp_metadata(&mut self, value: &Value) -> CliResult<()> {
        let has_timestamp_metadata = timestamp_role_names()
            .iter()
            .any(|role_name| value.get(role_name).is_some())
            || value.get("t_capture_elapsed_realtime_ns").is_some();

        for role_name in timestamp_role_names() {
            if let Some(role) = value.get(role_name) {
                self.validate_timestamp_role(value, role_name, role)?;
            }
        }

        if has_timestamp_metadata {
            self.validate_required_timestamp_roles(value)?;
            self.validate_capture_write_order(value)?;
        }
        Ok(())
    }

    fn validate_timestamp_role(
        &mut self,
        record: &Value,
        role_name: &str,
        role: &Value,
    ) -> CliResult<()> {
        let mut valid = role.is_object();
        let clock_domain = required_string(role, "clock_domain");
        let timestamp_source = required_string(role, "timestamp_source");
        let timestamp_precision = required_string(role, "timestamp_precision");
        let field = required_string(role, "field");
        let expectation = timestamp_role_expectation(record, role_name);

        valid &= clock_domain.is_some_and(|text| text.parse::<ClockDomain>().is_ok());
        valid &= timestamp_source.is_some_and(|text| text.parse::<TimestampSource>().is_ok());
        valid &= timestamp_precision.is_some_and(|text| text.parse::<TimestampPrecision>().is_ok());
        valid &= expectation.is_some_and(|expected| timestamp_role_matches(role, expected));

        if let Some(field_name) = field {
            valid &= record.get(field_name).and_then(non_negative_i64).is_some();
        } else {
            valid = false;
        }

        valid &= Self::validate_timestamp_companion(record, role, field);

        if !valid {
            increment(&mut self.invalid_timestamp_metadata_count)?;
        }
        Ok(())
    }

    fn validate_timestamp_companion(record: &Value, role: &Value, field: Option<&str>) -> bool {
        let Some(field_ns) = role.get("field_ns") else {
            return role.get("field_ns_precision").is_none();
        };
        let Some(field_ns_name) = field_ns.as_str().filter(|text| !text.is_empty()) else {
            return false;
        };
        let Some(field_ns_precision) = required_string(role, "field_ns_precision") else {
            return false;
        };
        let Ok(precision) = field_ns_precision.parse::<TimestampPrecision>() else {
            return false;
        };
        let Some(ns_value) = record.get(field_ns_name).and_then(non_negative_i64) else {
            return false;
        };
        if precision != TimestampPrecision::MillisecondsConvertedToNanoseconds {
            return true;
        }
        let Some(field_name) = field else {
            return false;
        };
        let Some(ms_value) = record.get(field_name).and_then(non_negative_i64) else {
            return false;
        };
        millis_to_nanos(ms_value) == Some(ns_value)
    }

    fn validate_required_timestamp_roles(&mut self, value: &Value) -> CliResult<()> {
        let mut valid = value.get("capture_time").is_some();
        valid &= value.get("write_time").is_some();
        match value.get("event").and_then(Value::as_str) {
            Some("pointer_sample") => {
                valid &= value.get("event_time").is_some();
                valid &= value.get("down_time").is_some();
            }
            Some("system_back_event") => {
                valid &= value.get("event_time").is_some();
            }
            Some(event) if requires_touch_sequence_id(event) && event.starts_with("key_") => {
                valid &= value.get("event_time").is_some();
            }
            Some(_) | None => {}
        }
        if !valid {
            increment(&mut self.invalid_timestamp_metadata_count)?;
        }
        Ok(())
    }

    fn validate_capture_write_order(&mut self, value: &Value) -> CliResult<()> {
        let capture_time_ns = value
            .get("t_capture_elapsed_realtime_ns")
            .and_then(non_negative_i64);
        let write_time_ns = value
            .get("t_elapsed_realtime_ns")
            .and_then(non_negative_i64);
        if let (Some(capture_ns), Some(write_ns)) = (capture_time_ns, write_time_ns) {
            if capture_ns > write_ns {
                increment(&mut self.invalid_timestamp_metadata_count)?;
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

#[derive(Clone, Copy)]
struct TimestampRoleExpectation {
    clock_domain: ClockDomain,
    timestamp_source: TimestampSource,
    timestamp_precision: TimestampPrecision,
    field: &'static str,
    field_ns: Option<&'static str>,
    field_ns_precision: Option<TimestampPrecision>,
}

const fn timestamp_role_names() -> [&'static str; 4] {
    ["event_time", "down_time", "capture_time", "write_time"]
}

fn timestamp_role_expectation(record: &Value, role_name: &str) -> Option<TimestampRoleExpectation> {
    match role_name {
        "event_time" => event_time_expectation(record),
        "down_time" => down_time_expectation(record),
        "capture_time" => Some(TimestampRoleExpectation {
            clock_domain: ClockDomain::DeviceElapsedRealtimeNs,
            timestamp_source: TimestampSource::CallbackCapture,
            timestamp_precision: TimestampPrecision::Nanoseconds,
            field: "t_capture_elapsed_realtime_ns",
            field_ns: None,
            field_ns_precision: None,
        }),
        "write_time" => Some(TimestampRoleExpectation {
            clock_domain: ClockDomain::DeviceElapsedRealtimeNs,
            timestamp_source: TimestampSource::Writer,
            timestamp_precision: TimestampPrecision::Nanoseconds,
            field: "t_elapsed_realtime_ns",
            field_ns: None,
            field_ns_precision: None,
        }),
        _ => None,
    }
}

fn event_time_expectation(record: &Value) -> Option<TimestampRoleExpectation> {
    let event = record.get("event").and_then(Value::as_str)?;
    let timestamp_source = match event {
        "system_back_event" => TimestampSource::KeyEvent,
        "pointer_sample" | "key_down" | "key_up" | "key_commit" => TimestampSource::MotionEvent,
        "key_repeat" | "key_long_press" | "key_cancel" => TimestampSource::SyntheticHandler,
        _ => return None,
    };
    Some(android_uptime_millisecond_expectation(
        timestamp_source,
        "t_event_uptime_ms",
        "t_event_uptime_ns",
    ))
}

fn down_time_expectation(record: &Value) -> Option<TimestampRoleExpectation> {
    if record.get("event").and_then(Value::as_str) != Some("pointer_sample") {
        return None;
    }
    Some(android_uptime_millisecond_expectation(
        TimestampSource::MotionEvent,
        "t_down_uptime_ms",
        "t_down_uptime_ns",
    ))
}

const fn android_uptime_millisecond_expectation(
    timestamp_source: TimestampSource,
    field: &'static str,
    field_ns: &'static str,
) -> TimestampRoleExpectation {
    TimestampRoleExpectation {
        clock_domain: ClockDomain::AndroidUptimeMs,
        timestamp_source,
        timestamp_precision: TimestampPrecision::Milliseconds,
        field,
        field_ns: Some(field_ns),
        field_ns_precision: Some(TimestampPrecision::MillisecondsConvertedToNanoseconds),
    }
}

fn timestamp_role_matches(role: &Value, expectation: TimestampRoleExpectation) -> bool {
    required_string(role, "clock_domain") == Some(expectation.clock_domain.as_str())
        && required_string(role, "timestamp_source") == Some(expectation.timestamp_source.as_str())
        && required_string(role, "timestamp_precision")
            == Some(expectation.timestamp_precision.as_str())
        && required_string(role, "field") == Some(expectation.field)
        && optional_string(role, "field_ns") == expectation.field_ns
        && optional_string(role, "field_ns_precision")
            == expectation
                .field_ns_precision
                .map(TimestampPrecision::as_str)
}

fn required_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn optional_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
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

fn non_negative_i64(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64().filter(|number| *number >= 0) {
        return Some(number);
    }
    value.as_u64().and_then(|number| i64::try_from(number).ok())
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
    fn validation_counts_diagnose_lifecycle_only_records() {
        let mut counts = ValidationCounts::default();
        let start_record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "session_start",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 1_u64,
            "input_actor": "human",
            "input_controller": null,
            "input_cadence_policy": "manual"
        });
        let stop_record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "session_stop",
            "t_wall_ms": 2_u64,
            "t_uptime_ms": 2_u64
        });

        assert!(
            counts.update(&start_record).is_ok(),
            "session_start should update counts"
        );
        assert!(
            counts.update(&stop_record).is_ok(),
            "session_stop should update counts"
        );

        assert!(
            !counts.is_valid(),
            "lifecycle-only session should not validate"
        );
        let diagnostics = counts.diagnostic_reasons();
        assert!(
            diagnostics.contains(&"only_session_lifecycle_records"),
            "diagnostics should explain missing input records"
        );
        assert!(
            diagnostics.contains(&"no_input_scope_records"),
            "diagnostics should explain missing field scope"
        );
        assert!(
            diagnostics.contains(&"no_touch_sequence_records"),
            "diagnostics should explain missing touch/key timing records"
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

    #[test]
    fn validation_counts_accept_canonical_timestamp_metadata() {
        let mut counts = ValidationCounts::default();
        let record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "key_down",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 2_u64,
            "t_elapsed_realtime_ns": 5_u64,
            "t_capture_elapsed_realtime_ns": 4_u64,
            "t_event_uptime_ms": 5_u64,
            "t_event_uptime_ns": 5_000_000_u64,
            "press_id": 1_u64,
            "gesture_id": 1_u64,
            "target_package": "org.example.input",
            "password_field": false,
            "event_time": {
                "clock_domain": "android_uptime_ms",
                "timestamp_source": "motion_event",
                "timestamp_precision": "milliseconds",
                "field": "t_event_uptime_ms",
                "field_ns": "t_event_uptime_ns",
                "field_ns_precision": "milliseconds_converted_to_nanoseconds"
            },
            "capture_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "callback_capture",
                "timestamp_precision": "nanoseconds",
                "field": "t_capture_elapsed_realtime_ns"
            },
            "write_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "writer",
                "timestamp_precision": "nanoseconds",
                "field": "t_elapsed_realtime_ns"
            }
        });

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "canonical metadata should update counts"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 0,
            "canonical timestamp metadata should not be invalid"
        );
    }

    #[test]
    fn validation_counts_reject_unknown_timestamp_vocabulary() {
        let mut counts = ValidationCounts::default();
        let record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "key_down",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 2_u64,
            "t_elapsed_realtime_ns": 5_u64,
            "t_capture_elapsed_realtime_ns": 4_u64,
            "t_event_uptime_ms": 5_u64,
            "t_event_uptime_ns": 5_000_000_u64,
            "press_id": 1_u64,
            "gesture_id": 1_u64,
            "target_package": "org.example.input",
            "password_field": false,
            "event_time": {
                "clock_domain": "android_uptime_ms",
                "timestamp_source": "misspelled_source",
                "timestamp_precision": "milliseconds",
                "field": "t_event_uptime_ms",
                "field_ns": "t_event_uptime_ns",
                "field_ns_precision": "milliseconds_converted_to_nanoseconds"
            },
            "capture_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "callback_capture",
                "timestamp_precision": "nanoseconds",
                "field": "t_capture_elapsed_realtime_ns"
            },
            "write_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "writer",
                "timestamp_precision": "nanoseconds",
                "field": "t_elapsed_realtime_ns"
            }
        });

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "invalid metadata should still be counted"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "unknown timestamp source should fail metadata validation"
        );
        assert!(
            counts
                .failure_reasons()
                .contains(&"invalid_timestamp_metadata"),
            "failure reasons should include timestamp metadata"
        );
    }

    #[test]
    fn validation_counts_reject_mismatched_timestamp_companion() {
        let mut counts = ValidationCounts::default();
        let record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "pointer_sample",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 2_u64,
            "t_elapsed_realtime_ns": 5_u64,
            "t_capture_elapsed_realtime_ns": 4_u64,
            "t_event_uptime_ms": 5_u64,
            "t_event_uptime_ns": 6_u64,
            "t_down_uptime_ms": 4_u64,
            "t_down_uptime_ns": 4_000_000_u64,
            "press_id": 1_u64,
            "gesture_id": 1_u64,
            "target_package": "org.example.input",
            "password_field": false,
            "event_time": {
                "clock_domain": "android_uptime_ms",
                "timestamp_source": "motion_event",
                "timestamp_precision": "milliseconds",
                "field": "t_event_uptime_ms",
                "field_ns": "t_event_uptime_ns",
                "field_ns_precision": "milliseconds_converted_to_nanoseconds"
            },
            "down_time": {
                "clock_domain": "android_uptime_ms",
                "timestamp_source": "motion_event",
                "timestamp_precision": "milliseconds",
                "field": "t_down_uptime_ms",
                "field_ns": "t_down_uptime_ns",
                "field_ns_precision": "milliseconds_converted_to_nanoseconds"
            },
            "capture_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "callback_capture",
                "timestamp_precision": "nanoseconds",
                "field": "t_capture_elapsed_realtime_ns"
            },
            "write_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "writer",
                "timestamp_precision": "nanoseconds",
                "field": "t_elapsed_realtime_ns"
            }
        });

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "invalid metadata should still be counted"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "mismatched converted ns companion should fail metadata validation"
        );
    }

    #[test]
    fn validation_counts_reject_capture_timestamp_without_metadata() {
        let mut counts = ValidationCounts::default();
        let record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "session_start",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 2_u64,
            "t_capture_elapsed_realtime_ns": 4_u64,
            "input_actor": "human",
            "input_controller": null,
            "input_cadence_policy": "manual"
        });

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "partial new timestamp fields should still be counted"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "new capture timestamp field without metadata should fail"
        );
    }

    #[test]
    fn validation_counts_reject_wrong_timestamp_role_source() {
        let mut counts = ValidationCounts::default();
        let record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "system_back_event",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 2_u64,
            "t_elapsed_realtime_ns": 5_u64,
            "t_capture_elapsed_realtime_ns": 4_u64,
            "t_event_uptime_ms": 5_u64,
            "t_event_uptime_ns": 5_000_000_u64,
            "target_package": "org.example.input",
            "password_field": false,
            "event_time": {
                "clock_domain": "android_uptime_ms",
                "timestamp_source": "synthetic_handler",
                "timestamp_precision": "milliseconds",
                "field": "t_event_uptime_ms",
                "field_ns": "t_event_uptime_ns",
                "field_ns_precision": "milliseconds_converted_to_nanoseconds"
            },
            "capture_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "callback_capture",
                "timestamp_precision": "nanoseconds",
                "field": "t_capture_elapsed_realtime_ns"
            },
            "write_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "writer",
                "timestamp_precision": "nanoseconds",
                "field": "t_elapsed_realtime_ns"
            }
        });

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "invalid role source should still be counted"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "system back event_time must use key_event"
        );
    }

    #[test]
    fn validation_counts_reject_callback_only_event_time() {
        let mut counts = ValidationCounts::default();
        let record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "field_enter",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 2_u64,
            "t_elapsed_realtime_ns": 5_u64,
            "t_capture_elapsed_realtime_ns": 4_u64,
            "t_event_uptime_ms": 5_u64,
            "t_event_uptime_ns": 5_000_000_u64,
            "target_package": "org.example.input",
            "password_field": false,
            "event_time": {
                "clock_domain": "android_uptime_ms",
                "timestamp_source": "motion_event",
                "timestamp_precision": "milliseconds",
                "field": "t_event_uptime_ms",
                "field_ns": "t_event_uptime_ns",
                "field_ns_precision": "milliseconds_converted_to_nanoseconds"
            },
            "capture_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "callback_capture",
                "timestamp_precision": "nanoseconds",
                "field": "t_capture_elapsed_realtime_ns"
            },
            "write_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "writer",
                "timestamp_precision": "nanoseconds",
                "field": "t_elapsed_realtime_ns"
            }
        });

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "callback-only event_time should still be counted"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "callback-only records must not carry event_time"
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
