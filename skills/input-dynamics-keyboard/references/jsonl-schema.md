# JSONL Schema Notes

Use this reference when validating logs or changing event fields.

## Contract

- Logs are newline-delimited JSON.
- Every record includes `schema`.
- The current schema is `input_dynamics_event.v1`.
- Every record in a session includes the internal `session_id`.
- Every record in a session includes caller-provided `external_run_id` when a
  run id was passed to `START`.
- `session_start` and `session_stop` records bracket a normal session.
- `session_start` includes session-level provenance: `input_actor`,
  `input_controller`, and `input_cadence_policy`.
- Key and pointer records include `press_id` when they belong to a touch
  sequence. `gesture_id` currently matches `press_id` for ordinary key touches.
- Lifecycle events include `input_view_start`, `input_view_finish`,
  `input_finish`, `ime_window_shown`, and `ime_window_hidden`.
- Dismissal-evidence events include `ime_hide_request`,
  `ime_hide_window_called`, `system_back_event`, and `editor_action`.
- Keep observed and inferred dismissal fields separate. Do not treat app-side
  hide reasons as ground truth unless they are actually observed by the event
  source.
- `target_package` identifies the active editor package reported to the IME.
- `password_field: true` records should not appear in pulled non-password
  validation logs; password-class contexts are the hard suppression boundary.

## Common Fields

Minimum fields expected across typing/touch records include:

- `schema`
- `session_id`
- `external_run_id`
- `event`
- `t_uptime_ms`
- `t_wall_ms`
- `target_package`
- `press_id`
- `gesture_id`
- `pointer_id`
- `key_code`
- `key_label`
- `key_output_text`
- `key_class`
- `key_touch_x_ratio`
- `key_touch_y_ratio`
- `key_center_offset_x_px`
- `key_center_offset_y_px`
- `pressure`
- `size`
- `password_field`

Not every field is meaningful for every event type. Preserve existing fields
when adding new records.

## Validation Query

```bash
cat input_dynamics_logs/session-*.jsonl | jq -s --arg run_id "$RUN_ID" '
  (map(select(.event == "session_start")) | length) >= 1 and
  (map(select(.event == "session_stop")) | length) >= 1 and
  all(.[]; .schema == "input_dynamics_event.v1") and
  all(.[]; .external_run_id == $run_id) and
  any(.[]; has("target_package")) and
  all(.[]; .password_field != true)
'
```

When validating password-field suppression, use only disposable local input
text. Do not enter or log real sensitive material.
