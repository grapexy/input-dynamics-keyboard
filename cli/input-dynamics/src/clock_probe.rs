//! Canonical device clock probe helpers.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use input_dynamics_analysis::clock::{
    AlignmentStatus, ClockDomain, TimestampPrecision, TimestampSource,
};
use serde_json::{Value, json};

use crate::app::App;
use crate::error::{CliError, CliResult};

pub(crate) const DEVICE_CLOCK_PROBE_SCHEMA: &str = "input_dynamics_device_clock_probe.v1";

static MONOTONIC_REFERENCE: OnceLock<Instant> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProbeFields {
    pub(crate) uptime_ms: i64,
    pub(crate) uptime_ns: i64,
    pub(crate) elapsed_realtime_ns: i64,
    pub(crate) wall_ms: i64,
}

pub(crate) fn capture_device_clock_probe(app: &App, phase: &str) -> CliResult<Value> {
    let host_wall_ms_before_device_timestamp = host_wall_millis()?;
    let host_monotonic_ns_before_device_timestamp = monotonic_nanos_since_process_start()?;
    let status = app.broadcast("STATUS", Vec::new())?;
    let host_wall_ms_after_device_timestamp = host_wall_millis()?;
    let host_monotonic_ns_after_device_timestamp = monotonic_nanos_since_process_start()?;

    if !status.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Err(CliError::new("device clock probe status returned ok:false"));
    }
    if status
        .get("pending_writes_drained")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(CliError::new(
            "device clock probe did not drain pending app writes",
        ));
    }

    let request_id = required_string(&status, "request_id")?;
    let probe = status
        .get("device_clock_probe")
        .ok_or_else(|| CliError::new("STATUS result is missing device_clock_probe"))?;
    let fields = validate_device_clock_probe(probe)?;
    let probe_request_id = required_string(probe, "request_id")?;
    if probe_request_id != request_id {
        return Err(CliError::new(format!(
            "device clock probe request_id mismatch: status={request_id} probe={probe_request_id}"
        )));
    }

    Ok(json!({
        "schema": DEVICE_CLOCK_PROBE_SCHEMA,
        "phase": phase,
        "probe_source": "ime_status_broadcast",
        "request_id": request_id,
        "package_name": status.get("package_name").cloned().unwrap_or(Value::Null),
        "command": status.get("command").cloned().unwrap_or(Value::Null),
        "result_file_path": status.get("result_file_path").cloned().unwrap_or(Value::Null),
        "status_file_path": status.get("status_file_path").cloned().unwrap_or(Value::Null),
        "host_wall_ms_before_device_timestamp": host_wall_ms_before_device_timestamp,
        "host_wall_ms_after_device_timestamp": host_wall_ms_after_device_timestamp,
        "host_monotonic_ns_before_device_timestamp": host_monotonic_ns_before_device_timestamp,
        "host_monotonic_ns_after_device_timestamp": host_monotonic_ns_after_device_timestamp,
        "host_monotonic_reference": "cli_process_start",
        "host_bracket": {
            "clock_domain": ClockDomain::HostProcessMonotonicNs.as_str(),
            "timestamp_source": TimestampSource::HostProcess.as_str(),
            "timestamp_precision": TimestampPrecision::Nanoseconds.as_str(),
            "before_ns": host_monotonic_ns_before_device_timestamp,
            "after_ns": host_monotonic_ns_after_device_timestamp,
        },
        "host_wall_bracket": {
            "clock_domain": ClockDomain::HostWallMs.as_str(),
            "timestamp_source": TimestampSource::HostProcess.as_str(),
            "timestamp_precision": TimestampPrecision::Milliseconds.as_str(),
            "before_ms": host_wall_ms_before_device_timestamp,
            "after_ms": host_wall_ms_after_device_timestamp,
        },
        "clock_domain": ClockDomain::DeviceElapsedRealtimeNs.as_str(),
        "clock_alignment_status": AlignmentStatus::NotEstimated.as_str(),
        "device_clock_probe": probe.clone(),
        "t_uptime_ms": fields.uptime_ms,
        "t_uptime_ns": fields.uptime_ns,
        "t_elapsed_realtime_ns": fields.elapsed_realtime_ns,
        "device_wall_ms": fields.wall_ms,
    }))
}

pub(crate) fn validate_probe_order(previous: &Value, next: &Value) -> CliResult<()> {
    let previous_fields = marker_probe_fields(previous, "previous")?;
    let next_fields = marker_probe_fields(next, "next")?;
    if next_fields.uptime_ms < previous_fields.uptime_ms {
        return Err(CliError::new(format!(
            "device clock probe uptime moved backwards: previous={} next={}",
            previous_fields.uptime_ms, next_fields.uptime_ms
        )));
    }
    if next_fields.uptime_ns < previous_fields.uptime_ns {
        return Err(CliError::new(format!(
            "device clock probe uptime ns moved backwards: previous={} next={}",
            previous_fields.uptime_ns, next_fields.uptime_ns
        )));
    }
    if next_fields.elapsed_realtime_ns < previous_fields.elapsed_realtime_ns {
        return Err(CliError::new(format!(
            "device clock probe elapsed realtime moved backwards: previous={} next={}",
            previous_fields.elapsed_realtime_ns, next_fields.elapsed_realtime_ns
        )));
    }
    Ok(())
}

pub(crate) fn validate_probe_marker(marker: &Value, label: &str) -> CliResult<ProbeFields> {
    let schema = required_string(marker, "schema")?;
    if schema != DEVICE_CLOCK_PROBE_SCHEMA {
        return Err(CliError::new(format!(
            "{label} marker has unsupported schema: {schema}"
        )));
    }
    require_string_value(marker, "probe_source", "ime_status_broadcast")?;
    require_string_value(
        marker,
        "clock_domain",
        ClockDomain::DeviceElapsedRealtimeNs.as_str(),
    )?;
    require_string_value(
        marker,
        "clock_alignment_status",
        AlignmentStatus::NotEstimated.as_str(),
    )?;
    require_string_value(marker, "host_monotonic_reference", "cli_process_start")?;
    validate_probe_bracket(
        marker,
        BracketExpectation {
            key: "host_bracket",
            clock_domain: ClockDomain::HostProcessMonotonicNs,
            timestamp_source: TimestampSource::HostProcess,
            timestamp_precision: TimestampPrecision::Nanoseconds,
            before_key: "before_ns",
            after_key: "after_ns",
        },
    )?;
    validate_probe_bracket(
        marker,
        BracketExpectation {
            key: "host_wall_bracket",
            clock_domain: ClockDomain::HostWallMs,
            timestamp_source: TimestampSource::HostProcess,
            timestamp_precision: TimestampPrecision::Milliseconds,
            before_key: "before_ms",
            after_key: "after_ms",
        },
    )?;
    let fields = marker_probe_fields(marker, label)?;
    let marker_request_id = required_string(marker, "request_id")?;
    let probe_request_id = marker
        .get("device_clock_probe")
        .ok_or_else(|| CliError::new(format!("{label} marker is missing device_clock_probe")))
        .and_then(|probe| required_string(probe, "request_id"))?;
    if marker_request_id != probe_request_id {
        return Err(CliError::new(format!(
            "{label} marker request_id mismatch: marker={marker_request_id} probe={probe_request_id}"
        )));
    }
    require_i64_value(marker, "t_uptime_ms", fields.uptime_ms)?;
    require_i64_value(marker, "t_uptime_ns", fields.uptime_ns)?;
    require_i64_value(marker, "t_elapsed_realtime_ns", fields.elapsed_realtime_ns)?;
    require_i64_value(marker, "device_wall_ms", fields.wall_ms)?;
    Ok(fields)
}

pub(crate) fn host_wall_millis() -> CliResult<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::new(format!("system clock is before Unix epoch: {error}")))?
        .as_millis();
    u64::try_from(millis).map_err(|error| CliError::new(format!("millis overflow: {error}")))
}

fn monotonic_nanos_since_process_start() -> CliResult<u64> {
    let reference = MONOTONIC_REFERENCE.get_or_init(Instant::now);
    u64::try_from(reference.elapsed().as_nanos())
        .map_err(|error| CliError::new(format!("monotonic nanos overflow: {error}")))
}

fn marker_probe_fields(marker: &Value, label: &str) -> CliResult<ProbeFields> {
    let probe = marker
        .get("device_clock_probe")
        .ok_or_else(|| CliError::new(format!("{label} marker is missing device_clock_probe")))?;
    validate_device_clock_probe(probe)
}

fn validate_device_clock_probe(probe: &Value) -> CliResult<ProbeFields> {
    let schema = required_string(probe, "schema")?;
    if schema != DEVICE_CLOCK_PROBE_SCHEMA {
        return Err(CliError::new(format!(
            "unsupported device_clock_probe schema: {schema}"
        )));
    }
    let uptime_ms = required_i64(probe, "t_uptime_ms")?;
    let uptime_ns = required_i64(probe, "t_uptime_ns")?;
    let elapsed_realtime_ns = required_i64(probe, "t_elapsed_realtime_ns")?;
    let wall_ms = required_i64(probe, "t_wall_ms")?;
    require_string_value(probe, "probe_source", "status_broadcast")?;
    require_string_value(probe, "captured_by", "android_control_status")?;
    require_string_value(
        probe,
        "canonical_clock_domain",
        ClockDomain::DeviceElapsedRealtimeNs.as_str(),
    )?;
    require_string_value(probe, "wall_time_role", "diagnostic")?;
    require_true_bool_value(probe, "pending_writes_drained")?;
    let expected_uptime_ns = uptime_ms
        .checked_mul(1_000_000)
        .ok_or_else(|| CliError::new("t_uptime_ms to nanoseconds conversion overflow"))?;
    if uptime_ns != expected_uptime_ns {
        return Err(CliError::new(format!(
            "device_clock_probe uptime conversion mismatch: t_uptime_ms={uptime_ms} t_uptime_ns={uptime_ns}"
        )));
    }
    if elapsed_realtime_ns < uptime_ns {
        return Err(CliError::new(format!(
            "device_clock_probe elapsed realtime is before uptime: elapsed={elapsed_realtime_ns} uptime_ns={uptime_ns}"
        )));
    }
    validate_timestamp_metadata(
        probe,
        MetadataExpectation {
            key: "uptime_time",
            clock_domain: ClockDomain::AndroidUptimeMs,
            timestamp_source: TimestampSource::CallbackCapture,
            timestamp_precision: TimestampPrecision::Milliseconds,
            field: "t_uptime_ms",
            field_ns: Some("t_uptime_ns"),
            field_ns_precision: Some(TimestampPrecision::MillisecondsConvertedToNanoseconds),
        },
    )?;
    validate_timestamp_metadata(
        probe,
        MetadataExpectation {
            key: "elapsed_realtime_time",
            clock_domain: ClockDomain::DeviceElapsedRealtimeNs,
            timestamp_source: TimestampSource::CallbackCapture,
            timestamp_precision: TimestampPrecision::Nanoseconds,
            field: "t_elapsed_realtime_ns",
            field_ns: None,
            field_ns_precision: None,
        },
    )?;
    validate_timestamp_metadata(
        probe,
        MetadataExpectation {
            key: "wall_time",
            clock_domain: ClockDomain::DeviceWallMs,
            timestamp_source: TimestampSource::CallbackCapture,
            timestamp_precision: TimestampPrecision::Milliseconds,
            field: "t_wall_ms",
            field_ns: None,
            field_ns_precision: None,
        },
    )?;
    Ok(ProbeFields {
        uptime_ms,
        uptime_ns,
        elapsed_realtime_ns,
        wall_ms,
    })
}

#[derive(Clone, Copy)]
struct BracketExpectation {
    key: &'static str,
    clock_domain: ClockDomain,
    timestamp_source: TimestampSource,
    timestamp_precision: TimestampPrecision,
    before_key: &'static str,
    after_key: &'static str,
}

fn validate_probe_bracket(value: &Value, expected: BracketExpectation) -> CliResult<()> {
    let key = expected.key;
    let bracket = value
        .get(key)
        .ok_or_else(|| CliError::new(format!("probe marker is missing {key}")))?;
    require_string_value(bracket, "clock_domain", expected.clock_domain.as_str())?;
    require_string_value(
        bracket,
        "timestamp_source",
        expected.timestamp_source.as_str(),
    )?;
    require_string_value(
        bracket,
        "timestamp_precision",
        expected.timestamp_precision.as_str(),
    )?;
    let before = required_i64(bracket, expected.before_key)?;
    let after = required_i64(bracket, expected.after_key)?;
    if after < before {
        return Err(CliError::new(format!(
            "{} moved backwards: {}={before} {}={after}",
            expected.key, expected.before_key, expected.after_key
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct MetadataExpectation {
    key: &'static str,
    clock_domain: ClockDomain,
    timestamp_source: TimestampSource,
    timestamp_precision: TimestampPrecision,
    field: &'static str,
    field_ns: Option<&'static str>,
    field_ns_precision: Option<TimestampPrecision>,
}

fn validate_timestamp_metadata(value: &Value, expected: MetadataExpectation) -> CliResult<()> {
    let key = expected.key;
    let metadata = value
        .get(key)
        .ok_or_else(|| CliError::new(format!("device_clock_probe is missing {key}")))?;
    let actual_clock_domain = required_string(metadata, "clock_domain")?;
    let actual_timestamp_source = required_string(metadata, "timestamp_source")?;
    let actual_timestamp_precision = required_string(metadata, "timestamp_precision")?;
    let actual_field = required_string(metadata, "field")?;
    if actual_clock_domain != expected.clock_domain.as_str()
        || actual_clock_domain.parse::<ClockDomain>().is_err()
    {
        return Err(CliError::new(format!(
            "{key}.clock_domain mismatch: expected={} actual={actual_clock_domain}",
            expected.clock_domain.as_str()
        )));
    }
    if actual_timestamp_source != expected.timestamp_source.as_str()
        || actual_timestamp_source.parse::<TimestampSource>().is_err()
    {
        return Err(CliError::new(format!(
            "{key}.timestamp_source mismatch: expected={} actual={actual_timestamp_source}",
            expected.timestamp_source.as_str()
        )));
    }
    if actual_timestamp_precision != expected.timestamp_precision.as_str()
        || actual_timestamp_precision
            .parse::<TimestampPrecision>()
            .is_err()
    {
        return Err(CliError::new(format!(
            "{key}.timestamp_precision mismatch: expected={} actual={actual_timestamp_precision}",
            expected.timestamp_precision.as_str()
        )));
    }
    if actual_field != expected.field {
        return Err(CliError::new(format!(
            "{key}.field mismatch: expected={} actual={actual_field}",
            expected.field
        )));
    }

    match expected.field_ns {
        Some(expected_field) => {
            let actual = required_string(metadata, "field_ns")?;
            if actual != expected_field {
                return Err(CliError::new(format!(
                    "{key}.field_ns mismatch: expected={expected_field} actual={actual}"
                )));
            }
        }
        None if metadata.get("field_ns").is_some() => {
            return Err(CliError::new(format!("{key}.field_ns must be absent")));
        }
        None => {}
    }

    match expected.field_ns_precision {
        Some(expected_precision) => {
            let actual = required_string(metadata, "field_ns_precision")?;
            if actual != expected_precision.as_str() {
                return Err(CliError::new(format!(
                    "{key}.field_ns_precision mismatch: expected={} actual={actual}",
                    expected_precision.as_str()
                )));
            }
        }
        None if metadata.get("field_ns_precision").is_some() => {
            return Err(CliError::new(format!(
                "{key}.field_ns_precision must be absent"
            )));
        }
        None => {}
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, key: &str) -> CliResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| CliError::new(format!("missing or invalid string field: {key}")))
}

fn require_string_value(value: &Value, key: &str, expected: &str) -> CliResult<()> {
    let actual = required_string(value, key)?;
    if actual == expected {
        Ok(())
    } else {
        Err(CliError::new(format!(
            "{key} mismatch: expected={expected} actual={actual}"
        )))
    }
}

fn require_true_bool_value(value: &Value, key: &str) -> CliResult<()> {
    let actual = value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| CliError::new(format!("missing or invalid boolean field: {key}")))?;
    if actual {
        Ok(())
    } else {
        Err(CliError::new(format!("{key} mismatch: expected=true")))
    }
}

fn require_i64_value(value: &Value, key: &str, expected: i64) -> CliResult<()> {
    let actual = required_i64(value, key)?;
    if actual == expected {
        Ok(())
    } else {
        Err(CliError::new(format!(
            "{key} mismatch: expected={expected} actual={actual}"
        )))
    }
}

fn required_i64(value: &Value, key: &str) -> CliResult<i64> {
    let number = value
        .get(key)
        .ok_or_else(|| CliError::new(format!("missing numeric field: {key}")))?;
    let parsed = if let Some(signed) = number.as_i64() {
        signed
    } else if let Some(unsigned) = number.as_u64() {
        i64::try_from(unsigned)
            .map_err(|error| CliError::new(format!("numeric field overflow for {key}: {error}")))?
    } else {
        return Err(CliError::new(format!("invalid numeric field: {key}")));
    };
    if parsed < 0 {
        return Err(CliError::new(format!("negative numeric field: {key}")));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use serde_json::{Value, json};

    use crate::clock_probe::{
        DEVICE_CLOCK_PROBE_SCHEMA, validate_device_clock_probe, validate_probe_order,
    };

    #[test]
    fn accepts_complete_device_clock_probe() -> Result<(), String> {
        let probe = probe_json(10, 11);

        let Ok(fields) = validate_device_clock_probe(&probe) else {
            return Err(String::from("probe should validate"));
        };

        ensure_eq(&fields.uptime_ms, &10, "uptime_ms")?;
        ensure_eq(&fields.uptime_ns, &10_000_000, "uptime_ns")?;
        ensure_eq(
            &fields.elapsed_realtime_ns,
            &11_000_000,
            "elapsed_realtime_ns",
        )?;
        ensure_eq(&fields.wall_ms, &1_800_000_000_000, "wall_ms")?;
        Ok(())
    }

    #[test]
    fn rejects_probe_without_schema_object() -> Result<(), String> {
        let mut probe = probe_json(10, 11);
        let object = probe
            .as_object_mut()
            .ok_or_else(|| String::from("probe should be an object"))?;
        object.remove("schema");

        let Err(error) = validate_device_clock_probe(&probe) else {
            return Err(String::from("schema should be required"));
        };

        ensure_contains(&error.to_string(), "schema")?;
        Ok(())
    }

    #[test]
    fn rejects_inconsistent_uptime_nanoseconds() -> Result<(), String> {
        let mut probe = probe_json(10, 11);
        let object = probe
            .as_object_mut()
            .ok_or_else(|| String::from("probe should be an object"))?;
        object.insert(String::from("t_uptime_ns"), json!(9_999_999_i64));

        let Err(error) = validate_device_clock_probe(&probe) else {
            return Err(String::from("conversion should fail"));
        };

        ensure_contains(&error.to_string(), "uptime conversion mismatch")?;
        Ok(())
    }

    #[test]
    fn rejects_elapsed_before_uptime() -> Result<(), String> {
        let probe = probe_json(20, 19);

        let Err(error) = validate_device_clock_probe(&probe) else {
            return Err(String::from("elapsed should fail"));
        };

        ensure_contains(&error.to_string(), "elapsed realtime is before uptime")?;
        Ok(())
    }

    #[test]
    fn rejects_malformed_metadata_vocabulary() -> Result<(), String> {
        let mut probe = probe_json(10, 11);
        let metadata = probe
            .get_mut("elapsed_realtime_time")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| String::from("elapsed metadata should be an object"))?;
        metadata.insert(String::from("clock_domain"), json!("wallish"));

        let Err(error) = validate_device_clock_probe(&probe) else {
            return Err(String::from("metadata should fail"));
        };

        ensure_contains(&error.to_string(), "elapsed_realtime_time.clock_domain")?;
        Ok(())
    }

    #[test]
    fn rejects_wrong_probe_identity_fields() -> Result<(), String> {
        for (field, bad_value) in [
            ("probe_source", "other_source"),
            ("captured_by", "other_component"),
            ("canonical_clock_domain", "device_wall_ms"),
            ("wall_time_role", "ordering"),
        ] {
            let mut probe = probe_json(10, 11);
            let object = probe
                .as_object_mut()
                .ok_or_else(|| String::from("probe should be an object"))?;
            object.insert(String::from(field), json!(bad_value));

            let Err(error) = validate_device_clock_probe(&probe) else {
                return Err(format!("{field} should fail"));
            };

            ensure_contains(&error.to_string(), field)?;
        }
        Ok(())
    }

    #[test]
    fn rejects_missing_or_false_pending_write_drain() -> Result<(), String> {
        let mut false_probe = probe_json(10, 11);
        let false_object = false_probe
            .as_object_mut()
            .ok_or_else(|| String::from("probe should be an object"))?;
        false_object.insert(String::from("pending_writes_drained"), json!(false));
        let Err(false_error) = validate_device_clock_probe(&false_probe) else {
            return Err(String::from("false pending_writes_drained should fail"));
        };
        ensure_contains(&false_error.to_string(), "pending_writes_drained")?;

        let mut missing_probe = probe_json(10, 11);
        let missing_object = missing_probe
            .as_object_mut()
            .ok_or_else(|| String::from("probe should be an object"))?;
        missing_object.remove("pending_writes_drained");
        let Err(missing_error) = validate_device_clock_probe(&missing_probe) else {
            return Err(String::from("missing pending_writes_drained should fail"));
        };
        ensure_contains(&missing_error.to_string(), "pending_writes_drained")?;
        Ok(())
    }

    #[test]
    fn rejects_decreasing_marker_order() -> Result<(), String> {
        let previous_probe = probe_json(20, 25);
        let next_probe = probe_json(19, 26);
        let previous = marker_json(&previous_probe);
        let next = marker_json(&next_probe);

        let Err(error) = validate_probe_order(&previous, &next) else {
            return Err(String::from("order should fail"));
        };

        ensure_contains(&error.to_string(), "uptime moved backwards")?;
        Ok(())
    }

    proptest::proptest! {
        #[test]
        fn accepts_generated_consistent_probe_values(
            uptime_ms in 0_i64..1_000_000_i64,
            elapsed_extra_ms in 0_i64..1_000_000_i64,
        ) {
            let Some(elapsed_ms) = uptime_ms.checked_add(elapsed_extra_ms) else {
                proptest::prop_assert!(false, "elapsed milliseconds overflowed");
                return Ok(());
            };
            let probe = probe_json(uptime_ms, elapsed_ms);

            let Ok(fields) = validate_device_clock_probe(&probe) else {
                proptest::prop_assert!(false, "generated probe should validate");
                return Ok(());
            };

            let Some(expected_uptime_ns) = uptime_ms.checked_mul(1_000_000) else {
                proptest::prop_assert!(false, "uptime ns overflowed");
                return Ok(());
            };
            proptest::prop_assert_eq!(fields.uptime_ns, expected_uptime_ns);
            proptest::prop_assert!(fields.elapsed_realtime_ns >= fields.uptime_ns);
        }
    }

    fn marker_json(probe: &Value) -> Value {
        json!({"device_clock_probe": probe})
    }

    fn ensure_eq<T>(actual: &T, expected: &T, label: &str) -> Result<(), String>
    where
        T: Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!(
                "{label} mismatch: actual={actual:?} expected={expected:?}"
            ))
        }
    }

    fn ensure_contains(text: &str, needle: &str) -> Result<(), String> {
        if text.contains(needle) {
            Ok(())
        } else {
            Err(format!("expected '{text}' to contain '{needle}'"))
        }
    }

    fn probe_json(uptime_ms: i64, elapsed_realtime_ms: i64) -> Value {
        let uptime_ns = uptime_ms.saturating_mul(1_000_000);
        let elapsed_realtime_ns = elapsed_realtime_ms.saturating_mul(1_000_000);
        json!({
            "schema": DEVICE_CLOCK_PROBE_SCHEMA,
            "request_id": "idk-test",
            "probe_source": "status_broadcast",
            "captured_by": "android_control_status",
            "canonical_clock_domain": "device_elapsed_realtime_ns",
            "t_uptime_ms": uptime_ms,
            "t_uptime_ns": uptime_ns,
            "uptime_time": {
                "clock_domain": "android_uptime_ms",
                "timestamp_source": "callback_capture",
                "timestamp_precision": "milliseconds",
                "field": "t_uptime_ms",
                "field_ns": "t_uptime_ns",
                "field_ns_precision": "milliseconds_converted_to_nanoseconds"
            },
            "t_elapsed_realtime_ns": elapsed_realtime_ns,
            "elapsed_realtime_time": {
                "clock_domain": "device_elapsed_realtime_ns",
                "timestamp_source": "callback_capture",
                "timestamp_precision": "nanoseconds",
                "field": "t_elapsed_realtime_ns"
            },
            "t_wall_ms": 1_800_000_000_000_i64,
            "wall_time": {
                "clock_domain": "device_wall_ms",
                "timestamp_source": "callback_capture",
                "timestamp_precision": "milliseconds",
                "field": "t_wall_ms"
            },
            "wall_time_role": "diagnostic",
            "pending_writes_drained": true
        })
    }
}
