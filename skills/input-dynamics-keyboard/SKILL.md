---
name: input-dynamics-keyboard
description: Run and validate local Android Input Dynamics Keyboard sessions over ADB. Use when Codex needs to download or build the Android IME APK, install it on a device, enable or select it, start or stop input dynamics logging with an external run_id, inspect status or keyboard layout data, avoid screenshot-dependent automation, pull JSONL logs, or validate the password-field suppression boundary.
---

# Input Dynamics Keyboard

## Overview

Use this skill to operate the Input Dynamics Keyboard as a local Android IME
instrumentation surface. Keep workflows app-neutral, offline, and ADB-driven.

## Safety Rules

- Do not add or rely on Internet permission.
- Keep instructions app-neutral; do not name external services or study-specific
  workflows.
- Treat password-class fields as the hard automatic suppression boundary.
- Keep raw JSONL exports out of git.
- Prefer app-specific external storage for logs; use internal storage only as a
  fallback if the app reports that external storage is unavailable.
- Do not rely on UiAutomator to inspect soft-keyboard keys. Use
  `KEYBOARD_LAYOUT` and ADB taps when a non-screenshot path is needed.

## Defaults

Use these defaults unless the repo or device status says otherwise:

```bash
REPO=grapexy/input-dynamics-keyboard
PKG=org.inputdynamics.ime.debug
IME=helium314.keyboard.latin.LatinIME
RECEIVER=.control.InputDynamicsControlReceiver
ACTION_PREFIX=org.inputdynamics.ime.action
LOG_DIR=input_dynamics_logs
STATUS_FILE=input_dynamics_control_status.json
```

GitHub Release APKs are currently debug-variant APKs, so the default package is
`org.inputdynamics.ime.debug`. Locally built non-debug APKs use the same
receiver and action names with `PKG=org.inputdynamics.ime`.

## Workflow

1. Check the device:

```bash
adb devices
```

2. Install the latest published APK:

```bash
mkdir -p /tmp/input-dynamics-keyboard
gh release download --repo "$REPO" --pattern '*debug.apk' \
  --dir /tmp/input-dynamics-keyboard --clobber
APK="$(ls -t /tmp/input-dynamics-keyboard/*debug.apk | head -n 1)"
adb install -r "$APK"
```

If GitHub CLI is unavailable or source changes must be tested, build locally
from a repo checkout instead:

```bash
./gradlew :app:assembleDebug
APK="$(ls -t app/build/outputs/apk/debug/*.apk | head -n 1)"
adb install -r "$APK"
```

3. Confirm the installed APK does not request Internet permission:

```bash
if aapt dump permissions "$APK" | rg 'android.permission.INTERNET'; then
  echo "Unexpected INTERNET permission"
  exit 1
else
  echo "No INTERNET permission"
fi
```

4. Enable and select the IME:

```bash
adb shell ime enable "$PKG/$IME"
adb shell ime set "$PKG/$IME"
```

5. Start a session with an external run id:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.ENABLE"
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.START" --es run_id "$RUN_ID"
```

6. Read status and layout:

```bash
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.STATUS"
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.KEYBOARD_LAYOUT"
```

7. Stop and pull logs:

```bash
adb shell am broadcast -n "$PKG/$RECEIVER" -a "$ACTION_PREFIX.STOP"
adb pull "/sdcard/Android/data/$PKG/files/$LOG_DIR/" .
```

See `references/adb-control.md` for exact command variants and fallbacks.

## Non-Screenshot Automation

When the keyboard view is visible, use `KEYBOARD_LAYOUT` instead of screenshots.
Select key entries by `key_label` or `key_code`, then tap their screen centers:

```bash
adb shell input tap <tap_center_screen_x_px> <tap_center_screen_y_px>
```

After taps, verify that the JSONL log contains key/touch events and that status
record counts advance. If `keyboard_layout.available` is false, make the IME
visible in a text field and request the layout again.

## Validation

Validate the pulled JSONL before considering a run complete:

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

Read `references/jsonl-schema.md` before changing schema, event names, status
fields, or validation logic.
