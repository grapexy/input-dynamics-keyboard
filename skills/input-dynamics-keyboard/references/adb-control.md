# ADB Control Reference

Use this reference when an agent needs exact local commands for Input Dynamics
Keyboard control, status, layout inspection, log readback, or cleanup.

## Packages

```bash
# Debug build
PKG=org.inputdynamics.ime.debug

# Release build
PKG=org.inputdynamics.ime

IME=helium314.keyboard.latin.LatinIME
RECEIVER=.control.InputDynamicsControlReceiver
ACTION_PREFIX=org.inputdynamics.ime.action
LOG_DIR=input_dynamics_logs
STATUS_FILE=input_dynamics_control_status.json
```

## Enable IME

```bash
adb shell ime enable "$PKG/$IME"
adb shell ime set "$PKG/$IME"
```

If enable fails immediately after install, list IMEs and retry after Android
finishes registering the service:

```bash
adb shell ime list -a | rg "$PKG|$IME"
```

## Session Commands

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android

adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.ENABLE"
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.START" --es run_id "$RUN_ID"
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.STATUS"
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.KEYBOARD_LAYOUT"
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.LIST_LOGS"
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.STOP"
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.DISABLE"
```

Clear logs only when no session is active:

```bash
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.CLEAR_LOGS"
```

## Status Fallback

Ordered broadcast result data may print JSON to stdout. If stdout is missing or
truncated, read the status file:

```bash
adb shell cat "/sdcard/Android/data/$PKG/files/$LOG_DIR/$STATUS_FILE"
```

Expected status fields include:

- `package_name`
- `version_name`
- `version_code`
- `build_variant`
- `enabled`
- `active`
- `current_session_id`
- `last_session_id`
- `external_run_id`
- `log_directory_path`
- `current_log_file_path`
- `last_log_file_path`
- `log_file_count`
- `record_count`

## Layout Taps

Request a layout snapshot while the IME is visible:

```bash
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.KEYBOARD_LAYOUT"
```

When `keyboard_layout.available` is true, each key can include screen tap center
fields. Use those values directly:

```bash
adb shell input tap <tap_center_screen_x_px> <tap_center_screen_y_px>
```

This is the preferred automation path when avoiding screenshots and image
matching.

## Pull Logs

```bash
adb pull "/sdcard/Android/data/$PKG/files/$LOG_DIR/" .
```

Internal fallback files, if used by the app, can be inspected on debug builds
with `run-as`:

```bash
adb shell run-as "$PKG" ls "files/$LOG_DIR"
```
