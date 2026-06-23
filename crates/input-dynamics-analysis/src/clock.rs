//! Clock-domain vocabulary for input-dynamics analysis.

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Error returned when parsing a clock-domain vocabulary value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClockParseError {
    message: String,
}

impl ClockParseError {
    fn new(value_type: &str, value: &str) -> Self {
        Self {
            message: format!("unknown {value_type}: {value}"),
        }
    }
}

impl Display for ClockParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ClockParseError {}

/// Timestamp clock domains used by source and derived artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockDomain {
    /// Android uptime clock used by `MotionEvent` and `KeyEvent`, in milliseconds.
    AndroidUptimeMs,
    /// Android uptime clock used by `MotionEvent` and `KeyEvent`, in nanoseconds.
    AndroidUptimeNs,
    /// Android elapsed realtime clock, including deep sleep, in nanoseconds.
    DeviceElapsedRealtimeNs,
    /// Raw `getevent -lt` timestamp domain, in microseconds.
    KernelGeteventUs,
    /// Media presentation timestamp domain for encoded video frames, in nanoseconds.
    MediaPtsNs,
    /// CLI-process-relative monotonic clock, in nanoseconds.
    HostProcessMonotonicNs,
    /// Host wall clock, in milliseconds since Unix epoch.
    HostWallMs,
    /// Device wall clock, in milliseconds since Unix epoch.
    DeviceWallMs,
}

impl ClockDomain {
    /// Return the stable JSON string for this clock domain.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AndroidUptimeMs => "android_uptime_ms",
            Self::AndroidUptimeNs => "android_uptime_ns",
            Self::DeviceElapsedRealtimeNs => "device_elapsed_realtime_ns",
            Self::KernelGeteventUs => "kernel_getevent_us",
            Self::MediaPtsNs => "media_pts_ns",
            Self::HostProcessMonotonicNs => "host_process_monotonic_ns",
            Self::HostWallMs => "host_wall_ms",
            Self::DeviceWallMs => "device_wall_ms",
        }
    }
}

impl Display for ClockDomain {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ClockDomain {
    type Err = ClockParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "android_uptime_ms" => Ok(Self::AndroidUptimeMs),
            "android_uptime_ns" => Ok(Self::AndroidUptimeNs),
            "device_elapsed_realtime_ns" => Ok(Self::DeviceElapsedRealtimeNs),
            "kernel_getevent_us" => Ok(Self::KernelGeteventUs),
            "media_pts_ns" => Ok(Self::MediaPtsNs),
            "host_process_monotonic_ns" => Ok(Self::HostProcessMonotonicNs),
            "host_wall_ms" => Ok(Self::HostWallMs),
            "device_wall_ms" => Ok(Self::DeviceWallMs),
            other => Err(ClockParseError::new("clock domain", other)),
        }
    }
}

/// Timestamp precision metadata for recorded or derived timestamps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampPrecision {
    /// Native nanosecond timestamp value from the source domain.
    Nanoseconds,
    /// Native microsecond timestamp value from the source domain.
    Microseconds,
    /// Native millisecond timestamp value from the source domain.
    Milliseconds,
    /// A millisecond source timestamp represented in nanosecond units.
    MillisecondsConvertedToNanoseconds,
}

impl TimestampPrecision {
    /// Return the stable JSON string for this timestamp precision.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nanoseconds => "nanoseconds",
            Self::Microseconds => "microseconds",
            Self::Milliseconds => "milliseconds",
            Self::MillisecondsConvertedToNanoseconds => "milliseconds_converted_to_nanoseconds",
        }
    }
}

impl Display for TimestampPrecision {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TimestampPrecision {
    type Err = ClockParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "nanoseconds" => Ok(Self::Nanoseconds),
            "microseconds" => Ok(Self::Microseconds),
            "milliseconds" => Ok(Self::Milliseconds),
            "milliseconds_converted_to_nanoseconds" => Ok(Self::MillisecondsConvertedToNanoseconds),
            other => Err(ClockParseError::new("timestamp precision", other)),
        }
    }
}

/// Timestamp source metadata for recorded or derived timestamps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampSource {
    /// Timestamp from an Android `MotionEvent`.
    MotionEvent,
    /// Timestamp from an Android `KeyEvent`.
    KeyEvent,
    /// Timestamp captured by an Android callback.
    CallbackCapture,
    /// Timestamp generated by synthetic Android handler code.
    SyntheticHandler,
    /// Timestamp captured by an asynchronous writer.
    Writer,
    /// Timestamp captured by an ADB shell command or command result.
    AdbShell,
    /// Timestamp captured by the host-side CLI process.
    HostProcess,
    /// Timestamp captured by probing media metadata.
    MediaProbe,
    /// Timestamp produced by a derived transform.
    DerivedTransform,
}

impl TimestampSource {
    /// Return the stable JSON string for this timestamp source.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MotionEvent => "motion_event",
            Self::KeyEvent => "key_event",
            Self::CallbackCapture => "callback_capture",
            Self::SyntheticHandler => "synthetic_handler",
            Self::Writer => "writer",
            Self::AdbShell => "adb_shell",
            Self::HostProcess => "host_process",
            Self::MediaProbe => "media_probe",
            Self::DerivedTransform => "derived_transform",
        }
    }
}

impl Display for TimestampSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TimestampSource {
    type Err = ClockParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "motion_event" => Ok(Self::MotionEvent),
            "key_event" => Ok(Self::KeyEvent),
            "callback_capture" => Ok(Self::CallbackCapture),
            "synthetic_handler" => Ok(Self::SyntheticHandler),
            "writer" => Ok(Self::Writer),
            "adb_shell" => Ok(Self::AdbShell),
            "host_process" => Ok(Self::HostProcess),
            "media_probe" => Ok(Self::MediaProbe),
            "derived_transform" => Ok(Self::DerivedTransform),
            other => Err(ClockParseError::new("timestamp source", other)),
        }
    }
}

/// Alignment status for comparing or transforming clock domains.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentStatus {
    /// Mapping is bounded by before/after timing anchors.
    Bracketed,
    /// Mapping uses legacy wall-clock anchors and has lower confidence.
    LegacyWallClockBracketed,
    /// Mapping is estimated and should carry widened uncertainty.
    Estimated,
    /// Clock domains cannot currently be compared safely.
    UnsupportedClockDomain,
    /// Event or timestamp falls outside the mapped interval.
    OutsideRange,
    /// Source artifacts changed after the derived alignment was created.
    StaleInputs,
    /// Required source artifact is missing.
    MissingSource,
    /// External probe or media inspection failed.
    ProbeFailed,
    /// Alignment has not been estimated.
    NotEstimated,
}

impl AlignmentStatus {
    /// Return the stable JSON string for this alignment status.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bracketed => "bracketed",
            Self::LegacyWallClockBracketed => "legacy_wall_clock_bracketed",
            Self::Estimated => "estimated",
            Self::UnsupportedClockDomain => "unsupported_clock_domain",
            Self::OutsideRange => "outside_range",
            Self::StaleInputs => "stale_inputs",
            Self::MissingSource => "missing_source",
            Self::ProbeFailed => "probe_failed",
            Self::NotEstimated => "not_estimated",
        }
    }
}

impl Display for AlignmentStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AlignmentStatus {
    type Err = ClockParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "bracketed" => Ok(Self::Bracketed),
            "legacy_wall_clock_bracketed" => Ok(Self::LegacyWallClockBracketed),
            "estimated" => Ok(Self::Estimated),
            "unsupported_clock_domain" => Ok(Self::UnsupportedClockDomain),
            "outside_range" => Ok(Self::OutsideRange),
            "stale_inputs" => Ok(Self::StaleInputs),
            "missing_source" => Ok(Self::MissingSource),
            "probe_failed" => Ok(Self::ProbeFailed),
            "not_estimated" => Ok(Self::NotEstimated),
            other => Err(ClockParseError::new("alignment status", other)),
        }
    }
}

/// Convert a non-negative millisecond timestamp into nanosecond units.
pub const fn millis_to_nanos(timestamp_ms: i64) -> Option<i64> {
    if timestamp_ms < 0 {
        return None;
    }
    timestamp_ms.checked_mul(1_000_000)
}

/// Convert a non-negative microsecond timestamp into nanosecond units.
pub const fn micros_to_nanos(timestamp_us: i64) -> Option<i64> {
    if timestamp_us < 0 {
        return None;
    }
    timestamp_us.checked_mul(1_000)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use proptest::prelude::{Strategy, any, prop_assert_eq, proptest};
    use serde_json::json;

    use crate::clock::{
        AlignmentStatus, ClockDomain, TimestampPrecision, TimestampSource, micros_to_nanos,
        millis_to_nanos,
    };

    #[derive(Clone, Copy, Debug)]
    struct VocabularyCase<T> {
        value: T,
        text: &'static str,
    }

    const CLOCK_DOMAIN_CASES: &[VocabularyCase<ClockDomain>] = &[
        VocabularyCase {
            value: ClockDomain::AndroidUptimeMs,
            text: "android_uptime_ms",
        },
        VocabularyCase {
            value: ClockDomain::AndroidUptimeNs,
            text: "android_uptime_ns",
        },
        VocabularyCase {
            value: ClockDomain::DeviceElapsedRealtimeNs,
            text: "device_elapsed_realtime_ns",
        },
        VocabularyCase {
            value: ClockDomain::KernelGeteventUs,
            text: "kernel_getevent_us",
        },
        VocabularyCase {
            value: ClockDomain::MediaPtsNs,
            text: "media_pts_ns",
        },
        VocabularyCase {
            value: ClockDomain::HostProcessMonotonicNs,
            text: "host_process_monotonic_ns",
        },
        VocabularyCase {
            value: ClockDomain::HostWallMs,
            text: "host_wall_ms",
        },
        VocabularyCase {
            value: ClockDomain::DeviceWallMs,
            text: "device_wall_ms",
        },
    ];

    const TIMESTAMP_PRECISION_CASES: &[VocabularyCase<TimestampPrecision>] = &[
        VocabularyCase {
            value: TimestampPrecision::Nanoseconds,
            text: "nanoseconds",
        },
        VocabularyCase {
            value: TimestampPrecision::Microseconds,
            text: "microseconds",
        },
        VocabularyCase {
            value: TimestampPrecision::Milliseconds,
            text: "milliseconds",
        },
        VocabularyCase {
            value: TimestampPrecision::MillisecondsConvertedToNanoseconds,
            text: "milliseconds_converted_to_nanoseconds",
        },
    ];

    const TIMESTAMP_SOURCE_CASES: &[VocabularyCase<TimestampSource>] = &[
        VocabularyCase {
            value: TimestampSource::MotionEvent,
            text: "motion_event",
        },
        VocabularyCase {
            value: TimestampSource::KeyEvent,
            text: "key_event",
        },
        VocabularyCase {
            value: TimestampSource::CallbackCapture,
            text: "callback_capture",
        },
        VocabularyCase {
            value: TimestampSource::SyntheticHandler,
            text: "synthetic_handler",
        },
        VocabularyCase {
            value: TimestampSource::Writer,
            text: "writer",
        },
        VocabularyCase {
            value: TimestampSource::AdbShell,
            text: "adb_shell",
        },
        VocabularyCase {
            value: TimestampSource::HostProcess,
            text: "host_process",
        },
        VocabularyCase {
            value: TimestampSource::MediaProbe,
            text: "media_probe",
        },
        VocabularyCase {
            value: TimestampSource::DerivedTransform,
            text: "derived_transform",
        },
    ];

    const ALIGNMENT_STATUS_CASES: &[VocabularyCase<AlignmentStatus>] = &[
        VocabularyCase {
            value: AlignmentStatus::Bracketed,
            text: "bracketed",
        },
        VocabularyCase {
            value: AlignmentStatus::LegacyWallClockBracketed,
            text: "legacy_wall_clock_bracketed",
        },
        VocabularyCase {
            value: AlignmentStatus::Estimated,
            text: "estimated",
        },
        VocabularyCase {
            value: AlignmentStatus::UnsupportedClockDomain,
            text: "unsupported_clock_domain",
        },
        VocabularyCase {
            value: AlignmentStatus::OutsideRange,
            text: "outside_range",
        },
        VocabularyCase {
            value: AlignmentStatus::StaleInputs,
            text: "stale_inputs",
        },
        VocabularyCase {
            value: AlignmentStatus::MissingSource,
            text: "missing_source",
        },
        VocabularyCase {
            value: AlignmentStatus::ProbeFailed,
            text: "probe_failed",
        },
        VocabularyCase {
            value: AlignmentStatus::NotEstimated,
            text: "not_estimated",
        },
    ];

    #[test]
    fn clock_domain_strings_are_stable() {
        for case in CLOCK_DOMAIN_CASES {
            assert_eq!(case.value.as_str(), case.text, "clock domain as_str");
            assert_eq!(case.value.to_string(), case.text, "clock domain Display");
            assert_eq!(
                ClockDomain::from_str(case.text),
                Ok(case.value),
                "clock domain FromStr"
            );
            assert_eq!(
                serde_json::to_value(case.value).map_err(|err| err.to_string()),
                Ok(json!(case.text)),
                "clock domain serde serialize"
            );
            assert_eq!(
                serde_json::from_value::<ClockDomain>(json!(case.text))
                    .map_err(|err| err.to_string()),
                Ok(case.value),
                "clock domain serde deserialize"
            );
        }
    }

    #[test]
    fn timestamp_precision_strings_are_stable() {
        for case in TIMESTAMP_PRECISION_CASES {
            assert_eq!(case.value.as_str(), case.text, "timestamp precision as_str");
            assert_eq!(
                case.value.to_string(),
                case.text,
                "timestamp precision Display"
            );
            assert_eq!(
                TimestampPrecision::from_str(case.text),
                Ok(case.value),
                "timestamp precision FromStr"
            );
            assert_eq!(
                serde_json::to_value(case.value).map_err(|err| err.to_string()),
                Ok(json!(case.text)),
                "timestamp precision serde serialize"
            );
            assert_eq!(
                serde_json::from_value::<TimestampPrecision>(json!(case.text))
                    .map_err(|err| err.to_string()),
                Ok(case.value),
                "timestamp precision serde deserialize"
            );
        }
    }

    #[test]
    fn timestamp_source_strings_are_stable() {
        for case in TIMESTAMP_SOURCE_CASES {
            assert_eq!(case.value.as_str(), case.text, "timestamp source as_str");
            assert_eq!(
                case.value.to_string(),
                case.text,
                "timestamp source Display"
            );
            assert_eq!(
                TimestampSource::from_str(case.text),
                Ok(case.value),
                "timestamp source FromStr"
            );
            assert_eq!(
                serde_json::to_value(case.value).map_err(|err| err.to_string()),
                Ok(json!(case.text)),
                "timestamp source serde serialize"
            );
            assert_eq!(
                serde_json::from_value::<TimestampSource>(json!(case.text))
                    .map_err(|err| err.to_string()),
                Ok(case.value),
                "timestamp source serde deserialize"
            );
        }
    }

    #[test]
    fn alignment_status_strings_are_stable() {
        for case in ALIGNMENT_STATUS_CASES {
            assert_eq!(case.value.as_str(), case.text, "alignment status as_str");
            assert_eq!(
                case.value.to_string(),
                case.text,
                "alignment status Display"
            );
            assert_eq!(
                AlignmentStatus::from_str(case.text),
                Ok(case.value),
                "alignment status FromStr"
            );
            assert_eq!(
                serde_json::to_value(case.value).map_err(|err| err.to_string()),
                Ok(json!(case.text)),
                "alignment status serde serialize"
            );
            assert_eq!(
                serde_json::from_value::<AlignmentStatus>(json!(case.text))
                    .map_err(|err| err.to_string()),
                Ok(case.value),
                "alignment status serde deserialize"
            );
        }
    }

    #[test]
    fn unknown_vocabulary_values_fail() {
        assert!(
            ClockDomain::from_str("wallish_time").is_err(),
            "unknown clock domain should fail"
        );
        assert!(
            TimestampPrecision::from_str("almost_ns").is_err(),
            "unknown precision should fail"
        );
        assert!(
            TimestampSource::from_str("timer").is_err(),
            "unknown timestamp source should fail"
        );
        assert!(
            AlignmentStatus::from_str("probably_ok").is_err(),
            "unknown alignment status should fail"
        );
    }

    #[test]
    fn conversion_overflow_boundaries_return_none() {
        assert_eq!(
            millis_to_nanos(9_223_372_036_854),
            Some(9_223_372_036_854_000_000),
            "largest supported millisecond boundary should convert"
        );
        assert_eq!(
            millis_to_nanos(9_223_372_036_855),
            None,
            "overflowing millisecond boundary should fail"
        );
        assert_eq!(
            micros_to_nanos(9_223_372_036_854_775),
            Some(9_223_372_036_854_775_000),
            "largest supported microsecond boundary should convert"
        );
        assert_eq!(
            micros_to_nanos(9_223_372_036_854_776),
            None,
            "overflowing microsecond boundary should fail"
        );
    }

    proptest! {
        #[test]
        fn non_negative_millisecond_conversion_matches_checked_mul(
            timestamp_ms in 0_i64..9_000_000_000_i64
        ) {
            prop_assert_eq!(
                millis_to_nanos(timestamp_ms),
                timestamp_ms.checked_mul(1_000_000),
                "millisecond conversion should use checked multiplication"
            );
        }

        #[test]
        fn non_negative_microsecond_conversion_matches_checked_mul(
            timestamp_us in 0_i64..9_000_000_000_000_i64
        ) {
            prop_assert_eq!(
                micros_to_nanos(timestamp_us),
                timestamp_us.checked_mul(1_000),
                "microsecond conversion should use checked multiplication"
            );
        }

        #[test]
        fn negative_timestamps_do_not_convert(timestamp in any::<i64>().prop_filter(
            "negative values only",
            |value| *value < 0,
        )) {
            prop_assert_eq!(
                millis_to_nanos(timestamp),
                None,
                "negative millisecond timestamp should not convert"
            );
            prop_assert_eq!(
                micros_to_nanos(timestamp),
                None,
                "negative microsecond timestamp should not convert"
            );
        }

    }
}
