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
LOG_DIR=input_dynamics_logs
```

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

1. Check the host and device:

```bash
idk doctor
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

3. Enable and select the IME:

```bash
idk select-ime
```

4. Start a session with an external run id:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
idk start --run-id "$RUN_ID"
```

5. Read status and layout:

```bash
idk status
idk layout
```

6. Stop, pull, and validate logs:

```bash
idk stop
idk pull --out "runs/$RUN_ID"
idk validate "runs/$RUN_ID" --run-id "$RUN_ID"
```

If the CLI is unavailable, use `references/adb-control.md` for exact raw ADB
command variants and fallbacks.

## Non-Screenshot Automation

When the keyboard view is visible, use `KEYBOARD_LAYOUT` instead of screenshots.
With the CLI, tap by key label or code:

```bash
idk tap --label a
idk tap --code 97
```

Without the CLI, select key entries from `KEYBOARD_LAYOUT` by `key_label` or
`key_code`, then tap their screen centers with `adb shell input tap`.

## Validation

Validate pulled JSONL before considering a run complete:

```bash
idk validate "runs/$RUN_ID" --run-id "$RUN_ID"
```

Expected validation includes `session_start`, `session_stop`,
`external_run_id`, `target_package`, schema `input_dynamics_event.v1`, and no
`password_field: true` records. Read `references/jsonl-schema.md` before
changing schema, event names, status fields, or validation logic.
