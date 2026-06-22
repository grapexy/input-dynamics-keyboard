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

Input dynamics timing uses Android input timestamps from touch events:

- `MotionEvent.getEventTime()` for millisecond event time in uptime base
- `MotionEvent.getDownTime()` for gesture start time
- `MotionEvent.getHistoricalEventTime()` for coalesced move history
- `MotionEvent.getPressure()` and `MotionEvent.getSize()` when available
- `MotionEvent` action, pointer, source, device, tool-type, and classification
  metadata for reconstructing touch phases
- Keyboard-view coordinate frame and display metrics for aligning IME-local touch
  records with device-level event streams

Millisecond event time is the primary comparable signal.

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

The current schema value is:

```text
input_dynamics_event.v1
```

Input-scoped non-password records can also include:

- `press_id`
- `gesture_id`
- `t_event_uptime_ms`
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
- `pressure`
- `size`
- `touch_major_px`
- `touch_minor_px`
- `tool_major_px`
- `tool_minor_px`
- `orientation`
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

`press_id` correlates pointer samples with key down/up/commit records from the
same touch sequence. `gesture_id` currently matches `press_id` for ordinary key
touches; it is included so later gesture-level analysis can group richer
multi-sample interactions without changing existing records.

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
{"schema":"input_dynamics_event.v1","session_id":"20260621-102007-197e66cd","external_run_id":"run-YYYYMMDD-HHMMSS-human-android","event":"key_down","t_wall_ms":1782037207000,"t_uptime_ms":67690000,"t_event_uptime_ms":67689950,"target_package":"org.example.input","key_code":97,"key_label":"a","key_output_text":null,"key_class":"letter","key_touch_x_ratio":0.52,"key_touch_y_ratio":0.44,"password_field":false}
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
