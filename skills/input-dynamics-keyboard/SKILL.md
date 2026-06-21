---
name: input-dynamics-keyboard
description: Run and validate local Android Input Dynamics Keyboard sessions over ADB. Use when Codex needs to download or build the Android IME APK, install it on a device, enable or select it, record a bounded input dynamics run with an external run_id, inspect status or keyboard layout data, avoid screenshot-dependent automation, pull JSONL logs, or validate the password-field suppression boundary.
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
  the CLI layout and press/tap commands when a non-screenshot path is needed.
- Use the CLI's AOSP uinput-backed touch path for scripted key presses,
  absolute taps, and gestures. Do not use `adb shell input tap` for normal
  agent-driven input.

## Defaults

Use these defaults unless the repo or device status says otherwise:

```bash
REPO=grapexy/input-dynamics-keyboard
PKG=org.inputdynamics.ime.debug
LOG_DIR=input_dynamics_logs
```

If more than one Android device is connected, choose the target from
`adb devices` and pass `--serial "$SERIAL"` to every `idk` command. The CLI
fails rather than guessing when multiple devices are visible. Session runtime
state is keyed by package and ADB serial.

GitHub Release APKs are signed debug-variant APKs, so the default package is
`org.inputdynamics.ime.debug`. Locally built non-debug APKs use the same
receiver and action names with `PKG=org.inputdynamics.ime`.

## Workflow

Prefer the Rust host CLI when a full repo checkout or installed
`input-dynamics` binary is available. It emits JSON and wraps the lower-level
ADB commands.

```bash
idk() {
  if command -v input-dynamics >/dev/null 2>&1; then
    input-dynamics "$@"
  else
    cargo run --quiet -p input-dynamics -- "$@"
  fi
}
```

For multi-device hosts, include the serial in the wrapper:

```bash
idk() {
  if command -v input-dynamics >/dev/null 2>&1; then
    input-dynamics --serial "$SERIAL" "$@"
  else
    cargo run --quiet -p input-dynamics -- --serial "$SERIAL" "$@"
  fi
}
```

1. Check the host and device:

```bash
idk doctor
```

Check the local touchscreen input backend:

```bash
idk touch doctor
```

2. Install the latest published debug APK:

```bash
idk install
```

To install a local build instead:

```bash
./gradlew :app:assembleDebug
APK="$(ls -t app/build/outputs/apk/debug/*-debug.apk | head -n 1)"
idk install --apk "$APK"
```

3. For live agent-driven input, start one stateful session:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
idk session start --run-id "$RUN_ID"
```

Only one stateful session can be active for a package/device runtime. If
`session start` returns `ok: false` with `busy: true`, do not retry with
lower-level commands; inspect `idk session status` and wait for the active run
to stop.

Then use live commands while the session is active:

```bash
idk layout --wait-visible
idk type "ab a"
idk press delete
idk hide-keyboard --method edge-back --side right
idk session stop
```

If a controller process is interrupted, use `idk session stop` as the repair
path. It removes stale runtime files, stops IME logging, and reports whether
the saved virtual touchscreen event path is gone.

4. For bounded capture, use `record` with an external run id:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
idk record --run-id "$RUN_ID" --out "runs/$RUN_ID"
```

When `--duration-ms` is omitted, press Enter in the terminal to stop capture
cleanly. Agents should use `--duration-ms <ms>` for scripted smoke tests.

The command writes `manifest.json`, `validation.json`, `ime/`, `adb/`, and
`derived/` under the output directory. The `adb/getevent.raw.log` stream is
device-level touchscreen data and should be analyzed separately from IME-owned
JSONL privacy guarantees.

For a bounded agent-driven run that also needs persistent uinput controller
metadata, run `record` with `--with-input-controller` and a duration. Then drive
input from another CLI process while the record process is active. Use
`type <text>` for ordinary text entry:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
idk record \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --duration-ms 10000 \
  --with-input-controller \
  --input-actor agent_adb
```

The resulting manifest should include
`input_controller_runtime.summary.input_backend`,
`input_controller_runtime.summary.input_device_command`,
`input_controller_runtime.summary.physical_touchscreen_profile_hash`,
`input_controller_runtime.summary.virtual_touchscreen_event_path`, and
`input_controller_runtime.summary.cleanup`.

5. Use lower-level status and layout commands when debugging:

```bash
idk status
idk layout
```

6. If not using `record`, stop, pull, and validate logs manually:

```bash
idk stop
idk pull --out "runs/$RUN_ID"
idk validate "runs/$RUN_ID" --run-id "$RUN_ID"
```

If the CLI is unavailable, use `references/adb-control.md` for exact raw ADB
command variants and fallbacks.

## Non-Screenshot Automation

When the keyboard view is visible, use `KEYBOARD_LAYOUT` instead of screenshots.
With the CLI, start a session, wait for layout state, and press common keys by
semantic name. Use `type <text>` for ordinary text entry:

```bash
idk session start --run-id "$RUN_ID"
idk layout --wait-visible
idk touch doctor
idk type "ab a"
idk press delete
idk press enter
idk hide-keyboard --method edge-back --side right
idk session stop
```

`type <text>` plans the full string from visible layout keys before pressing any
key and fails on unsupported characters without partial typing. Use
`tap --code=<code>` only when there is no semantic command. Use `touch tap --x
<x> --y <y>` for diagnostic absolute screen coordinates. Use `touch swipe` and
`touch path` only when a protocol needs raw absolute gesture control; otherwise
prefer semantic commands such as `type`, `press`, and `hide-keyboard`. The CLI
routes session input and `touch` commands through AOSP `/system/bin/uinput` and
should fail rather than switch to another touch backend.

## Validation

Validate pulled JSONL before considering a run complete:

```bash
idk validate "runs/$RUN_ID" --run-id "$RUN_ID"
```

Expected validation includes `session_start`, `session_stop`,
`external_run_id`, session-level `input_actor`, `target_package`, schema
`input_dynamics_event.v1`, and no `password_field: true` records. Read
`references/jsonl-schema.md` before changing schema, event names, status
fields, or validation logic.
