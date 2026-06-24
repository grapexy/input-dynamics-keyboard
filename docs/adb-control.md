# ADB Control And Validation

Input Dynamics Keyboard has a local ADB control surface for coordinated
research runs. It uses an explicit broadcast receiver and writes command status
to JSON files next to the JSONL logs.

For normal agent and scripted operation, prefer the host CLI in [cli.md](cli.md).
Use this page as the raw protocol reference and diagnostic path. The CLI adds a
unique `request_id` to each broadcast and waits for the matching
`input_dynamics_control_result_<request_id>.json` file. The latest
`input_dynamics_control_status.json` file is still written for inspection and
compatibility.

## Build Artifacts

Build and test the debug variant:

```bash
./gradlew :app:testRunTestsUnitTest :app:assembleDebug
```

Build signed debug and release APKs:

```bash
./gradlew :app:assembleDebug :app:assembleRelease
```

Current APK outputs:

```text
app/build/outputs/apk/debug/*-debug.apk
app/build/outputs/apk/debugNoMinify/*-debugNoMinify.apk
app/build/outputs/apk/release/*-release.apk
```

Installable APK builds require the project signing environment. The Nix dev
shell loads `.git/signing/input-dynamics.env` automatically when it exists.
GitHub Release assets are signed with the same project APK key.

## Package IDs

| Build | Android package | IME component |
| --- | --- | --- |
| Release | `org.inputdynamics.ime` | `helium314.keyboard.latin.LatinIME` |
| Debug | `org.inputdynamics.ime.debug` | `helium314.keyboard.latin.LatinIME` |

The ADB receiver can be addressed with shorthand component names:

```text
org.inputdynamics.ime.debug/.control.InputDynamicsControlReceiver
org.inputdynamics.ime/.control.InputDynamicsControlReceiver
```

These expand to:

```text
org.inputdynamics.ime.debug/org.inputdynamics.ime.debug.control.InputDynamicsControlReceiver
org.inputdynamics.ime/org.inputdynamics.ime.control.InputDynamicsControlReceiver
```

## Install Debug Build

```bash
APK="$(ls -t app/build/outputs/apk/debug/*-debug.apk | head -n 1)"
adb install -r "$APK"
adb shell ime enable org.inputdynamics.ime.debug/helium314.keyboard.latin.LatinIME
adb shell ime set org.inputdynamics.ime.debug/helium314.keyboard.latin.LatinIME
```

## Control Commands

Use the debug package for local validation:

These are raw IME broadcast commands, not the complete observation workflow.
For normal capture use `input-dynamics session start`, `input-dynamics session
status`, and `input-dynamics session stop` from [cli.md](cli.md).

Canonical complete observation session:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
input-dynamics session start --input-actor human --run-id "$RUN_ID" --out "runs/$RUN_ID"
input-dynamics session status --run-id "$RUN_ID"
input-dynamics session stop --run-id "$RUN_ID"
input-dynamics recording inspect --dir "runs/$RUN_ID"
```

Bounded smoke capture:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-smoke
input-dynamics session run --input-actor human --run-id "$RUN_ID" --out "runs/$RUN_ID" --duration-ms 10000
input-dynamics recording inspect --dir "runs/$RUN_ID"
```

```bash
PKG=org.inputdynamics.ime.debug
IME=helium314.keyboard.latin.LatinIME
RUN_ID=run-YYYYMMDD-HHMMSS-local-android

adb shell ime enable "$PKG/$IME"
adb shell ime set "$PKG/$IME"

adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.ENABLE
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.START \
  --es run_id "$RUN_ID" \
  --es input_actor human \
  --es input_cadence_policy manual
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.STATUS
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.KEYBOARD_LAYOUT
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.LIST_LOGS
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.STOP
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.DISABLE
```

For request-correlated raw debugging, pass a unique `request_id`:

```bash
REQUEST_ID=manual-$(date +%s)
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" \
  -a org.inputdynamics.ime.action.STATUS \
  --es request_id "$REQUEST_ID"
adb shell cat "/sdcard/Android/data/$PKG/files/input_dynamics_logs/input_dynamics_control_result_${REQUEST_ID}.json"
```

Use `PKG=org.inputdynamics.ime` only for installed release builds.

Optional clear command, only when no session is active:

```bash
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.CLEAR_LOGS
```

## Status Output

Every command writes the latest status file:

```text
input_dynamics_control_status.json
```

When the caller supplies `request_id`, the app also writes an exact command
result file:

```text
input_dynamics_control_result_<request_id>.json
```

Status includes:

- package name
- request id, when the caller supplied one
- version name and code
- build variant
- enabled/active state
- current and last session ids
- current external run id and last external run id
- input actor, controller, and cadence policy
- input-scope readiness fields: `input_scope_ready`, `input_scope_state`,
  `current_target_package`, and `current_field_episode_id`
- log directory
- current or last log file path
- latest status file path
- exact result file path, when `request_id` was supplied
- log file count
- cheap record count when available
- `pending_writes_drained`, when the command came through the receiver
- `device_clock_probe`, the canonical status/control timing sample

Ordered broadcast result data may also print JSON to stdout on some Android
builds. Treat the exact result file as the raw command-result source when a
`request_id` was supplied.

`device_clock_probe` has schema `input_dynamics_device_clock_probe.v1` and is
the reusable device timing sample for status/control artifacts. It includes:

- `request_id`
- `probe_source: "status_broadcast"`
- `captured_by: "android_control_status"`
- `canonical_clock_domain: "device_elapsed_realtime_ns"`
- `t_elapsed_realtime_ns` with `elapsed_realtime_time`
- `t_uptime_ms` and converted `t_uptime_ns` with `uptime_time`
- diagnostic `t_wall_ms` with `wall_time`
- `pending_writes_drained`

The uptime nanosecond companion is converted from Android uptime milliseconds;
do not treat it as native nanosecond precision. Wall time is diagnostic only.
For canonical recording or evidence anchors, require a request-correlated result
file with a matching `request_id` and a valid `device_clock_probe`; do not use
the mutable latest status file as a freshness fallback.

## Keyboard Layout Snapshot

`KEYBOARD_LAYOUT` adds a `keyboard_layout` object to the status JSON.

When the keyboard view is visible, each non-spacer key includes code, label,
local bounds, hit box, and screen tap center fields for calibration and layout
validation. The snapshot also includes keyboard state fields such as
`keyboard_mode_name`, `keyboard_element_name`, `keyboard_shift_mode_name`,
`keyboard_subtype_locale_tag`, `keyboard_subtype_main_layout_name`, and
`keyboard_script`:

```json
{
  "keyboard_layout": {
    "available": true,
    "key_count": 33,
    "keys": [
      {
        "key_label": "t",
        "key_code": 116,
        "tap_center_screen_x_px": 648.5,
        "tap_center_screen_y_px": 2166
      }
    ]
  }
}
```

If the IME view is not available or not shown, `keyboard_layout.available` is
`false` and `keyboard_layout.unavailable_reason` explains why.

## Pull Logs

Debug logs:

```bash
adb pull /sdcard/Android/data/org.inputdynamics.ime.debug/files/input_dynamics_logs/ .
```

Release logs:

```bash
adb pull /sdcard/Android/data/org.inputdynamics.ime/files/input_dynamics_logs/ .
```

## Validate JSONL

For a normal non-password validation run:

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

For a password-field-only validation run, use a distinct `RUN_ID`, type only in
a password-class field, stop the session, and verify that the session contains
only lifecycle records:

```bash
cat input_dynamics_logs/session-*.jsonl | jq -s --arg run_id "$RUN_ID" '
  [ .[] | select(.external_run_id == $run_id) | .event ] == ["session_start", "session_stop"]
'
```

Confirm the APK does not request Internet permission:

```bash
APK="$(ls -t app/build/outputs/apk/debug/*-debug.apk | head -n 1)"
aapt dump permissions "$APK" | grep INTERNET
```

The command should print nothing.
