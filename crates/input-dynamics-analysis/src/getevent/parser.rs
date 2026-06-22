use crate::getevent::{NormalizeError, NormalizeResult};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParsedLine {
    Blank,
    DeviceAdded(DeviceAdded),
    DeviceName(String),
    InputEvent(InputEvent),
    Unparsed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeviceAdded {
    pub(crate) device_index: u64,
    pub(crate) event_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InputEvent {
    pub(crate) timestamp: EventTimestamp,
    pub(crate) event_path: String,
    pub(crate) event_type: String,
    pub(crate) code: String,
    pub(crate) value: EventValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EventTimestamp {
    pub(crate) text: String,
    pub(crate) micros: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EventValue {
    pub(crate) raw: String,
    pub(crate) integer: Option<i64>,
    pub(crate) key_state: Option<String>,
}

pub(crate) fn parse_line(line: &str) -> NormalizeResult<ParsedLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(ParsedLine::Blank);
    }
    if let Some(device) = parse_device_added(trimmed)? {
        return Ok(ParsedLine::DeviceAdded(device));
    }
    if let Some(name) = parse_device_name(trimmed) {
        return Ok(ParsedLine::DeviceName(name));
    }
    if let Some(event) = parse_input_event(trimmed)? {
        return Ok(ParsedLine::InputEvent(event));
    }
    Ok(ParsedLine::Unparsed(line.to_owned()))
}

fn parse_device_added(line: &str) -> NormalizeResult<Option<DeviceAdded>> {
    let Some(rest) = line.strip_prefix("add device ") else {
        return Ok(None);
    };
    let Some((index_text, path_text)) = rest.split_once(':') else {
        return Ok(None);
    };
    let device_index = index_text
        .trim()
        .parse::<u64>()
        .map_err(|error| NormalizeError::new(format!("invalid device index: {error}")))?;
    let event_path = path_text.trim().to_owned();
    if event_path.is_empty() {
        return Ok(None);
    }
    Ok(Some(DeviceAdded {
        device_index,
        event_path,
    }))
}

fn parse_device_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("name:")?.trim();
    let unquoted = rest
        .strip_prefix('"')
        .and_then(|without_prefix| without_prefix.strip_suffix('"'))
        .unwrap_or(rest);
    Some(unquoted.to_owned())
}

fn parse_input_event(line: &str) -> NormalizeResult<Option<InputEvent>> {
    let Some(rest) = line.strip_prefix('[') else {
        return Ok(None);
    };
    let Some((timestamp_text, event_text)) = rest.split_once(']') else {
        return Ok(None);
    };
    let mut fields = event_text.split_whitespace();
    let Some(event_path_with_colon) = fields.next() else {
        return Ok(None);
    };
    let Some(event_path) = event_path_with_colon.strip_suffix(':') else {
        return Ok(None);
    };
    let Some(event_type) = fields.next() else {
        return Ok(None);
    };
    let Some(code) = fields.next() else {
        return Ok(None);
    };
    let Some(value_text) = fields.next() else {
        return Ok(None);
    };
    Ok(Some(InputEvent {
        timestamp: parse_timestamp(timestamp_text.trim())?,
        event_path: event_path.to_owned(),
        event_type: event_type.to_owned(),
        code: code.to_owned(),
        value: parse_value(value_text)?,
    }))
}

fn parse_timestamp(text: &str) -> NormalizeResult<EventTimestamp> {
    let micros = if let Some((seconds_text, fraction_text)) = text.split_once('.') {
        let seconds = seconds_text
            .trim()
            .parse::<u64>()
            .map_err(|error| NormalizeError::new(format!("invalid getevent seconds: {error}")))?;
        let fraction = parse_micro_fraction(fraction_text)?;
        Some(
            seconds
                .checked_mul(1_000_000)
                .and_then(|value| value.checked_add(fraction))
                .ok_or_else(|| NormalizeError::new("getevent timestamp overflow"))?,
        )
    } else {
        None
    };
    Ok(EventTimestamp {
        text: text.to_owned(),
        micros,
    })
}

fn parse_micro_fraction(text: &str) -> NormalizeResult<u64> {
    let digits = text.trim();
    if digits.is_empty() {
        return Ok(0);
    }
    let mut value = 0_u64;
    let mut count = 0_u8;
    for character in digits.chars() {
        if count >= 6 {
            break;
        }
        let Some(digit) = character.to_digit(10) else {
            return Err(NormalizeError::new("invalid getevent fractional timestamp"));
        };
        value = value
            .checked_mul(10)
            .and_then(|current| current.checked_add(u64::from(digit)))
            .ok_or_else(|| NormalizeError::new("getevent fractional timestamp overflow"))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| NormalizeError::new("getevent fractional digit count overflow"))?;
    }
    while count < 6 {
        value = value
            .checked_mul(10)
            .ok_or_else(|| NormalizeError::new("getevent fractional timestamp overflow"))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| NormalizeError::new("getevent fractional digit count overflow"))?;
    }
    Ok(value)
}

fn parse_value(text: &str) -> NormalizeResult<EventValue> {
    let integer = if is_hex_word(text) {
        Some(parse_hex_i32ish(text)?)
    } else {
        text.parse::<i64>().ok()
    };
    let key_state = match text {
        "DOWN" | "UP" => Some(text.to_owned()),
        _ => None,
    };
    Ok(EventValue {
        raw: text.to_owned(),
        integer,
        key_state,
    })
}

fn is_hex_word(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|character| character.is_ascii_hexdigit())
}

fn parse_hex_i32ish(text: &str) -> NormalizeResult<i64> {
    let unsigned = u64::from_str_radix(text, 16)
        .map_err(|error| NormalizeError::new(format!("invalid getevent hex value: {error}")))?;
    if text.len() == 8 && unsigned >= 0x8000_0000 {
        let signed = i64::try_from(unsigned)
            .map_err(|error| NormalizeError::new(format!("getevent hex value overflow: {error}")))?
            .checked_sub(0x1_0000_0000)
            .ok_or_else(|| NormalizeError::new("getevent signed value overflow"))?;
        return Ok(signed);
    }
    i64::try_from(unsigned)
        .map_err(|error| NormalizeError::new(format!("getevent value overflow: {error}")))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{Just, any};
    use proptest::prop_assert_eq;

    use crate::getevent::parser::{ParsedLine, parse_line};

    #[test]
    fn parser_reads_input_event_line() {
        let line =
            "[  118158.858060] /dev/input/event4: EV_ABS       ABS_MT_POSITION_X    00000090";

        let parsed = parse_line(line);

        assert!(parsed.is_ok(), "event line should parse");
        assert!(
            matches!(parsed, Ok(ParsedLine::InputEvent(_))),
            "expected parsed input event"
        );
        if let Ok(ParsedLine::InputEvent(event)) = parsed {
            assert_eq!(event.timestamp.text, "118158.858060", "timestamp text");
            assert_eq!(
                event.timestamp.micros,
                Some(118_158_858_060),
                "timestamp micros"
            );
            assert_eq!(event.event_path, "/dev/input/event4", "event path");
            assert_eq!(event.event_type, "EV_ABS", "event type");
            assert_eq!(event.code, "ABS_MT_POSITION_X", "code");
            assert_eq!(event.value.integer, Some(144), "integer value");
        }
    }

    #[test]
    fn parser_reads_negative_tracking_id() {
        let line =
            "[  118158.858940] /dev/input/event4: EV_ABS       ABS_MT_TRACKING_ID   ffffffff";

        let parsed = parse_line(line);

        assert!(parsed.is_ok(), "event line should parse");
        assert!(
            matches!(parsed, Ok(ParsedLine::InputEvent(_))),
            "expected parsed input event"
        );
        if let Ok(ParsedLine::InputEvent(event)) = parsed {
            assert_eq!(event.value.integer, Some(-1), "tracking id release");
        }
    }

    proptest::proptest! {
        #[test]
        fn parser_preserves_generated_hex_word(value in any::<u16>()) {
            let line = format!(
                "[  1.000001] /dev/input/event0: EV_ABS ABS_X {value:08x}"
            );

            let parsed = parse_line(&line);

            prop_assert_eq!(parsed.is_ok(), true, "generated event line should parse");
            if let Ok(ParsedLine::InputEvent(event)) = parsed {
                prop_assert_eq!(event.value.integer, Some(i64::from(value)), "hex value should parse");
            } else {
                prop_assert_eq!(Some(()), None, "expected generated input event");
            }
        }

        #[test]
        fn non_event_text_is_preserved_as_unparsed(text in Just(String::from("not getevent output"))) {
            let parsed = parse_line(&text);

            prop_assert_eq!(parsed.is_ok(), true, "unparsed text should not fail");
            prop_assert_eq!(parsed.ok(), Some(ParsedLine::Unparsed(text)), "text should be preserved");
        }
    }
}
