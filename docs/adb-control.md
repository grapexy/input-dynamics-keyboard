# ADB Control And Validation

Input Dynamics Keyboard has a local ADB control surface for coordinated
research runs. It uses an explicit broadcast receiver and writes command status
to `input_dynamics_control_status.json` next to the JSONL logs.

## Build Artifacts

Build and test the debug variant:

```bash
./gradlew :app:testRunTestsUnitTest :app:assembleDebug
```

Build debug and unsigned release APKs:

```bash
./gradlew :app:assembleDebug :app:assembleRelease
```

Current APK outputs:

```text
app/build/outputs/apk/debug/InputDynamicsKeyboard_3.9-debug.apk
app/build/outputs/apk/debugNoMinify/InputDynamicsKeyboard_3.9-debugNoMinify.apk
app/build/outputs/apk/release/InputDynamicsKeyboard_3.9-release-unsigned.apk
```

The GitHub Release workflow currently publishes the debug APK. The local release
APK is unsigned and is not distributed unless a signed release process is added
later.

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
adb install -r app/build/outputs/apk/debug/InputDynamicsKeyboard_3.9-debug.apk
adb shell ime enable org.inputdynamics.ime.debug/helium314.keyboard.latin.LatinIME
adb shell ime set org.inputdynamics.ime.debug/helium314.keyboard.latin.LatinIME
```

## Control Commands

Use the debug package for local validation:

```bash
PKG=org.inputdynamics.ime.debug
IME=helium314.keyboard.latin.LatinIME
RUN_ID=run-YYYYMMDD-HHMMSS-human-android

adb shell ime enable "$PKG/$IME"
adb shell ime set "$PKG/$IME"

adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.ENABLE
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.START --es run_id "$RUN_ID"
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.STATUS
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.KEYBOARD_LAYOUT
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.LIST_LOGS
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.STOP
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.DISABLE
```

Use `PKG=org.inputdynamics.ime` only for locally installed signed release
builds.

Optional clear command, only when no session is active:

```bash
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.CLEAR_LOGS
```

## Status Output

Every command writes:

```text
input_dynamics_control_status.json
```

Status includes:

- package name
- version name and code
- build variant
- enabled/active state
- current and last session ids
- external run id
- log directory
- current or last log file path
- log file count
- cheap record count when available

Ordered broadcast result data may also print the same JSON to stdout on some
Android builds.

## Keyboard Layout Snapshot

`KEYBOARD_LAYOUT` adds a `keyboard_layout` object to the status JSON.

When the keyboard view is visible, each non-spacer key includes code, label,
local bounds, hit box, and screen tap center fields for calibration and layout
validation:

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
aapt dump permissions app/build/outputs/apk/debug/InputDynamicsKeyboard_3.9-debug.apk | grep INTERNET
```

The command should print nothing.
