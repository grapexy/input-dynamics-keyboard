# Keystroke Capture Plan

## Goal

Build an Android IME research mode that records close-to-millisecond human
typing dynamics from real soft-keyboard interaction.

## Event Sources

Use Android input timestamps from touch events:

- `MotionEvent.getEventTime()` for millisecond event time in uptime base
- `MotionEvent.getDownTime()` for the gesture start time
- `MotionEvent.getHistoricalEventTime()` for coalesced move history
- `MotionEvent.getPressure()` and `MotionEvent.getSize()` when available

On newer Android versions, evaluate nanosecond timestamps only as optional
metadata. Treat millisecond event time as the primary comparable signal.

## Event Types

Implemented base set:

- `session_start`
- `session_stop`
- `field_enter`
- `field_exit`
- `pointer_sample`
- `key_down`
- `key_up`
- `key_commit`
- `key_repeat`
- `key_long_press`
- `key_cancel`

Key records use `key_class` values such as `letter`, `digit`, `symbol`,
`space`, `enter`, `delete`, `modifier`, `action`, and `function`; they do not
store the raw key label, code point, output text, or surrounding field text.

## Fields

Recommended event fields:

- `schema`
- `session_id`
- `event`
- `t_wall_ms`
- `t_uptime_ms`
- `t_event_uptime_ms`
- `pointer_id`
- `key_class`
- `x_px`
- `y_px`
- `key_touch_x_ratio`
- `key_touch_y_ratio`
- `key_center_offset_x_px`
- `key_center_offset_y_px`
- `pressure`
- `size`
- `target_package`
- `input_type_class`
- `input_type_variation`
- `password_field`

Avoid raw text. If text is needed for a controlled calibration task, use a
separate explicit mode and store that fact in the schema.

## Safety Gate

Logging should be disabled when any of these are true:

- password variation
- visible password variation
- web password variation
- number password variation

Those password-class fields are the only automatic exclusions. OTP, email,
phone, URI, number, no-suggestions, no-personalized-learning, private IME
option, and incognito contexts remain in scope unless a later written protocol
explicitly changes that.

## Storage

Write JSONL to app-specific external storage by default so sessions are
directly pullable with ADB. Manual export can still use Android's standard
share/document picker flow. The keyboard should not request Internet
permission or broad storage permissions.

Suggested path:

```text
/sdcard/Android/data/org.typingresearch.ime/files/research_typing_logs/session-YYYYMMDD-HHMMSS.jsonl
/sdcard/Android/data/org.typingresearch.ime.debug/files/research_typing_logs/session-YYYYMMDD-HHMMSS.jsonl
```

Release and debug builds use distinct Android package IDs, so their direct
ADB paths differ:

```bash
adb pull /sdcard/Android/data/org.typingresearch.ime/files/research_typing_logs/ .
adb pull /sdcard/Android/data/org.typingresearch.ime.debug/files/research_typing_logs/ .
```

If app-specific external storage is unavailable, fall back to internal
app-private storage:

```text
files/research_typing_logs/session-YYYYMMDD-HHMMSS.jsonl
```

Fallback files can be inspected with `adb run-as`.

## Validation

Before collecting real sessions:

1. Verify logging can be toggled on/off.
2. Verify password fields suppress all key events.
3. Verify exported JSONL is valid.
4. Verify latest JSONL can be read with direct `adb pull`.
5. Verify down/up/commit ordering for normal letters, backspace, space, enter.
6. Compare event-time deltas against wall-clock deltas for drift.
7. Run one short calibration phrase and inspect hold/flight-time distributions.
