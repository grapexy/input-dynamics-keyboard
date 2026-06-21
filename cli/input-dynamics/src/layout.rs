//! Keyboard layout matching helpers.

use serde_json::Value;

pub(crate) fn key_matches(value: &Value, label: Option<&str>, code: Option<i64>) -> bool {
    let label_matches = match label {
        Some(expected_label) => {
            value.get("key_label").and_then(Value::as_str) == Some(expected_label)
        }
        None => false,
    };
    let code_matches = match code {
        Some(expected_code) => value.get("key_code").and_then(Value::as_i64) == Some(expected_code),
        None => false,
    };
    label_matches || code_matches
}

pub(crate) fn json_number_to_shell_arg(value: &Value) -> Option<String> {
    value.as_i64().map_or_else(
        || value.as_f64().map(|number| format!("{number:.0}")),
        |number| Some(number.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use proptest::strategy::Strategy;
    use serde_json::json;

    use crate::layout::{json_number_to_shell_arg, key_matches};

    proptest::proptest! {
        #[test]
        fn layout_key_matching_accepts_label_or_code(
            label in non_empty_text(),
            candidate_label in non_empty_text(),
            code in any_i64(),
            candidate_code in any_i64(),
        ) {
            let key = json!({
                "key_label": label,
                "key_code": code
            });
            let expected_label_match = key
                .get("key_label")
                .and_then(serde_json::Value::as_str)
                == Some(candidate_label.as_str());
            let expected_code_match = key
                .get("key_code")
                .and_then(serde_json::Value::as_i64)
                == Some(candidate_code);

            proptest::prop_assert_eq!(
                key_matches(&key, Some(candidate_label.as_str()), None),
                expected_label_match,
                "label-only matching should be exact"
            );
            proptest::prop_assert_eq!(
                key_matches(&key, None, Some(candidate_code)),
                expected_code_match,
                "code-only matching should be exact"
            );
            proptest::prop_assert_eq!(
                key_matches(&key, Some(candidate_label.as_str()), Some(candidate_code)),
                expected_label_match || expected_code_match,
                "combined matching should accept label or code"
            );
        }

        #[test]
        fn integer_json_coordinate_round_trips(value in any_i64()) {
            let coordinate = json!(value);

            proptest::prop_assert_eq!(
                json_number_to_shell_arg(&coordinate),
                Some(value.to_string()),
                "integer coordinates should round-trip"
            );
        }

        #[test]
        fn finite_float_json_coordinate_formats_as_zero_decimal(value in finite_f64()) {
            let coordinate = json!(value);
            let expected = format!("{value:.0}");

            proptest::prop_assert_eq!(
                json_number_to_shell_arg(&coordinate),
                Some(expected),
                "float coordinates should match adb integer tap formatting"
            );
        }
    }

    fn non_empty_text() -> impl Strategy<Value = String> {
        ".{1,64}"
    }

    fn any_i64() -> impl Strategy<Value = i64> {
        i64::MIN..=i64::MAX
    }

    fn finite_f64() -> impl Strategy<Value = f64> {
        (-1_000_000_f64..=1_000_000_f64).prop_filter("finite f64", |value| value.is_finite())
    }
}
