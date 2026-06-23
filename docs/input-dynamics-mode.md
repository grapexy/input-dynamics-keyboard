# Input Dynamics Mode

Input Dynamics Keyboard is a local Android IME research fork for measuring
real human soft-keyboard interaction timing.

The project is application-neutral and focused on local keyboard
instrumentation.

## Purpose

The input dynamics mode records consented typing interaction traces from real Android
keyboard use so timing and touch behavior can be studied empirically.

Primary measurements include:

- key press and release timing
- hold time and flight time
- touch position within key bounds
- pointer pressure and size when available
- correction actions such as backspace, space, enter, and suggestion taps
- session-level cadence and pause structure

## Privacy Boundary

Use this keyboard only in consented, local research settings.

- The app is offline and must not request Internet permission.
- Logs are written locally; there is no automatic upload path.
- Password-class fields are the hard automatic suppression boundary.
- Non-password fields remain in scope and may include typed content, key labels,
  output text, input app package context, timing, and touch geometry.
- Raw JSONL exports are sensitive research data and must stay out of git,
  public issues, screenshots, and public logs unless a written protocol
  explicitly allows sharing.

Password-class fields include password, visible password, web password, and
number password variations. Other ordinary non-password field types remain in
scope unless a written protocol changes that.

## Event Sources

Input dynamics timing uses Android input timestamps from touch and key events:

- `MotionEvent.getEventTime()` for millisecond event time in uptime base
- `MotionEvent.getDownTime()` for gesture start time
- `MotionEvent.getHistoricalEventTime()` for coalesced move history
- `KeyEvent.getEventTime()` for observed system Back key event time
- `MotionEvent.getPressure()` and `MotionEvent.getSize()` when available
- `MotionEvent` action, pointer, source, device, tool-type, and classification
  metadata for reconstructing touch phases
- Keyboard-view coordinate frame and display metrics for aligning IME-local touch
  records with device-level event streams

Each JSONL record also includes `t_elapsed_realtime_ns`, a high-resolution
monotonic timestamp captured when the record is written, and
`t_capture_elapsed_realtime_ns`, a high-resolution monotonic timestamp captured
before the async writer enqueue. Existing Android input event timestamps remain
millisecond uptime values. The logger includes nanosecond-unit companions such
as `t_event_uptime_ns` and `t_down_uptime_ns` when those millisecond source
fields are present; these fields are for unit alignment, not extra source-event
precision. Legacy flat fields `t_uptime_ms` and `t_uptime_ns` are writer-time
uptime fields, not source-event fields.

Clock domains are intentionally explicit. Do not compare timestamps from
different domains unless a derived artifact also names the transform and
uncertainty used for that comparison.

| Clock domain | Meaning | Typical use |
| --- | --- | --- |
| `android_uptime_ms` / `android_uptime_ns` | Android uptime clock used by `MotionEvent` and `KeyEvent`; source-event time, excluding deep sleep. | Key/touch hold and flight timing. |
| `device_elapsed_realtime_ns` | Android elapsed realtime clock, including deep sleep. | App status/control timestamps and future recording/video/evidence anchors. |
| `kernel_getevent_us` | Raw `getevent -lt` timestamp domain. | Device-level touch streams before explicit alignment. |
| `media_pts_ns` | Encoded video media presentation timestamp. | Video frame indexes before explicit event mapping. |
| `host_process_monotonic_ns` | Host CLI process-relative monotonic clock. | ADB latency diagnostics. |
| `host_wall_ms` | Host wall clock. | Human-readable provenance only. |
| `device_wall_ms` | Device wall clock. | Legacy diagnostics only; not ordering truth. |

The canonical vocabulary is closed for writers and validators. Public readers
that need to inspect newer artifacts should preserve unknown strings as unknown
metadata instead of silently interpreting them. Adding a new canonical string is
schema-additive, but strict validation code must be updated before treating it
as known.

Records with more than one timestamp role must not use a single top-level
`clock_domain` as if it describes every timestamp. New JSONL records describe
timestamp roles separately with nested metadata objects:

| Object | Applies To | Meaning |
| --- | --- | --- |
| `event_time` | Event-bearing records with `t_event_uptime_ms` | Source-event timestamp metadata. |
| `down_time` | Pointer samples with `t_down_uptime_ms` | Gesture-start timestamp metadata. |
| `capture_time` | Every new record | Callback/capture timestamp metadata, before async writer enqueue. |
| `write_time` | Every new record | Async writer timestamp metadata. |

Each timestamp metadata object includes `clock_domain`, `timestamp_source`,
`timestamp_precision`, and `field`. Objects that reference millisecond source
fields with nanosecond-unit companions also include `field_ns` and
`field_ns_precision`.

The validator accepts older `input_dynamics_event.v1` records that predate
timestamp-role metadata as legacy-compatible input. Once a record includes the
new capture timestamp or any timestamp-role object, validation requires the
complete current role metadata for that event family.

Current source mapping:

| Record family | Timestamp source |
| --- | --- |
| `pointer_sample` `event_time` / `down_time` | `motion_event` |
| Soft-key semantic records `key_down`, `key_up`, and `key_commit` | `motion_event` |
| Timer-driven soft-key semantic records `key_repeat`, `key_long_press`, and `key_cancel` | `synthetic_handler` |
| `system_back_event` | `key_event` |
| Lifecycle, field, text-edit, editor-action, suggestion, manual, and session records | No `event_time`; use `capture_time` and `write_time`. |

Some older derived artifacts may contain pre-vocabulary labels such as
`ime_uptime_ms`, `getevent_time_us`, or
`host_wall_ms_bracketed_device_epoch_ms`. Treat those as legacy labels, not
canonical clock domains. Do not silently alias them to canonical domains unless
a migration artifact or explicit compatibility layer says how confidence and
uncertainty are preserved.

## Event Types

Implemented event types:

- `session_start`
- `session_stop`
- `field_enter`
- `field_exit`
- `input_view_start`
- `input_view_finish`
- `input_finish`
- `ime_window_shown`
- `ime_window_hidden`
- `ime_hide_request`
- `ime_hide_window_called`
- `system_back_event`
- `editor_action`
- `pointer_sample`
- `key_down`
- `key_up`
- `key_commit`
- `key_repeat`
- `key_long_press`
- `key_cancel`
- `suggestion_pick`
- `auto_correction_commit`
- `auto_correction_revert`

Key records use `key_class` values such as `letter`, `digit`, `symbol`,
`space`, `enter`, `delete`, `modifier`, `action`, and `function`.

## JSONL Schema

Logs are newline-delimited JSON. Every record has:

- `schema`
- `session_id`
- `external_run_id`
- `event`
- `t_wall_ms`
- `t_uptime_ms`
- `t_uptime_ns`
- `t_elapsed_realtime_ns`
- `t_capture_elapsed_realtime_ns`
- `capture_time`
- `write_time`

The current schema value is:

```text
input_dynamics_event.v1
```

Input-scoped non-password records can also include:

- `press_id`
- `gesture_id`
- `t_event_uptime_ms`
- `t_event_uptime_ns`
- `event_time`
- `down_time`
- `pointer_id`
- `pointer_index`
- `pointer_count`
- `motion_action`
- `motion_action_name`
- `motion_action_index`
- `motion_source`
- `input_device_id`
- `tool_type`
- `tool_type_name`
- `classification`
- `classification_name`
- `key_code`
- `key_code_printable`
- `key_label`
- `key_hint_label`
- `key_preview_label`
- `key_output_text`
- `key_icon_name`
- `key_alt_code`
- `key_popup_keys`
- `key_class`
- `x_px`
- `y_px`
- `key_touch_x_ratio`
- `key_touch_y_ratio`
- `key_center_offset_x_px`
- `key_center_offset_y_px`
- `active_key_present`
- `active_key_lookup`
- `active_key_relation`
- `active_key_code`
- `active_key_code_printable`
- `active_key_label`
- `active_key_output_text`
- `active_key_class`
- `active_key_x_px`
- `active_key_y_px`
- `active_key_width_px`
- `active_key_height_px`
- `active_key_hitbox_left_px`
- `active_key_hitbox_top_px`
- `active_key_hitbox_right_px`
- `active_key_hitbox_bottom_px`
- `active_key_touch_x_ratio`
- `active_key_touch_y_ratio`
- `active_key_center_offset_x_px`
- `active_key_center_offset_y_px`
- `active_key_distance_to_bounds_px`
- `active_key_near_threshold_px`
- `active_key_inside_hitbox`
- `active_key_inside_bounds`
- `pressure`
- `size`
- `touch_major_px`
- `touch_minor_px`
- `tool_major_px`
- `tool_minor_px`
- `orientation`
- `t_down_uptime_ms`
- `t_down_uptime_ns`
- `coordinate_space`
- `coordinate_frame_available`
- `keyboard_view_visible`
- `keyboard_view_width_px`
- `keyboard_view_height_px`
- `keyboard_view_left_screen_px`
- `keyboard_view_top_screen_px`
- `keyboard_view_right_screen_px`
- `keyboard_view_bottom_screen_px`
- `keyboard_visible_top_y_screen_px`
- `keyboard_visible_height_px`
- `display_width_px`
- `display_height_px`
- `display_rotation`
- `display_rotation_name`
- `x_screen_px`
- `y_screen_px`
- `target_package`
- `field_episode_id`
- `input_type_class`
- `input_type_variation`
- `ime_options`
- `effective_ime_options`
- `ime_action`
- `ime_action_name`
- `action_label`
- `action_id`
- `field_action_id`
- `editor_field_id`
- `commit_text`
- `commit_text_length`
- `commit_text_code_point_count`
- `composing_text`
- `composing_text_before`
- `composing_text_after`
- `delete_before_count`
- `delete_after_count`
- `selection_start_before`
- `selection_end_before`
- `selection_start_after`
- `selection_end_after`
- `suggestion_decision`
- `suggestion`
- `suggestion_length`
- `suggestion_code_point_count`
- `suggestion_index`
- `suggestion_rank`
- `suggestion_type`
- `suggestion_score`
- `suggestions_count`
- `suggestions_will_auto_correct`
- `typed_word`
- `typed_word_length`
- `typed_word_code_point_count`
- `auto_correction`
- `auto_correction_type`
- `auto_correction_index`
- `auto_correction_rank`
- `auto_correction_applied`
- `committed_word`
- `originally_typed_word`
- `separator`
- `input_connection_result`
- `restarting`
- `finishing_input`
- `input_view_shown`
- `dismissal_source_observed`
- `dismissal_confidence`
- `dismissal_evidence`
- `password_field`

`target_package` is the Android package name reported for the app that owns the
current input field. It is useful for session provenance and layout/input-type
debugging, and should be treated as sensitive context in exports.

`field_episode_id` is a logger-generated grouping key for non-password
field-scoped records that appear to belong to one visible editing episode.
Rapid finish/start churn for the same field signature may reuse an episode id.
Treat it as an analysis aid, not app-provided ground truth.

`session_id` is generated internally for each logging session.
`external_run_id` is optional caller-provided metadata for coordinating local
runs; when present, it is copied to every JSONL record in that session.

`session_start` records also include session-level input provenance:

- `input_actor`, default `human`
- `input_controller`, default null
- `input_cadence_policy`, default `manual`
- `input_profile_source`
- `input_profile_id`
- `input_profile_schema`
- `input_profile_hash`
- `input_profile_seed`

These fields let later analysis compare human-observed sessions with other
locally controlled sessions without changing the per-key timing schema.
Profile fields identify the controller-side input generator configuration; the
IME records provenance only and does not implement profile sampling.

Lifecycle and dismissal-evidence records use the same non-password field
boundary as key/touch records. They preserve the latest non-password field
context long enough to record IME visibility and hide callbacks that may arrive
after field cleanup. Observed dismissal evidence such as `ime_hide_request`,
`ime_hide_window_called`, `system_back_event`, and `editor_action` should not be
treated as a definitive app-side hide reason unless the record explicitly marks
`dismissal_confidence` as `definitive`.

`field_enter` records include editor metadata from Android `EditorInfo`:
`ime_options`, `effective_ime_options`, `ime_action`, `ime_action_name`,
`action_label`, `action_id`, and `editor_field_id`. `editor_action` records use
`action_id` for the performed action and `field_action_id` for the app-provided
field action id.

Text-edit operation records are emitted from the keyboard's `InputConnection`
wrapper for non-password fields. Current operation events include
`commit_text`, `set_composing_text`, `finish_composing_text`,
`delete_surrounding_text`, `set_selection`, `set_composing_region`,
`send_key_event`, and `commit_completion`. They include operation-specific
fields plus before/after expected selection and composing-text snapshots where
the keyboard has them.

Suggestion and autocorrection records describe semantic decisions while the
text-edit operation records describe lower-level editor calls:

- `suggestion_pick`: a suggestion was manually chosen from the suggestion UI.
  It includes suggestion text, rank/index in the `SuggestedWords` list, kind/type,
  score, current composing word, and before/after edit-state snapshots.
- `auto_correction_commit`: the keyboard committed a correction that differs
  from the typed word. It includes typed word, committed word, correction rank,
  correction type, separator, and before/after edit-state snapshots.
- `auto_correction_revert`: backspace reverted an autocorrection. It includes
  the originally typed word, committed word, separator handling, and before/after
  edit-state snapshots.

`press_id` correlates pointer samples with key down/up/commit records from the
same touch sequence. `gesture_id` currently matches `press_id` for ordinary key
touches; it is included so later gesture-level analysis can group richer
multi-sample interactions without changing existing records.

`pointer_sample` records include `active_key_*` fields when the keyboard layout
is available. These fields describe the key hit box or nearest key for that
sample, including key-relative ratios and whether the sample is inside the key
hit box, inside the visual key bounds, near the bounds, or outside them. When
layout context is not available, `active_key_present` is false and
`active_key_lookup` explains why.

Input-scoped records include `keyboard_state_*` and related keyboard context
fields when the IME can read its current keyboard state. These include
`keyboard_id`, `keyboard_mode`, `keyboard_mode_name`, `keyboard_element_id`,
`keyboard_element_name`, `keyboard_shift_mode`, `keyboard_shift_mode_name`,
`keyboard_shifted`, `keyboard_shift_source`, `keyboard_caps_locked`,
`keyboard_subtype_locale`, `keyboard_subtype_locale_tag`,
`keyboard_subtype_main_layout_name`, and `keyboard_script`. When state is not
available, `keyboard_state_available` is `false` and
`keyboard_state_unavailable_reason` explains why.

Pointer samples keep the original legacy `action`, `action_name`, `source`, and
`device_id` fields for compatibility. New records also include the clearer
aliases `motion_action`, `motion_action_name`, `motion_source`, and
`input_device_id`.

`x_px` and `y_px` are local to the keyboard view when they appear on key and
pointer records. `x_screen_px` and `y_screen_px` are the same point translated
into screen pixels using the logged keyboard-view frame. Display and keyboard
frame fields are included on pointer, key, and IME lifecycle records when the
keyboard view is available.

Example record:

```json
{"schema":"input_dynamics_event.v1","session_id":"20260621-102007-197e66cd","external_run_id":"run-YYYYMMDD-HHMMSS-human-android","event":"key_down","t_wall_ms":1782037207000,"t_uptime_ms":67690000,"t_uptime_ns":67690000000000,"t_elapsed_realtime_ns":1800200300400500,"t_capture_elapsed_realtime_ns":1800200300000000,"t_event_uptime_ms":67689950,"t_event_uptime_ns":67689950000000,"event_time":{"clock_domain":"android_uptime_ms","timestamp_source":"motion_event","timestamp_precision":"milliseconds","field":"t_event_uptime_ms","field_ns":"t_event_uptime_ns","field_ns_precision":"milliseconds_converted_to_nanoseconds"},"capture_time":{"clock_domain":"device_elapsed_realtime_ns","timestamp_source":"callback_capture","timestamp_precision":"nanoseconds","field":"t_capture_elapsed_realtime_ns"},"write_time":{"clock_domain":"device_elapsed_realtime_ns","timestamp_source":"writer","timestamp_precision":"nanoseconds","field":"t_elapsed_realtime_ns"},"target_package":"org.example.input","key_code":97,"key_label":"a","key_output_text":null,"key_class":"letter","key_touch_x_ratio":0.52,"key_touch_y_ratio":0.44,"password_field":false}
```

## Storage

Input dynamics logs are written to app-specific external storage by default:

```text
/sdcard/Android/data/org.inputdynamics.ime/files/input_dynamics_logs/
/sdcard/Android/data/org.inputdynamics.ime.debug/files/input_dynamics_logs/
```

Each session writes a file named:

```text
session-<session_id>.jsonl
```

The ADB control surface also writes:

```text
input_dynamics_control_status.json
```

If app-specific external storage is unavailable, the logger falls back to
internal app-private storage. `adb shell run-as` access to fallback files is
normally limited to debuggable builds.
