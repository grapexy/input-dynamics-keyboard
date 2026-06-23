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
  `input_controller`, `input_cadence_policy`, and optional `input_profile_*`
  fields.
- Every record includes `t_elapsed_realtime_ns`, a high-resolution monotonic
  timestamp captured when the record is written. `t_event_uptime_ns` and
  `t_down_uptime_ns` are nanosecond-unit companions for Android millisecond
  input event times when those source fields are present.
- Key and pointer records include `press_id` when they belong to a touch
  sequence. `gesture_id` currently matches `press_id` for ordinary key touches.
- Pointer samples include `active_key_*` fields when keyboard layout context is
  available. These fields describe the key hit box or nearest key for that
  sample and the sample's relation to that key.
- Input-scoped records include `keyboard_state_available` and related keyboard
  context fields such as `keyboard_id`, `keyboard_mode`, `keyboard_element_id`,
  shift/caps state, subtype locale, subtype layout, and keyboard script when
  available.
- Lifecycle events include `input_view_start`, `input_view_finish`,
  `input_finish`, `ime_window_shown`, and `ime_window_hidden`.
- Dismissal-evidence events include `ime_hide_request`,
  `ime_hide_window_called`, `system_back_event`, and `editor_action`.
- Keep observed and inferred dismissal fields separate. Do not treat app-side
  hide reasons as ground truth unless they are actually observed by the event
  source.
- `target_package` identifies the active editor package reported to the IME.
- `field_episode_id` groups field-scoped records that appear to belong to one
  visible editing episode. It is a logger heuristic, not app-provided truth.
- `field_enter` records include editor metadata such as `ime_options`,
  `effective_ime_options`, `ime_action`, `ime_action_name`, `action_label`,
  `action_id`, and `editor_field_id`.
- `editor_action` records include the performed `action_id` plus the current
  field's editor metadata, using `field_action_id` for the app-provided
  `EditorInfo.actionId`.
- Text-edit operation events currently include `commit_text`, `set_composing_text`,
  `finish_composing_text`, `delete_surrounding_text`, `set_selection`,
  `set_composing_region`, `send_key_event`, and `commit_completion`.
  These records are emitted only for non-password fields and include
  before/after expected selection and composing-state snapshots where available.
- Semantic correction events currently include `suggestion_pick`,
  `auto_correction_commit`, and `auto_correction_revert`. These records are
  emitted only for non-password fields and include before/after expected
  selection and composing-state snapshots where available.
- `password_field: true` records should not appear in pulled non-password
  validation logs; password-class contexts are the hard suppression boundary.

## Common Fields

Fields that may appear across typing, touch, lifecycle, and text-edit records
include:

- `schema`
- `session_id`
- `external_run_id`
- `event`
- `t_uptime_ms`
- `t_uptime_ns`
- `t_wall_ms`
- `t_elapsed_realtime_ns`
- `t_event_uptime_ms`
- `t_event_uptime_ns`
- `t_down_uptime_ms`
- `t_down_uptime_ns`
- `target_package`
- `field_episode_id`
- `ime_options`
- `effective_ime_options`
- `ime_action`
- `ime_action_name`
- `action_label`
- `action_id`
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
- `press_id`
- `gesture_id`
- `keyboard_state_available`
- `keyboard_state_unavailable_reason`
- `keyboard_id`
- `keyboard_mode`
- `keyboard_mode_name`
- `keyboard_element_id`
- `keyboard_element_name`
- `keyboard_shift_mode`
- `keyboard_shift_mode_name`
- `keyboard_shifted`
- `keyboard_shift_source`
- `keyboard_caps_locked`
- `keyboard_is_alphabet`
- `keyboard_is_alpha_or_symbol`
- `keyboard_is_alphabet_shifted`
- `keyboard_is_alphabet_shifted_manually`
- `keyboard_is_number_layout`
- `keyboard_is_emoji`
- `keyboard_subtype_locale`
- `keyboard_subtype_locale_tag`
- `keyboard_subtype_main_layout_name`
- `keyboard_subtype_is_rtl`
- `keyboard_subtype_is_no_language`
- `keyboard_script`
- `pointer_id`
- `key_code`
- `key_label`
- `key_output_text`
- `key_class`
- `key_touch_x_ratio`
- `key_touch_y_ratio`
- `key_center_offset_x_px`
- `key_center_offset_y_px`
- `active_key_present`
- `active_key_lookup`
- `active_key_relation`
- `active_key_code`
- `active_key_label`
- `active_key_class`
- `active_key_touch_x_ratio`
- `active_key_touch_y_ratio`
- `active_key_center_offset_x_px`
- `active_key_center_offset_y_px`
- `active_key_distance_to_bounds_px`
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
