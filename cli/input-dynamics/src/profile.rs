//! Input profile parsing, provenance, and deterministic sampling.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{CliError, CliResult};
use crate::uinput::TapSpec;

pub(crate) const PROFILE_SCHEMA: &str = "input_dynamics_profile.v1";
const BASELINE_PROFILE_ID: &str = "baseline-v1";
const BASELINE_PROFILE_JSON: &str = include_str!("../../../profiles/baseline-v1.json");
const SHA256_PREFIX: &str = "sha256:";
const RATIO_SCALE_PPM: i64 = 1_000_000;
const RATIO_HALF_SCALE_PPM: i64 = 500_000;
const NORMAL_SAMPLE_COUNT: u64 = 6;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ProfileDefinition {
    schema: String,
    id: String,
    parameters: BTreeMap<String, ParameterDistribution>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ParameterDistribution {
    dist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stddev: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    choices: Option<Vec<WeightedChoice>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct WeightedChoice {
    value: i64,
    weight: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RuntimeProfile {
    source: ProfileSource,
    definition: ProfileDefinition,
    hash: String,
    seed: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProfileSource {
    Bundled,
    LocalFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProfileProvenance {
    pub(crate) source: ProfileSource,
    pub(crate) id: String,
    pub(crate) schema: String,
    pub(crate) hash: String,
    pub(crate) seed: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct KeyProfileContext {
    pub(crate) key_width_px: i32,
    pub(crate) key_height_px: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SampledTap {
    pub(crate) spec: TapSpec,
    pub(crate) sample: Option<ProfileTapSample>,
    pub(crate) inter_key_delay_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProfileTapSample {
    pub(crate) x_ratio_ppm: i64,
    pub(crate) y_ratio_ppm: i64,
    pub(crate) hold_ms: u64,
    pub(crate) pressure: i32,
    pub(crate) touch_major_px: i32,
    pub(crate) touch_minor_px: i32,
    pub(crate) orientation: i32,
}

#[derive(Clone, Debug)]
pub(crate) struct ProfileGenerator {
    runtime: RuntimeProfile,
    rng: DeterministicRng,
    sample_count: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InterKeyDelaySampling {
    Skip,
    Sample,
}

#[derive(Clone, Copy, Debug)]
struct DeterministicRng {
    state: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParameterName {
    TimingInterKeyDelayMs,
    TimingHoldMs,
    TapXRatioPpm,
    TapYRatioPpm,
    ContactPressure,
    ContactTouchMajorPx,
    ContactTouchMinorPx,
    ContactOrientation,
    MovementSamplesPerPress,
    MovementMaxDriftPx,
    GestureEdgeBackDurationMs,
}

pub(crate) fn load_for_session(
    input_actor: &str,
    input_controller: &str,
    input_profile_path: Option<&Path>,
    input_profile_seed: Option<u64>,
) -> CliResult<Option<RuntimeProfile>> {
    let actor = input_actor.trim();
    let controller = input_controller.trim();
    if actor == "human" && input_profile_path.is_none() {
        return Ok(None);
    }
    if actor == "human" && input_profile_path.is_some() {
        return Err(CliError::new(
            "input profiles are for controller-driven sessions; use a non-human input actor",
        ));
    }
    if controller.is_empty() && input_profile_path.is_none() {
        return Ok(None);
    }
    let seed = input_profile_seed.unwrap_or_else(new_seed);
    match input_profile_path {
        Some(path) => load_local_file(path, seed).map(Some),
        None => load_bundled_baseline(seed).map(Some),
    }
}

pub(crate) fn parse_runtime_json(text: &str) -> CliResult<RuntimeProfile> {
    let runtime = serde_json::from_str::<RuntimeProfile>(text)?;
    validate_profile(&runtime.definition)?;
    Ok(runtime)
}

pub(crate) fn runtime_json(runtime: &RuntimeProfile) -> CliResult<String> {
    Ok(serde_json::to_string(runtime)?)
}

impl RuntimeProfile {
    pub(crate) fn provenance(&self) -> ProfileProvenance {
        ProfileProvenance {
            source: self.source,
            id: self.definition.id.clone(),
            schema: self.definition.schema.clone(),
            hash: self.hash.clone(),
            seed: self.seed,
        }
    }

    pub(crate) fn summary_json(&self) -> Value {
        let provenance = self.provenance();
        json!({
            "source": provenance.source.as_str(),
            "id": provenance.id,
            "schema": provenance.schema,
            "hash": provenance.hash,
            "seed": provenance.seed,
            "parameter_count": self.definition.parameters.len(),
        })
    }
}

impl ProfileProvenance {
    pub(crate) fn broadcast_extras(&self) -> Vec<String> {
        vec![
            String::from("--es"),
            String::from("input_profile_source"),
            String::from(self.source.as_str()),
            String::from("--es"),
            String::from("input_profile_id"),
            self.id.clone(),
            String::from("--es"),
            String::from("input_profile_schema"),
            self.schema.clone(),
            String::from("--es"),
            String::from("input_profile_hash"),
            self.hash.clone(),
            String::from("--es"),
            String::from("input_profile_seed"),
            self.seed.to_string(),
        ]
    }
}

impl ProfileSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::LocalFile => "local_file",
        }
    }
}

impl ProfileGenerator {
    pub(crate) const fn new(runtime: RuntimeProfile) -> Self {
        Self {
            rng: DeterministicRng::new(runtime.seed),
            runtime,
            sample_count: 0,
        }
    }

    pub(crate) fn summary_json(&self) -> Value {
        let mut summary = self.runtime.summary_json();
        if let Some(object) = summary.as_object_mut() {
            object.insert(String::from("sample_count"), json!(self.sample_count));
        }
        summary
    }

    pub(crate) fn sample_tap(
        &mut self,
        fallback: TapSpec,
        key_context: Option<KeyProfileContext>,
        inter_key_delay_sampling: InterKeyDelaySampling,
    ) -> CliResult<SampledTap> {
        let Some(context) = key_context else {
            return Ok(SampledTap {
                spec: fallback,
                sample: None,
                inter_key_delay_ms: None,
            });
        };
        let x_ratio_ppm =
            self.sample_parameter(ParameterName::TapXRatioPpm, RATIO_HALF_SCALE_PPM)?;
        let y_ratio_ppm =
            self.sample_parameter(ParameterName::TapYRatioPpm, RATIO_HALF_SCALE_PPM)?;
        let hold_ms = i64_to_u64(
            self.sample_parameter(
                ParameterName::TimingHoldMs,
                i64::try_from(fallback.hold_ms).map_err(|error| {
                    CliError::new(format!("hold duration conversion failed: {error}"))
                })?,
            )?,
            "timing.hold_ms",
        )?;
        let pressure = i64_to_i32(
            self.sample_parameter(ParameterName::ContactPressure, i64::from(fallback.pressure))?,
            "contact.pressure",
        )?;
        let touch_major_px = i64_to_i32(
            self.sample_parameter(
                ParameterName::ContactTouchMajorPx,
                i64::from(fallback.touch_major_px),
            )?,
            "contact.touch_major_px",
        )?;
        let touch_minor_px = i64_to_i32(
            self.sample_parameter(
                ParameterName::ContactTouchMinorPx,
                i64::from(fallback.touch_minor_px),
            )?,
            "contact.touch_minor_px",
        )?;
        let orientation = i64_to_i32(
            self.sample_parameter(
                ParameterName::ContactOrientation,
                i64::from(fallback.orientation),
            )?,
            "contact.orientation",
        )?;
        let inter_key_delay_ms = match inter_key_delay_sampling {
            InterKeyDelaySampling::Sample => Some(i64_to_u64(
                self.sample_parameter(ParameterName::TimingInterKeyDelayMs, 0_i64)?,
                "timing.inter_key_delay_ms",
            )?),
            InterKeyDelaySampling::Skip => None,
        };
        let x = coordinate_from_ratio(fallback.x, context.key_width_px, x_ratio_ppm, "x")?;
        let y = coordinate_from_ratio(fallback.y, context.key_height_px, y_ratio_ppm, "y")?;
        self.sample_count = self
            .sample_count
            .checked_add(1)
            .ok_or_else(|| CliError::new("input profile sample count overflowed"))?;
        Ok(SampledTap {
            spec: TapSpec {
                x,
                y,
                hold_ms,
                pressure,
                touch_major_px,
                touch_minor_px,
                orientation,
            },
            sample: Some(ProfileTapSample {
                x_ratio_ppm,
                y_ratio_ppm,
                hold_ms,
                pressure,
                touch_major_px,
                touch_minor_px,
                orientation,
            }),
            inter_key_delay_ms,
        })
    }

    fn sample_parameter(&mut self, name: ParameterName, fallback: i64) -> CliResult<i64> {
        let Some(distribution) = self
            .runtime
            .definition
            .parameters
            .get(name.as_str())
            .cloned()
        else {
            return Ok(fallback);
        };
        distribution.sample(&mut self.rng, name.as_str())
    }
}

impl ProfileTapSample {
    pub(crate) fn json(self) -> Value {
        json!({
            "x_ratio_ppm": self.x_ratio_ppm,
            "y_ratio_ppm": self.y_ratio_ppm,
            "hold_ms": self.hold_ms,
            "pressure": self.pressure,
            "touch_major_px": self.touch_major_px,
            "touch_minor_px": self.touch_minor_px,
            "orientation": self.orientation,
        })
    }
}

impl ParameterDistribution {
    fn validate(&self, name: &str) -> CliResult<()> {
        match self.dist.as_str() {
            "fixed" => {
                self.required_value(name)?;
                Ok(())
            }
            "uniform" | "integer_uniform" => {
                let (min, max) = self.required_min_max(name)?;
                ensure_min_max(name, min, max)
            }
            "normal_with_bounds" => {
                let mean = required_i64(self.mean, name, "mean")?;
                let stddev = required_i64(self.stddev, name, "stddev")?;
                let (min, max) = self.required_min_max(name)?;
                ensure_min_max(name, min, max)?;
                if stddev < 0 {
                    return Err(CliError::new(format!("{name} stddev must be non-negative")));
                }
                if mean < min || mean > max {
                    return Err(CliError::new(format!(
                        "{name} mean must be inside min..max"
                    )));
                }
                Ok(())
            }
            "weighted_choice" => {
                let choices = self.choices.as_ref().ok_or_else(|| {
                    CliError::new(format!("{name} weighted_choice requires choices"))
                })?;
                if choices.is_empty() {
                    return Err(CliError::new(format!(
                        "{name} weighted_choice requires at least one choice"
                    )));
                }
                let mut total = 0_u64;
                for choice in choices {
                    if choice.weight == 0 {
                        return Err(CliError::new(format!(
                            "{name} weighted_choice weights must be positive"
                        )));
                    }
                    total = total.checked_add(choice.weight).ok_or_else(|| {
                        CliError::new(format!("{name} weighted_choice weight total overflowed"))
                    })?;
                }
                if total == 0 {
                    return Err(CliError::new(format!(
                        "{name} weighted_choice total weight must be positive"
                    )));
                }
                Ok(())
            }
            _ => Err(CliError::new(format!(
                "{name} uses unsupported distribution {}",
                self.dist
            ))),
        }
    }

    fn sample(&self, rng: &mut DeterministicRng, name: &str) -> CliResult<i64> {
        match self.dist.as_str() {
            "fixed" => self.required_value(name),
            "uniform" | "integer_uniform" => {
                let (min, max) = self.required_min_max(name)?;
                rng.integer_range(min, max)
            }
            "normal_with_bounds" => {
                let mean = required_i64(self.mean, name, "mean")?;
                let stddev = required_i64(self.stddev, name, "stddev")?;
                let (min, max) = self.required_min_max(name)?;
                sample_normal_with_bounds(rng, mean, stddev, min, max)
            }
            "weighted_choice" => self.sample_weighted_choice(rng, name),
            _ => Err(CliError::new(format!(
                "{name} uses unsupported distribution {}",
                self.dist
            ))),
        }
    }

    fn required_value(&self, name: &str) -> CliResult<i64> {
        required_i64(self.value, name, "value")
    }

    fn required_min_max(&self, name: &str) -> CliResult<(i64, i64)> {
        Ok((
            required_i64(self.min, name, "min")?,
            required_i64(self.max, name, "max")?,
        ))
    }

    fn sample_weighted_choice(&self, rng: &mut DeterministicRng, name: &str) -> CliResult<i64> {
        let choices = self
            .choices
            .as_ref()
            .ok_or_else(|| CliError::new(format!("{name} weighted_choice requires choices")))?;
        let total = choices.iter().try_fold(0_u64, |accumulator, choice| {
            accumulator.checked_add(choice.weight).ok_or_else(|| {
                CliError::new(format!("{name} weighted_choice weight total overflowed"))
            })
        })?;
        let mut selected = rng.u64_below(total)?;
        for choice in choices {
            if selected < choice.weight {
                return Ok(choice.value);
            }
            selected = selected.checked_sub(choice.weight).ok_or_else(|| {
                CliError::new(format!("{name} weighted_choice selection underflowed"))
            })?;
        }
        Err(CliError::new(format!(
            "{name} weighted_choice selection failed"
        )))
    }

    fn representative_values(&self) -> Vec<i64> {
        let mut values = Vec::new();
        if let Some(value) = self.value {
            values.push(value);
        }
        if let Some(value) = self.min {
            values.push(value);
        }
        if let Some(value) = self.max {
            values.push(value);
        }
        if let Some(value) = self.mean {
            values.push(value);
        }
        if let Some(choices) = self.choices.as_ref() {
            values.extend(choices.iter().map(|choice| choice.value));
        }
        values
    }
}

impl DeterministicRng {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn u64_below(&mut self, upper_exclusive: u64) -> CliResult<u64> {
        if upper_exclusive == 0 {
            return Err(CliError::new("random upper bound must be positive"));
        }
        self.next_u64()
            .checked_rem(upper_exclusive)
            .ok_or_else(|| CliError::new("random remainder failed"))
    }

    fn integer_range(&mut self, min: i64, max: i64) -> CliResult<i64> {
        ensure_min_max("range", min, max)?;
        let span_i128 = i128::from(max)
            .checked_sub(i128::from(min))
            .and_then(|delta| delta.checked_add(1))
            .ok_or_else(|| CliError::new("integer range span overflowed"))?;
        let span = u64::try_from(span_i128).map_err(|error| {
            CliError::new(format!("integer range span conversion failed: {error}"))
        })?;
        let offset = i64::try_from(self.u64_below(span)?).map_err(|error| {
            CliError::new(format!("integer range offset conversion failed: {error}"))
        })?;
        min.checked_add(offset)
            .ok_or_else(|| CliError::new("integer range addition overflowed"))
    }
}

impl ParameterName {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TimingInterKeyDelayMs => "timing.inter_key_delay_ms",
            Self::TimingHoldMs => "timing.hold_ms",
            Self::TapXRatioPpm => "tap.x_ratio_ppm",
            Self::TapYRatioPpm => "tap.y_ratio_ppm",
            Self::ContactPressure => "contact.pressure",
            Self::ContactTouchMajorPx => "contact.touch_major_px",
            Self::ContactTouchMinorPx => "contact.touch_minor_px",
            Self::ContactOrientation => "contact.orientation",
            Self::MovementSamplesPerPress => "movement.samples_per_press",
            Self::MovementMaxDriftPx => "movement.max_drift_px",
            Self::GestureEdgeBackDurationMs => "gesture.edge_back.duration_ms",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "timing.inter_key_delay_ms" => Some(Self::TimingInterKeyDelayMs),
            "timing.hold_ms" => Some(Self::TimingHoldMs),
            "tap.x_ratio_ppm" => Some(Self::TapXRatioPpm),
            "tap.y_ratio_ppm" => Some(Self::TapYRatioPpm),
            "contact.pressure" => Some(Self::ContactPressure),
            "contact.touch_major_px" => Some(Self::ContactTouchMajorPx),
            "contact.touch_minor_px" => Some(Self::ContactTouchMinorPx),
            "contact.orientation" => Some(Self::ContactOrientation),
            "movement.samples_per_press" => Some(Self::MovementSamplesPerPress),
            "movement.max_drift_px" => Some(Self::MovementMaxDriftPx),
            "gesture.edge_back.duration_ms" => Some(Self::GestureEdgeBackDurationMs),
            _ => None,
        }
    }
}

fn load_bundled_baseline(seed: u64) -> CliResult<RuntimeProfile> {
    let definition = parse_profile_definition(BASELINE_PROFILE_JSON.as_bytes())?;
    if definition.id != BASELINE_PROFILE_ID {
        return Err(CliError::new("bundled baseline profile id mismatch"));
    }
    Ok(RuntimeProfile {
        hash: profile_hash(&definition)?,
        source: ProfileSource::Bundled,
        definition,
        seed,
    })
}

fn load_local_file(path: &Path, seed: u64) -> CliResult<RuntimeProfile> {
    let bytes = fs::read(path)?;
    let definition = parse_profile_definition(&bytes)?;
    Ok(RuntimeProfile {
        hash: profile_hash(&definition)?,
        source: ProfileSource::LocalFile,
        definition,
        seed,
    })
}

fn parse_profile_definition(bytes: &[u8]) -> CliResult<ProfileDefinition> {
    let definition = serde_json::from_slice::<ProfileDefinition>(bytes)?;
    validate_profile(&definition)?;
    Ok(definition)
}

fn validate_profile(definition: &ProfileDefinition) -> CliResult<()> {
    if definition.schema != PROFILE_SCHEMA {
        return Err(CliError::new(format!(
            "unsupported input profile schema {}; expected {PROFILE_SCHEMA}",
            definition.schema
        )));
    }
    if definition.id.trim().is_empty() {
        return Err(CliError::new("input profile id must not be empty"));
    }
    for (name, distribution) in &definition.parameters {
        if ParameterName::parse(name).is_none() {
            return Err(CliError::new(format!(
                "unknown input profile parameter {name}"
            )));
        }
        distribution.validate(name)?;
        validate_parameter_value_bounds(name, distribution)?;
    }
    Ok(())
}

fn validate_parameter_value_bounds(
    name: &str,
    distribution: &ParameterDistribution,
) -> CliResult<()> {
    match ParameterName::parse(name) {
        Some(ParameterName::TapXRatioPpm | ParameterName::TapYRatioPpm) => {
            validate_distribution_bounds(name, distribution, 0, RATIO_SCALE_PPM)
        }
        Some(
            ParameterName::TimingInterKeyDelayMs
            | ParameterName::TimingHoldMs
            | ParameterName::ContactPressure
            | ParameterName::ContactTouchMajorPx
            | ParameterName::ContactTouchMinorPx
            | ParameterName::MovementSamplesPerPress
            | ParameterName::MovementMaxDriftPx
            | ParameterName::GestureEdgeBackDurationMs,
        ) => validate_distribution_bounds(name, distribution, 0, i64::MAX),
        Some(ParameterName::ContactOrientation) => Ok(()),
        None => Err(CliError::new(format!(
            "unknown input profile parameter {name}"
        ))),
    }
}

fn validate_distribution_bounds(
    name: &str,
    distribution: &ParameterDistribution,
    lower: i64,
    upper: i64,
) -> CliResult<()> {
    for value in distribution.representative_values() {
        if value < lower || value > upper {
            return Err(CliError::new(format!(
                "{name} value {value} is outside {lower}..{upper}"
            )));
        }
    }
    Ok(())
}

fn profile_hash(definition: &ProfileDefinition) -> CliResult<String> {
    let canonical = serde_json::to_vec(definition)?;
    let digest = Sha256::digest(canonical);
    Ok(format!("{SHA256_PREFIX}{}", hex_encode(&digest)))
}

fn sample_normal_with_bounds(
    rng: &mut DeterministicRng,
    mean: i64,
    stddev: i64,
    min: i64,
    max: i64,
) -> CliResult<i64> {
    ensure_min_max("normal_with_bounds", min, max)?;
    if stddev == 0 {
        return Ok(mean.clamp(min, max));
    }
    let mut total = 0_i64;
    let low = stddev
        .checked_neg()
        .ok_or_else(|| CliError::new("stddev negation overflowed"))?;
    for _ in 0..NORMAL_SAMPLE_COUNT {
        total = total
            .checked_add(rng.integer_range(low, stddev)?)
            .ok_or_else(|| CliError::new("normal sample accumulation overflowed"))?;
    }
    let average = total
        .checked_div(i64::try_from(NORMAL_SAMPLE_COUNT).map_err(|error| {
            CliError::new(format!("normal sample count conversion failed: {error}"))
        })?)
        .ok_or_else(|| CliError::new("normal sample average failed"))?;
    mean.checked_add(average)
        .ok_or_else(|| CliError::new("normal sample mean addition overflowed"))
        .map(|value| value.clamp(min, max))
}

fn coordinate_from_ratio(center: i32, size: i32, ratio_ppm: i64, label: &str) -> CliResult<i32> {
    if size <= 0_i32 {
        return Err(CliError::new(format!("{label} key size must be positive")));
    }
    if !(0..=RATIO_SCALE_PPM).contains(&ratio_ppm) {
        return Err(CliError::new(format!(
            "{label} ratio {ratio_ppm} ppm is outside 0..{RATIO_SCALE_PPM}"
        )));
    }
    let half = i64::from(size)
        .checked_div(2)
        .ok_or_else(|| CliError::new(format!("{label} size division failed")))?;
    let left = i64::from(center)
        .checked_sub(half)
        .ok_or_else(|| CliError::new(format!("{label} left coordinate underflowed")))?;
    let scaled = i64::from(size)
        .checked_mul(ratio_ppm)
        .and_then(|value| value.checked_add(RATIO_HALF_SCALE_PPM))
        .and_then(|value| value.checked_div(RATIO_SCALE_PPM))
        .ok_or_else(|| CliError::new(format!("{label} ratio scaling failed")))?;
    let coordinate = left
        .checked_add(scaled)
        .ok_or_else(|| CliError::new(format!("{label} coordinate addition overflowed")))?;
    i32::try_from(coordinate)
        .map_err(|error| CliError::new(format!("{label} coordinate conversion failed: {error}")))
}

fn required_i64(value: Option<i64>, name: &str, field: &str) -> CliResult<i64> {
    value.ok_or_else(|| CliError::new(format!("{name} requires {field}")))
}

fn ensure_min_max(name: &str, min: i64, max: i64) -> CliResult<()> {
    if min > max {
        return Err(CliError::new(format!("{name} min must be <= max")));
    }
    Ok(())
}

fn i64_to_u64(value: i64, name: &str) -> CliResult<u64> {
    u64::try_from(value)
        .map_err(|error| CliError::new(format!("{name} value {value} is invalid: {error}")))
}

fn i64_to_i32(value: i64, name: &str) -> CliResult<i32> {
    i32::try_from(value)
        .map_err(|error| CliError::new(format!("{name} value {value} is invalid: {error}")))
}

fn new_seed() -> u64 {
    let wall_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_nanos());
    let wall_low = u64::try_from(wall_nanos & u128::from(u64::MAX)).unwrap_or(0);
    wall_low ^ u64::from(process::id()).rotate_left(32)
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
    use std::path::Path;

    use proptest::prelude::{Just, any};
    use proptest::prop_assert;

    use crate::profile::{
        BASELINE_PROFILE_JSON, DeterministicRng, InterKeyDelaySampling, KeyProfileContext,
        PROFILE_SCHEMA, ProfileGenerator, ProfileSource, RuntimeProfile, load_for_session,
        parse_profile_definition, sample_normal_with_bounds,
    };
    use crate::uinput::TapSpec;

    #[test]
    fn bundled_baseline_profile_loads_with_neutral_provenance() {
        let loaded = load_for_session("agent_adb", "input-dynamics-cli", None, Some(42));

        assert!(loaded.is_ok(), "bundled baseline should load");
        let runtime = loaded.ok().flatten();
        assert!(
            runtime.is_some(),
            "agent-controlled session should get a default profile"
        );
        let Some(provenance) = runtime.as_ref().map(RuntimeProfile::provenance) else {
            assert!(runtime.is_some(), "runtime profile should be present");
            return;
        };
        assert_eq!(provenance.source, ProfileSource::Bundled);
        assert_eq!(provenance.id, "baseline-v1");
        assert_eq!(provenance.schema, PROFILE_SCHEMA);
        assert_eq!(provenance.seed, 42);
        assert!(
            provenance.hash.starts_with("sha256:"),
            "profile hash should include algorithm prefix"
        );
    }

    #[test]
    fn human_session_without_profile_is_profile_free() {
        let loaded = load_for_session("human", "", None, Some(42));

        assert!(loaded.is_ok(), "human observation should not fail");
        assert!(
            loaded.ok().flatten().is_none(),
            "human observation should not receive an input profile"
        );
    }

    #[test]
    fn parser_rejects_unknown_parameters() {
        let text = format!(
            r#"{{
                "schema":"{PROFILE_SCHEMA}",
                "id":"custom",
                "parameters":{{
                    "unknown.parameter":{{"dist":"fixed","value":1}}
                }}
            }}"#
        );

        let parsed = parse_profile_definition(text.as_bytes());

        assert!(parsed.is_err(), "unknown parameters should fail validation");
    }

    #[test]
    fn parser_rejects_bad_schema() {
        let text = r#"{"schema":"bad","id":"custom","parameters":{}}"#;

        let parsed = parse_profile_definition(text.as_bytes());

        assert!(parsed.is_err(), "unsupported schemas should fail");
    }

    #[test]
    fn same_seed_reproduces_profiled_tap() {
        let loaded = load_for_session("agent_adb", "input-dynamics-cli", None, Some(7));
        assert!(loaded.is_ok(), "profile should load");
        let runtime = loaded.ok().flatten();
        if runtime.is_none() {
            assert!(runtime.is_some(), "profile should be present");
            return;
        }
        let Some(runtime_profile) = runtime else {
            return;
        };
        let mut left = ProfileGenerator::new(runtime_profile.clone());
        let mut right = ProfileGenerator::new(runtime_profile);
        let context = Some(KeyProfileContext {
            key_width_px: 100,
            key_height_px: 80,
        });

        let left_sample = left.sample_tap(
            TapSpec::new(500, 700),
            context,
            InterKeyDelaySampling::Sample,
        );
        let right_sample = right.sample_tap(
            TapSpec::new(500, 700),
            context,
            InterKeyDelaySampling::Sample,
        );

        assert!(left_sample.is_ok(), "left sample should succeed");
        assert!(right_sample.is_ok(), "right sample should succeed");
        assert_eq!(
            left_sample.ok(),
            right_sample.ok(),
            "same profile and seed should reproduce the sample"
        );
    }

    #[test]
    fn bundled_profile_json_is_valid() {
        let parsed = parse_profile_definition(BASELINE_PROFILE_JSON.as_bytes());

        assert!(parsed.is_ok(), "bundled profile JSON should validate");
    }

    proptest::proptest! {
        #[test]
        fn bounded_normal_samples_stay_inside_bounds(
            seed in any::<u64>(),
            mean in Just(50_i64),
            stddev in 0_i64..100_i64,
            min in Just(0_i64),
            max in Just(100_i64),
        ) {
            let mut rng = DeterministicRng::new(seed);
            let sampled = sample_normal_with_bounds(&mut rng, mean, stddev, min, max);

            prop_assert!(sampled.is_ok(), "sample should succeed");
            let value = sampled.unwrap_or(min);
            prop_assert!(value >= min, "sample should respect lower bound");
            prop_assert!(value <= max, "sample should respect upper bound");
        }
    }

    #[test]
    fn explicit_profile_rejected_for_human_actor() {
        let loaded = load_for_session(
            "human",
            "input-dynamics-cli",
            Some(Path::new("profile.json")),
            Some(42),
        );

        assert!(
            loaded.is_err(),
            "human actor should not accept an input profile"
        );
    }
}
