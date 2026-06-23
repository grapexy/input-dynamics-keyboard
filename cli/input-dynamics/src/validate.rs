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
        "clock_validation": counts.clock_validation_json(),
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
    timestamp_metadata_record_count: u64,
    legacy_timestamp_metadata_missing_count: u64,
    missing_timestamp_role_count: u64,
    missing_clock_domain_count: u64,
    invalid_clock_domain_count: u64,
    invalid_timestamp_source_count: u64,
    invalid_timestamp_precision_count: u64,
    timestamp_role_mismatch_count: u64,
    timestamp_field_reference_error_count: u64,
    timestamp_unit_mismatch_count: u64,
    timestamp_order_violation_count: u64,
    mixed_clock_domain_claim_count: u64,
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
            && self.timestamp_metadata_failure_count() == 0
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
        if self.missing_timestamp_role_count > 0 {
            reasons.push("missing_timestamp_roles");
        }
        if self.missing_clock_domain_count > 0 {
            reasons.push("missing_clock_domains");
        }
        if self.invalid_clock_domain_count > 0 {
            reasons.push("invalid_clock_domains");
        }
        if self.invalid_timestamp_source_count > 0 {
            reasons.push("invalid_timestamp_sources");
        }
        if self.invalid_timestamp_precision_count > 0 {
            reasons.push("invalid_timestamp_precisions");
        }
        if self.timestamp_role_mismatch_count > 0 {
            reasons.push("timestamp_role_mismatch");
        }
        if self.timestamp_field_reference_error_count > 0 {
            reasons.push("timestamp_field_reference_error");
        }
        if self.timestamp_unit_mismatch_count > 0 {
            reasons.push("timestamp_unit_mismatch");
        }
        if self.timestamp_order_violation_count > 0 {
            reasons.push("timestamp_order_violation");
        }
        if self.mixed_clock_domain_claim_count > 0 {
            reasons.push("mixed_clock_domain_claim");
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

    const fn timestamp_metadata_failure_count(&self) -> u64 {
        self.missing_timestamp_role_count
            .saturating_add(self.missing_clock_domain_count)
            .saturating_add(self.invalid_clock_domain_count)
            .saturating_add(self.invalid_timestamp_source_count)
            .saturating_add(self.invalid_timestamp_precision_count)
            .saturating_add(self.timestamp_role_mismatch_count)
            .saturating_add(self.timestamp_field_reference_error_count)
            .saturating_add(self.timestamp_unit_mismatch_count)
            .saturating_add(self.timestamp_order_violation_count)
            .saturating_add(self.mixed_clock_domain_claim_count)
    }

    fn clock_validation_json(&self) -> Value {
        json!({
            "timestamp_metadata_record_count": self.timestamp_metadata_record_count,
            "legacy_timestamp_metadata_missing_count": self.legacy_timestamp_metadata_missing_count,
            "missing_timestamp_role_count": self.missing_timestamp_role_count,
            "missing_clock_domain_count": self.missing_clock_domain_count,
            "invalid_clock_domain_count": self.invalid_clock_domain_count,
            "invalid_timestamp_source_count": self.invalid_timestamp_source_count,
            "invalid_timestamp_precision_count": self.invalid_timestamp_precision_count,
            "timestamp_role_mismatch_count": self.timestamp_role_mismatch_count,
            "timestamp_field_reference_error_count": self.timestamp_field_reference_error_count,
            "timestamp_unit_mismatch_count": self.timestamp_unit_mismatch_count,
            "timestamp_order_violation_count": self.timestamp_order_violation_count,
            "mixed_clock_domain_claim_count": self.mixed_clock_domain_claim_count,
        })
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
        if has_current_timestamp_metadata_signal(value) {
            increment(&mut self.timestamp_metadata_record_count)?;
        } else {
            increment(&mut self.legacy_timestamp_metadata_missing_count)?;
            return Ok(());
        }

        let failure_count_before = self.timestamp_metadata_failure_count();

        for role_name in timestamp_role_names() {
            if let Some(role) = value.get(role_name) {
                self.validate_timestamp_role(value, role_name, role)?;
            }
        }

        self.validate_required_timestamp_roles(value)?;
        self.validate_capture_write_order(value)?;
        self.validate_mixed_clock_domain_claim(value)?;
        if self.timestamp_metadata_failure_count() > failure_count_before {
            increment(&mut self.invalid_timestamp_metadata_count)?;
        }
        Ok(())
    }

    fn validate_timestamp_role(
        &mut self,
        record: &Value,
        role_name: &str,
        role: &Value,
    ) -> CliResult<()> {
        let clock_domain = required_string(role, "clock_domain");
        let timestamp_source = required_string(role, "timestamp_source");
        let timestamp_precision = required_string(role, "timestamp_precision");
        let field = required_string(role, "field");
        let expectation = timestamp_role_expectation(record, role_name);

        if !role.is_object() {
            increment(&mut self.timestamp_role_mismatch_count)?;
        }

        match clock_domain {
            Some(text) => match text.parse::<ClockDomain>() {
                Ok(parsed) => {
                    if expectation.is_some_and(|expected| parsed != expected.clock_domain) {
                        increment(&mut self.invalid_clock_domain_count)?;
                    }
                }
                Err(_error) => increment(&mut self.invalid_clock_domain_count)?,
            },
            None => increment(&mut self.missing_clock_domain_count)?,
        }

        match timestamp_source {
            Some(text) => match text.parse::<TimestampSource>() {
                Ok(parsed) => {
                    if expectation.is_some_and(|expected| parsed != expected.timestamp_source) {
                        increment(&mut self.invalid_timestamp_source_count)?;
                    }
                }
                Err(_error) => increment(&mut self.invalid_timestamp_source_count)?,
            },
            None => increment(&mut self.invalid_timestamp_source_count)?,
        }

        match timestamp_precision {
            Some(text) => match text.parse::<TimestampPrecision>() {
                Ok(parsed) => {
                    if expectation.is_some_and(|expected| parsed != expected.timestamp_precision) {
                        increment(&mut self.invalid_timestamp_precision_count)?;
                    }
                }
                Err(_error) => increment(&mut self.invalid_timestamp_precision_count)?,
            },
            None => increment(&mut self.invalid_timestamp_precision_count)?,
        }

        let Some(expected) = expectation else {
            increment(&mut self.timestamp_role_mismatch_count)?;
            return Ok(());
        };

        if field != Some(expected.field) {
            increment(&mut self.timestamp_field_reference_error_count)?;
        }
        if let Some(field_name) = field {
            if record.get(field_name).and_then(non_negative_i64).is_none() {
                increment(&mut self.timestamp_field_reference_error_count)?;
            }
        } else {
            increment(&mut self.timestamp_field_reference_error_count)?;
        }

        self.validate_timestamp_companion(record, role, field, expected)?;
        Ok(())
    }

    fn validate_timestamp_companion(
        &mut self,
        record: &Value,
        role: &Value,
        field: Option<&str>,
        expectation: TimestampRoleExpectation,
    ) -> CliResult<()> {
        let Some(field_ns) = role.get("field_ns") else {
            if role.get("field_ns_precision").is_some() || expectation.field_ns.is_some() {
                increment(&mut self.timestamp_field_reference_error_count)?;
            }
            return Ok(());
        };
        let Some(field_ns_name) = field_ns.as_str().filter(|text| !text.is_empty()) else {
            increment(&mut self.timestamp_field_reference_error_count)?;
            return Ok(());
        };
        let Some(field_ns_precision) = required_string(role, "field_ns_precision") else {
            increment(&mut self.invalid_timestamp_precision_count)?;
            return Ok(());
        };
        let Ok(precision) = field_ns_precision.parse::<TimestampPrecision>() else {
            increment(&mut self.invalid_timestamp_precision_count)?;
            return Ok(());
        };
        let Some(ns_value) = record.get(field_ns_name).and_then(non_negative_i64) else {
            increment(&mut self.timestamp_field_reference_error_count)?;
            return Ok(());
        };
        if Some(field_ns_name) != expectation.field_ns {
            increment(&mut self.timestamp_field_reference_error_count)?;
        }
        if Some(precision) != expectation.field_ns_precision {
            increment(&mut self.invalid_timestamp_precision_count)?;
        }
        if precision != TimestampPrecision::MillisecondsConvertedToNanoseconds {
            return Ok(());
        }
        let Some(field_name) = field else {
            increment(&mut self.timestamp_field_reference_error_count)?;
            return Ok(());
        };
        let Some(ms_value) = record.get(field_name).and_then(non_negative_i64) else {
            increment(&mut self.timestamp_field_reference_error_count)?;
            return Ok(());
        };
        if millis_to_nanos(ms_value) != Some(ns_value) {
            increment(&mut self.timestamp_unit_mismatch_count)?;
        }
        Ok(())
    }

    fn validate_required_timestamp_roles(&mut self, value: &Value) -> CliResult<()> {
        self.require_timestamp_role(value, "capture_time")?;
        self.require_timestamp_role(value, "write_time")?;
        match value.get("event").and_then(Value::as_str) {
            Some("pointer_sample") => {
                self.require_timestamp_role(value, "event_time")?;
                self.require_timestamp_role(value, "down_time")?;
            }
            Some("system_back_event") => {
                self.require_timestamp_role(value, "event_time")?;
            }
            Some(event) if requires_touch_sequence_id(event) && event.starts_with("key_") => {
                self.require_timestamp_role(value, "event_time")?;
            }
            Some(_) | None => {}
        }
        Ok(())
    }

    fn require_timestamp_role(&mut self, value: &Value, role_name: &str) -> CliResult<()> {
        if value.get(role_name).is_none() {
            increment(&mut self.missing_timestamp_role_count)?;
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
                increment(&mut self.timestamp_order_violation_count)?;
            }
        }
        Ok(())
    }

    fn validate_mixed_clock_domain_claim(&mut self, value: &Value) -> CliResult<()> {
        if !record_has_cross_domain_claim(value) {
            return Ok(());
        }
        let mut domains = Vec::new();
        for role_name in timestamp_role_names() {
            let Some(domain_text) = value
                .get(role_name)
                .and_then(|role| required_string(role, "clock_domain"))
            else {
                continue;
            };
            let Ok(domain) = domain_text.parse::<ClockDomain>() else {
                continue;
            };
            if !domains.contains(&domain) {
                domains.push(domain);
            }
        }
        if domains.len() > 1 {
            increment(&mut self.mixed_clock_domain_claim_count)?;
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

const fn current_timestamp_field_names() -> [&'static str; 5] {
    [
        "t_capture_elapsed_realtime_ns",
        "t_event_uptime_ms",
        "t_event_uptime_ns",
        "t_down_uptime_ms",
        "t_down_uptime_ns",
    ]
}

fn has_current_timestamp_metadata_signal(value: &Value) -> bool {
    timestamp_role_names()
        .into_iter()
        .any(|role_name| value.get(role_name).is_some())
        || current_timestamp_field_names()
            .into_iter()
            .any(|field_name| value.get(field_name).is_some())
}

fn record_has_cross_domain_claim(record: &Value) -> bool {
    record
        .get("time_delta_ms")
        .is_some_and(|value| !value.is_null())
        || record
            .pointer("/normalized_time/status")
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "bracketed" | "estimated"))
        || has_non_null_pointer(record, "/normalized_time/time_ns")
        || has_non_null_pointer(record, "/normalized_time/time_interval_ns")
}

fn has_non_null_pointer(value: &Value, pointer: &str) -> bool {
    value.pointer(pointer).is_some_and(|field| !field.is_null())
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

fn required_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
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
    use serde_json::{Value, json};

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
        assert_eq!(
            counts.timestamp_metadata_record_count, 1,
            "canonical metadata should be counted as current timestamp metadata"
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
        assert_eq!(
            counts.invalid_timestamp_source_count, 1,
            "unknown timestamp source should have a specific counter"
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
        assert_eq!(
            counts.timestamp_unit_mismatch_count, 1,
            "converted millisecond/nanosecond mismatch should be counted directly"
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
        assert_eq!(
            counts.missing_timestamp_role_count, 2,
            "partial current metadata should count missing capture/write roles"
        );
    }

    #[test]
    fn validation_counts_reject_source_event_timestamp_without_metadata() {
        let mut counts = ValidationCounts::default();
        let record = json!({
            "schema": "input_dynamics_event.v1",
            "session_id": "session-1",
            "event": "key_down",
            "t_wall_ms": 1_u64,
            "t_uptime_ms": 2_u64,
            "t_event_uptime_ms": 5_u64,
            "t_event_uptime_ns": 5_000_000_u64,
            "press_id": 1_u64,
            "gesture_id": 1_u64,
            "target_package": "org.example.input",
            "password_field": false
        });

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "partial source-event timestamp fields should still be counted"
        );
        assert_eq!(
            counts.timestamp_metadata_record_count, 1,
            "current source-event timestamp fields should not be classified as legacy"
        );
        assert_eq!(
            counts.legacy_timestamp_metadata_missing_count, 0,
            "current source-event timestamp fields should not increment legacy count"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "new source-event timestamp field without metadata should fail"
        );
        assert_eq!(
            counts.missing_timestamp_role_count, 3,
            "partial key timestamp metadata should count missing event/capture/write roles"
        );
    }

    #[test]
    fn validation_counts_reject_raw_mixed_domain_time_claim() {
        let mut counts = ValidationCounts::default();
        let mut record = canonical_key_down_record();
        add_normalized_time(
            &mut record,
            json!({
                "status": "bracketed",
                "clock_domain": "device_elapsed_realtime_ns",
                "time_ns": 5_000_000_i64
            }),
        );

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "mixed-domain time claim should still be counted"
        );
        assert_eq!(
            counts.mixed_clock_domain_claim_count, 1,
            "raw records with multiple source domains must not make normalized time claims"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "mixed-domain time claim should fail timestamp metadata once for the record"
        );
        assert!(
            counts
                .failure_reasons()
                .contains(&"mixed_clock_domain_claim"),
            "failure reasons should include mixed-domain claims"
        );
    }

    #[test]
    fn validation_counts_accept_mixed_domains_without_time_claim() {
        let mut counts = ValidationCounts::default();
        let mut record = canonical_key_down_record();
        add_normalized_time(
            &mut record,
            json!({
                "status": "unsupported_clock_domain",
                "clock_domain": null,
                "time_ns": null
            }),
        );

        let update_result = counts.update(&record);

        assert!(
            update_result.is_ok(),
            "unsupported metadata should still be counted"
        );
        assert_eq!(
            counts.mixed_clock_domain_claim_count, 0,
            "multiple raw timestamp role domains are allowed when no cross-domain claim is made"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 0,
            "unsupported non-claim metadata should not fail timestamp validation"
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
        assert_eq!(
            counts.invalid_timestamp_source_count, 1,
            "wrong role source should be counted directly"
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
        assert_eq!(
            counts.timestamp_role_mismatch_count, 1,
            "callback-only event_time should be reported as a role mismatch"
        );
    }

    #[test]
    fn validation_counts_reject_missing_clock_domain() {
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
            "missing clock domain should still be counted"
        );
        assert_eq!(
            counts.missing_clock_domain_count, 1,
            "missing event_time clock domain should be counted directly"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "missing clock domain should fail timestamp metadata once for the record"
        );
    }

    #[test]
    fn validation_counts_reject_invalid_clock_domain() {
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
                "clock_domain": "wallish_time",
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
            "invalid clock domain should still be counted"
        );
        assert_eq!(
            counts.invalid_clock_domain_count, 1,
            "invalid clock domain vocabulary should have a specific counter"
        );
        assert_eq!(
            counts.invalid_timestamp_metadata_count, 1,
            "invalid clock domain should fail timestamp metadata once for the record"
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
                .and_then(Value::as_str);
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

    fn canonical_key_down_record() -> Value {
        json!({
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
        })
    }

    fn add_normalized_time(record: &mut Value, normalized_time: Value) {
        let Some(record_object) = record.as_object_mut() else {
            unreachable!("canonical key down test record is an object");
        };
        record_object.insert(String::from("normalized_time"), normalized_time);
    }
}
