//! Derivation policy parsing for recorded-run analysis.

use std::fs;
use std::path::Path;

use input_dynamics_analysis::derivation::{
    DEFAULT_DERIVATION_POLICY_JSON, DERIVATION_POLICY_SCHEMA, DismissalDerivationPolicy,
    default_derivation_policy,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{CliError, CliResult};

const SHA256_PREFIX: &str = "sha256:";
const POLICY_FIELD_COUNT: u64 = 6;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DerivationPolicyFile {
    schema: String,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge_ratio_ppm: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge_inward_ratio_ppm: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_edge_vertical_drift_ratio_ppm: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tap_max_distance_px: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tap_max_duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hide_correlation_window_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedDerivationPolicy {
    pub(crate) policy: DismissalDerivationPolicy,
    pub(crate) summary: Value,
}

pub(crate) fn load(policy_path: Option<&Path>) -> CliResult<LoadedDerivationPolicy> {
    match policy_path {
        Some(path) => load_local_file(path),
        None => load_bundled_default(),
    }
}

fn load_bundled_default() -> CliResult<LoadedDerivationPolicy> {
    let policy_file = serde_json::from_str::<DerivationPolicyFile>(DEFAULT_DERIVATION_POLICY_JSON)?;
    validate_policy_file(&policy_file)?;
    Ok(LoadedDerivationPolicy {
        policy: default_derivation_policy().map_err(CliError::from)?,
        summary: summary_json("bundled", &policy_file)?,
    })
}

fn loaded_defaults() -> CliResult<DismissalDerivationPolicy> {
    default_derivation_policy().map_err(CliError::from)
}

fn summary_json(source: &str, policy_file: &DerivationPolicyFile) -> CliResult<Value> {
    Ok(json!({
        "source": source,
        "schema": policy_file.schema,
        "id": policy_file.id,
        "hash": policy_hash(policy_file)?,
        "overridden_field_count": overridden_field_count(policy_file),
        "field_count": POLICY_FIELD_COUNT,
    }))
}

fn loaded_policy(
    source: &str,
    policy_file: &DerivationPolicyFile,
) -> CliResult<LoadedDerivationPolicy> {
    let defaults = loaded_defaults()?;
    let policy = policy_from_file(policy_file, defaults);
    validate_policy(policy)?;
    Ok(LoadedDerivationPolicy {
        policy,
        summary: summary_json(source, policy_file)?,
    })
}

fn load_local_file(path: &Path) -> CliResult<LoadedDerivationPolicy> {
    let bytes = fs::read(path)?;
    let policy_file = serde_json::from_slice::<DerivationPolicyFile>(&bytes)?;
    validate_policy_file(&policy_file)?;
    loaded_policy("local_file", &policy_file)
}

fn validate_policy_file(policy_file: &DerivationPolicyFile) -> CliResult<()> {
    if policy_file.schema != DERIVATION_POLICY_SCHEMA {
        return Err(CliError::new(format!(
            "unsupported derivation policy schema {}; expected {DERIVATION_POLICY_SCHEMA}",
            policy_file.schema
        )));
    }
    if policy_file.id.trim().is_empty() {
        return Err(CliError::new("derivation policy id must not be empty"));
    }
    Ok(())
}

fn policy_from_file(
    policy_file: &DerivationPolicyFile,
    defaults: DismissalDerivationPolicy,
) -> DismissalDerivationPolicy {
    DismissalDerivationPolicy {
        edge_ratio_ppm: policy_file
            .edge_ratio_ppm
            .unwrap_or(defaults.edge_ratio_ppm),
        edge_inward_ratio_ppm: policy_file
            .edge_inward_ratio_ppm
            .unwrap_or(defaults.edge_inward_ratio_ppm),
        max_edge_vertical_drift_ratio_ppm: policy_file
            .max_edge_vertical_drift_ratio_ppm
            .unwrap_or(defaults.max_edge_vertical_drift_ratio_ppm),
        tap_max_distance_px: policy_file
            .tap_max_distance_px
            .unwrap_or(defaults.tap_max_distance_px),
        tap_max_duration_ms: policy_file
            .tap_max_duration_ms
            .unwrap_or(defaults.tap_max_duration_ms),
        hide_correlation_window_ms: policy_file
            .hide_correlation_window_ms
            .unwrap_or(defaults.hide_correlation_window_ms),
    }
}

fn validate_policy(policy: DismissalDerivationPolicy) -> CliResult<()> {
    validate_ppm("edge_ratio_ppm", policy.edge_ratio_ppm)?;
    validate_ppm("edge_inward_ratio_ppm", policy.edge_inward_ratio_ppm)?;
    validate_ppm(
        "max_edge_vertical_drift_ratio_ppm",
        policy.max_edge_vertical_drift_ratio_ppm,
    )?;
    validate_non_negative("tap_max_distance_px", policy.tap_max_distance_px)?;
    validate_non_negative("tap_max_duration_ms", policy.tap_max_duration_ms)?;
    validate_non_negative(
        "hide_correlation_window_ms",
        policy.hide_correlation_window_ms,
    )
}

fn validate_ppm(name: &str, value: i64) -> CliResult<()> {
    if !(0..=1_000_000).contains(&value) {
        return Err(CliError::new(format!("{name} must be in 0..1000000")));
    }
    Ok(())
}

fn validate_non_negative(name: &str, value: i64) -> CliResult<()> {
    if value < 0 {
        return Err(CliError::new(format!("{name} must be non-negative")));
    }
    Ok(())
}

fn overridden_field_count(policy_file: &DerivationPolicyFile) -> u64 {
    [
        policy_file.edge_ratio_ppm,
        policy_file.edge_inward_ratio_ppm,
        policy_file.max_edge_vertical_drift_ratio_ppm,
        policy_file.tap_max_distance_px,
        policy_file.tap_max_duration_ms,
        policy_file.hide_correlation_window_ms,
    ]
    .iter()
    .filter(|field| field.is_some())
    .count()
    .try_into()
    .unwrap_or(POLICY_FIELD_COUNT)
}

fn policy_hash(policy_file: &DerivationPolicyFile) -> CliResult<String> {
    let canonical = serde_json::to_vec(policy_file)?;
    let digest = Sha256::digest(canonical);
    Ok(format!("{SHA256_PREFIX}{}", hex_encode(&digest)))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(hex_char(byte >> 4));
        encoded.push(hex_char(byte & 0x0f));
    }
    encoded
}

fn hex_char(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0'.saturating_add(nibble)),
        10..=15 => char::from(b'a'.saturating_add(nibble.saturating_sub(10))),
        _ => '0',
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use input_dynamics_analysis::derivation::default_derivation_policy;

    use crate::derivation_policy::{DERIVATION_POLICY_SCHEMA, load};

    #[test]
    fn bundled_policy_matches_analysis_default() {
        let loaded = load(None);

        assert!(loaded.is_ok(), "bundled derivation policy should load");
        let Some(policy) = loaded.ok() else {
            return;
        };
        let default = default_derivation_policy();
        assert!(default.is_ok(), "analysis default policy should load");
        assert_eq!(
            default.ok(),
            Some(policy.policy),
            "CLI bundled default should match analysis default"
        );
        assert_eq!(
            policy
                .summary
                .get("source")
                .and_then(serde_json::Value::as_str),
            Some("bundled"),
            "bundled policy should be explicitly identified"
        );
    }

    #[test]
    fn local_policy_overrides_selected_fields() {
        let path = std::env::temp_dir().join("input-dynamics-policy-test.json");
        let write = fs::write(
            &path,
            format!(
                r#"{{
                    "schema":"{DERIVATION_POLICY_SCHEMA}",
                    "id":"test-policy",
                    "edge_ratio_ppm":40000,
                    "tap_max_distance_px":41
                }}"#
            ),
        );
        assert!(write.is_ok(), "test policy file should be written");

        let loaded = load(Some(&path));
        let _remove = fs::remove_file(&path);

        assert!(loaded.is_ok(), "local derivation policy should load");
        let Some(policy) = loaded.ok() else {
            return;
        };
        assert_eq!(policy.policy.edge_ratio_ppm, 40_000);
        assert_eq!(policy.policy.tap_max_distance_px, 41);
        assert_eq!(
            policy
                .summary
                .get("overridden_field_count")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
    }
}
