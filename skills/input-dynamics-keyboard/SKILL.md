---
name: input-dynamics-keyboard
description: Run and validate local Android Input Dynamics Keyboard sessions over ADB. Use when Codex needs to download or build the Android IME APK, install it on a device, enable or select it, record a bounded input dynamics run with an external run_id, inspect status, keyboard layout, accessibility, or screenshot evidence, avoid screenshot-dependent keyboard automation, pull JSONL logs, or validate the password-field suppression boundary.
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
- Use `observe` commands for optional screen-context evidence. Accessibility
  dumps and screenshots may contain visible screen content; store them with the
  same care as local run artifacts.
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

For current screen context without starting a run, use `observe`:

```bash
idk observe state --with-accessibility
idk observe all --out-dir "/tmp/input-dynamics-evidence"
```

`observe all` writes status, keyboard layout, accessibility XML, screenshot
PNG, state JSON, and index JSON. Prefer `layout` for keyboard-key coordinates;
use observation evidence to understand surrounding screen state.

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

`session start` uses the bundled `profiles/baseline-v1.json` input profile by
default for controller-driven sessions. Its default session provenance is
`input_actor=agent_adb`, `input_controller=input-dynamics-cli`, and
`input_cadence_policy=input_profile`. To bind a local profile to the whole
session, pass `--input-profile <path>` and optionally
`--input-profile-seed <u64>` for reproducible sampling:

```bash
idk session start \
  --run-id "$RUN_ID" \
  --input-profile ./profiles/custom.json \
  --input-profile-seed 12345
```

Only one stateful session can be active for a package/device runtime. If
`session start` returns `ok: false` with `busy: true`, do not retry with
lower-level commands; inspect `idk session status` and wait for the active run
to stop.

Then use live commands while the session is active:

```bash
idk keyboard ensure-visible
idk type "ab a"
idk press delete
idk hide-keyboard --method edge-back --side right
idk session stop
```

If a controller process is interrupted, use `idk session stop` as the repair
path. It removes stale runtime files, stops IME logging, and reports whether
the saved virtual touchscreen event path is gone. For controller timing or
socket failures, inspect `idk session status`; `input.state` exposes
`current_command`, `last_command`, `last_error`, and `command_sequence`.

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
JSONL privacy guarantees. Current manifests include `coordinate_frame`, derived
from recorded touchscreen profile and layout snapshots.

When a run needs screen context, add `--with-evidence`. This writes start/end
observation bundles under `evidence/start/` and `evidence/end/`, each with
accessibility XML, screenshot PNG, status JSON, layout JSON, state JSON, and
index JSON:

```bash
idk record \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --with-evidence
```

Use `--full-accessibility-evidence` only when a protocol requires uncompressed
accessibility hierarchy dumps. Treat evidence artifacts as sensitive local run
data.

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
`coordinate_frame`,
`input_controller_runtime.summary.input_backend`,
`input_controller_runtime.summary.input_device_command`,
`input_controller_runtime.summary.input_profile`,
`input_controller_runtime.summary.physical_touchscreen_profile_hash`,
`input_controller_runtime.summary.virtual_touchscreen_event_path`,
`input_controller_runtime.summary.command_sequence`,
`input_controller_runtime.summary.current_command`,
`input_controller_runtime.summary.last_command`,
`input_controller_runtime.summary.last_error`, and
`input_controller_runtime.summary.cleanup`. If `--with-evidence` was used, it
should also include `evidence.enabled: true` and `evidence.policy: start_end`.

To derive touch gestures and dismissal inferences from a completed run:

```bash
idk derive presses --recording-dir "runs/$RUN_ID"
```

This writes `derived/press_summaries.jsonl`. Use it for per-key timing,
hold/flight timing, landing geometry, pointer movement, and pressure/contact
ranges. It groups IME records by `press_id`; do not treat it as a direct raw
`getevent` correlation unless a later artifact explicitly adds clock alignment.

Then derive a run-level press summary:

```bash
idk derive summary --recording-dir "runs/$RUN_ID"
```

This writes `derived/run_summary.json`. Use it for aggregate press counts,
semantic key counts, timing distributions, pointer/contact ranges, target
package coverage counts, session provenance, and source freshness. It is derived from
`derived/press_summaries.jsonl`; rerun it after regenerating press summaries.

Then derive touch gestures and dismissal inferences:

```bash
idk derive dismissals --recording-dir "runs/$RUN_ID"
```

The CLI uses the bundled derivation policy by default. Pass `--policy <path>`
only when a protocol requires a local classifier-threshold override. Do not use
input profiles for derivation thresholds; profiles control generated input.

To build an agent-readable cross-source recording timeline:

```bash
idk derive timeline --recording-dir "runs/$RUN_ID"
```

This writes `derived/timeline/index.json` and
`derived/timeline/events.jsonl`. Use the timeline as an index over source
records and evidence artifacts, not as the raw source of truth. Preserve clock
domains and source references when reasoning from it.

To inspect a local recording directory without modifying it:

```bash
idk recording inspect --dir "runs/$RUN_ID"
```

Use `flags.valid_for_analysis`, `flags.needs_validation`,
`flags.needs_press_summaries`, `flags.needs_run_summary`,
`flags.needs_derivation`, and `flags.needs_timeline` to decide the next step.
If `next_actions` is non-empty, prefer those CLI commands over ad hoc file
inspection. The inspection output fingerprints source artifacts and reports
stale summaries and timelines, but it does not rewrite validation or derived
files.

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
With the CLI, start a session, explicitly ensure keyboard visibility when a
non-password editable field should reopen it, and press common keys by semantic
name. Use `type <text>` for ordinary text entry:

```bash
idk session start --run-id "$RUN_ID"
idk keyboard ensure-visible
idk touch doctor
idk type "ab a"
idk press delete
idk press enter
idk hide-keyboard --method edge-back --side right
idk session stop
```

`type <text>` plans the full string from visible layout keys before pressing any
key and fails on unsupported characters or hidden keyboard state without
partial typing. `tap` and `press` also fail when the keyboard is hidden; use
`keyboard ensure-visible` as the explicit recovery command. It uses the focused
non-password editable field first, or the only visible non-password editable
field if none is focused. Use `tap --code=<code>` only when there is no
semantic command. Use `touch tap --x <x> --y <y>` for diagnostic absolute
screen coordinates. Use `touch swipe` and `touch path` only when a protocol
needs raw absolute gesture control; otherwise prefer semantic commands such as
`type`, `press`, and `hide-keyboard`. The CLI routes session input and `touch`
commands through AOSP `/system/bin/uinput` and should fail rather than switch
to another touch backend. The active input profile can sample key-local landing
ratios, hold duration, contact fields, and inter-key delay.

## Validation

Validate pulled JSONL before considering a run complete:

```bash
idk validate "runs/$RUN_ID" --run-id "$RUN_ID"
idk recording inspect --dir "runs/$RUN_ID"
```

Expected validation includes `session_start`, `session_stop`,
`external_run_id`, session-level `input_actor`, optional `input_profile_*`
fields for controller-driven sessions, `target_package`, schema
`input_dynamics_event.v1`, and no `password_field: true` records. Read
`references/jsonl-schema.md` before changing schema, event names, status
fields, or validation logic.
