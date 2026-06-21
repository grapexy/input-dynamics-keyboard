//! Fixed-point ratio parsing for reproducible CLI geometry.

use std::str::FromStr;

use crate::error::{CliError, CliResult};

pub(crate) const RATIO_SCALE_PPM: u32 = 1_000_000;
const RATIO_SCALE_I64: i64 = 1_000_000;
const FRACTION_WEIGHTS: [u32; 6] = [100_000, 10_000, 1_000, 100, 10, 1];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RatioPpm(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SignedRatioPpm(i32);

impl RatioPpm {
    pub(crate) const fn from_ppm(ppm: u32) -> Self {
        Self(ppm)
    }

    pub(crate) const fn ppm(self) -> u32 {
        self.0
    }

    pub(crate) fn checked_add(self, other: Self) -> CliResult<Self> {
        let Some(ppm) = self.0.checked_add(other.0) else {
            return Err(CliError::new("ratio addition overflowed"));
        };
        Self::from_checked_ppm(ppm)
    }

    pub(crate) fn checked_subtract(self, other: Self) -> CliResult<Self> {
        let Some(ppm) = self.0.checked_sub(other.0) else {
            return Err(CliError::new("ratio subtraction went below zero"));
        };
        Self::from_checked_ppm(ppm)
    }

    pub(crate) fn checked_add_signed(self, other: SignedRatioPpm) -> CliResult<Self> {
        let value = i64::from(self.0)
            .checked_add(i64::from(other.ppm()))
            .ok_or_else(|| CliError::new("signed ratio addition overflowed"))?;
        if !(0_i64..=RATIO_SCALE_I64).contains(&value) {
            return Err(CliError::new(format!(
                "ratio result {value} ppm is outside 0..{RATIO_SCALE_PPM}"
            )));
        }
        let ppm = u32::try_from(value)
            .map_err(|error| CliError::new(format!("ratio result conversion failed: {error}")))?;
        Ok(Self(ppm))
    }

    fn from_checked_ppm(ppm: u32) -> CliResult<Self> {
        if ppm > RATIO_SCALE_PPM {
            return Err(CliError::new(format!(
                "ratio {ppm} ppm is outside 0..{RATIO_SCALE_PPM}"
            )));
        }
        Ok(Self(ppm))
    }
}

impl SignedRatioPpm {
    pub(crate) const fn from_ppm(ppm: i32) -> Self {
        Self(ppm)
    }

    pub(crate) const fn ppm(self) -> i32 {
        self.0
    }
}

impl FromStr for RatioPpm {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        parse_ratio_ppm(text).map(Self)
    }
}

impl FromStr for SignedRatioPpm {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let trimmed = text.trim();
        let Some(unsigned_text) = trimmed.strip_prefix('-') else {
            return parse_ratio_ppm(trimmed).and_then(signed_from_u32).map(Self);
        };
        parse_ratio_ppm(unsigned_text)
            .and_then(signed_from_u32)
            .and_then(|ppm| {
                ppm.checked_neg()
                    .ok_or_else(|| String::from("signed ratio negation overflowed"))
            })
            .map(Self)
    }
}

fn signed_from_u32(value: u32) -> Result<i32, String> {
    i32::try_from(value).map_err(|error| format!("ratio is too large: {error}"))
}

fn parse_ratio_ppm(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(String::from("ratio must not be empty"));
    }
    let (whole_text, fraction_text) = trimmed.split_once('.').map_or((trimmed, ""), |parts| parts);
    let whole = parse_whole_ratio(whole_text)?;
    let fraction = parse_fraction_ppm(fraction_text)?;
    let whole_ppm = whole
        .checked_mul(RATIO_SCALE_PPM)
        .ok_or_else(|| String::from("ratio whole-part multiplication overflowed"))?;
    let ppm = whole_ppm
        .checked_add(fraction)
        .ok_or_else(|| String::from("ratio ppm addition overflowed"))?;
    if ppm > RATIO_SCALE_PPM {
        return Err(format!(
            "ratio {trimmed} is outside supported range 0.0..1.0"
        ));
    }
    Ok(ppm)
}

fn parse_whole_ratio(text: &str) -> Result<u32, String> {
    if text.is_empty() {
        return Ok(0_u32);
    }
    text.parse::<u32>()
        .map_err(|error| format!("invalid ratio whole part {text}: {error}"))
}

fn parse_fraction_ppm(text: &str) -> Result<u32, String> {
    if text.len() > FRACTION_WEIGHTS.len() {
        return Err(format!(
            "ratio fraction supports at most {} decimal places",
            FRACTION_WEIGHTS.len()
        ));
    }
    let mut ppm = 0_u32;
    for (character, weight) in text.chars().zip(FRACTION_WEIGHTS) {
        let digit = character
            .to_digit(10)
            .ok_or_else(|| format!("invalid ratio fraction digit {character}"))?;
        let weighted = digit
            .checked_mul(weight)
            .ok_or_else(|| String::from("ratio fraction multiplication overflowed"))?;
        ppm = ppm
            .checked_add(weighted)
            .ok_or_else(|| String::from("ratio fraction addition overflowed"))?;
    }
    Ok(ppm)
}

#[cfg(test)]
mod tests {
    use crate::ratio::{RatioPpm, SignedRatioPpm};

    #[test]
    fn parses_decimal_ratio_to_ppm() {
        assert_eq!("0".parse::<RatioPpm>().ok().map(RatioPpm::ppm), Some(0));
        assert_eq!(
            "0.5".parse::<RatioPpm>().ok().map(RatioPpm::ppm),
            Some(500_000)
        );
        assert_eq!(
            ".125".parse::<RatioPpm>().ok().map(RatioPpm::ppm),
            Some(125_000)
        );
        assert_eq!(
            "1.0".parse::<RatioPpm>().ok().map(RatioPpm::ppm),
            Some(1_000_000)
        );
    }

    #[test]
    fn rejects_out_of_range_ratio() {
        assert!(
            "1.000001".parse::<RatioPpm>().is_err(),
            "ratios above one should be rejected"
        );
        assert!(
            "0.1234567".parse::<RatioPpm>().is_err(),
            "more than six decimal places should be rejected"
        );
    }

    #[test]
    fn parses_signed_ratio() {
        assert_eq!(
            "-0.02"
                .parse::<SignedRatioPpm>()
                .ok()
                .map(SignedRatioPpm::ppm),
            Some(-20_000_i32)
        );
        assert_eq!(
            "0.02"
                .parse::<SignedRatioPpm>()
                .ok()
                .map(SignedRatioPpm::ppm),
            Some(20_000_i32)
        );
    }
}
