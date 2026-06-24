---
name: input-dynamics-keyboard
description: Run and validate local Android Input Dynamics Keyboard sessions over ADB. Use when Codex needs to download or build the Android IME APK, install it on a device, enable or select it, record a bounded input dynamics run with screen video and an external run_id, inspect status, keyboard layout, accessibility, or screenshot evidence, avoid screenshot-dependent keyboard automation, pull JSONL logs, or validate the password-field suppression boundary.
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
- `record` captures screen video by default. Treat `video/screen.mp4` and
  `video/timing.json` as sensitive local run artifacts. Use `--no-video` only
  for explicit diagnostics or CI, not for normal bounded capture.
- Treat everything under `derived/video_map/` as sensitive local run artifacts.
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
state is keyed by package and ADB serial. Each controller start also creates a
preserved diagnostics invocation directory under the runtime root, exposed in
`session status` as `input.runtime.current_invocation`.

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

This is the only normal live-input path for agents. `session start` must return
`input.ready_for_input: true`, and `keyboard ensure-visible` must return an IME
status with `input_scope_ready: true`, before calling `type`, `tap`, or
`press`. If those commands fail with `input_scope_state` or
`session_lock.state` errors, do not inject input through another mechanism;
repair the session state or stop the session.

If a controller process is interrupted, use `idk session stop` as the repair
path. It removes stale runtime files, stops IME logging, and reports whether
the saved virtual touchscreen event path is gone. For controller timing or
socket failures, inspect `idk session status`; `input.state` exposes
`current_command`, `last_command`, `last_error`, and `command_sequence`.
Also inspect `input.runtime.current_invocation.invocation.events`; it is a
unified `controller.events.jsonl` journal with client and controller request
events, response timeouts/write failures, uinput writes, state writes, and
controller exit. Do not rely on manual `/tmp` snapshots for normal forensics.

4. For bounded capture, use `record` with an external run id:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
idk record --run-id "$RUN_ID" --out "runs/$RUN_ID" --duration-ms 10000
```

Always pass a positive `--duration-ms <ms>` for agent-run or scripted captures.
Open-ended `record` is disabled during the session-workflow migration; omitting
`--duration-ms` is a hard error. Do not use open-ended `record` for automated or
agent-driven human-operated runs.

The command writes `manifest.json`, `validation.json`, `ime/`, `adb/`,
`video/`, and `derived/` under the output directory. The
`adb/getevent.raw.log` stream is device-level touchscreen data and should be
analyzed separately from IME-owned JSONL privacy guarantees. The
`video/screen.mp4` artifact is device-level visual context and is sensitive
local run data. Current manifests include `coordinate_frame`, derived from
recorded touchscreen profile and layout snapshots, plus `video.enabled`,
`video.required`, timing brackets, and the pulled video fingerprint.

When a run needs screen context, add `--with-evidence`. This writes start/end
observation bundles under `evidence/start/` and `evidence/end/`, each with
accessibility XML, screenshot PNG, status JSON, layout JSON, state JSON, and
index JSON:

```bash
idk record \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --duration-ms 10000 \
  --with-evidence
```

These bundles are separate from the continuous video captured by default. Use
`--full-accessibility-evidence` only when a protocol requires uncompressed
accessibility hierarchy dumps. Treat evidence artifacts as sensitive local run
data. Current evidence phase metadata is bracketed by the same STATUS-backed
`device_clock_probe` helper as screenrecord timing, with
`before_evidence_start`, `after_evidence_start`, `before_evidence_end`, and
`after_evidence_end` phases.

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

Do not send input after a fixed sleep. In the second CLI process, poll
`idk session status` until `input.session_lock.state: "active"` and
`input.ready_for_input: true`. For `type`, `press`, and keyboard-scoped
commands, also require `ime.input_scope_ready: true`; if it is false, establish
the input scope with the canonical UI step for the run, then poll status again.

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
Unless `--no-video` was explicitly used, it should include `video.enabled:
true`, `video.required: true`, `video.local_path`, and a non-null
`video.file.sha256`.

Before deriving or interpreting a completed run, inspect it:

```bash
idk recording inspect --dir "runs/$RUN_ID"
```

Use `flags.valid_for_analysis`, `flags.needs_validation`,
`flags.has_video`, `flags.needs_video`, `flags.canonical_clock_ready`,
`flags.has_legacy_timing`, `flags.needs_canonical_recording`,
`flags.needs_press_summaries`, `flags.needs_run_summary`,
`flags.needs_derivation`, `flags.needs_timeline`,
`flags.has_video_frame_index`, `flags.needs_video_frame_index`,
`flags.has_video_map`, and `flags.needs_video_map` to decide the next step.
Required missing or stale video makes `valid_for_analysis` false. The `clock`
object classifies saved video/evidence anchors as `bracketed`,
`legacy_wall_clock_bracketed`, `missing_source`, `stale_inputs`,
`probe_failed`, `not_requested`, or `not_estimated`. If `next_actions` is
non-empty, prefer those CLI command templates over ad hoc file inspection. Each
`next_actions` item has `kind`, `command`, and `reason`; branch on `kind`.
Recorder actions include placeholders such as `<new-run-id>`, `<new-run-dir>`,
and `<positive-ms>`; fill them before running the command.
Current action kinds are `validate`, `record_with_video`,
`record_with_canonical_clocks`, `derive_presses`, `derive_summary`,
`derive_dismissals`, `derive_timeline`, and `derive_video_map`.
Use `has_video_frame_index` only for encoded frame metadata readiness. Use
`has_video_map` for event-frame windows. Canonical timing confidence is
reported separately by `canonical_clock_ready` and `needs_canonical_recording`.

Read `validation.current.clock_validation` from the same inspect output when
diagnosing timestamp metadata. Stable keys are
`timestamp_metadata_record_count`, `legacy_timestamp_metadata_missing_count`,
`missing_timestamp_role_count`, `missing_clock_domain_count`,
`invalid_clock_domain_count`, `invalid_timestamp_source_count`,
`invalid_timestamp_precision_count`, `timestamp_role_mismatch_count`,
`timestamp_field_reference_error_count`, `timestamp_unit_mismatch_count`,
`timestamp_order_violation_count`, and `mixed_clock_domain_claim_count`.
`invalid_timestamp_metadata_count` remains only a compatibility rollup.

To derive per-press summaries from an inspected run:

```bash
idk derive presses --recording-dir "runs/$RUN_ID"
```

This writes `derived/press_summaries.jsonl`. Use it for per-key timing,
hold/flight timing, landing geometry, pointer movement, and pressure/contact
ranges. It groups IME records by `press_id`; do not treat it as a direct raw
`getevent` correlation unless a later artifact explicitly adds clock alignment.

Clock reasoning rules for agents:

- Canonical clock domains are `android_uptime_ms`, `android_uptime_ns`,
  `device_elapsed_realtime_ns`, `kernel_getevent_us`, `media_pts_ns`,
  `host_process_monotonic_ns`, `host_wall_ms`, and `device_wall_ms`.
- `idk status` and recording video markers use the app `STATUS`
  request-result path. The canonical status/control sample is
  `device_clock_probe` with schema `input_dynamics_device_clock_probe.v1`.
- Treat `device_clock_probe.t_elapsed_realtime_ns` as the canonical device
  monotonic status/control timestamp. Treat uptime fields as Android uptime
  metadata and wall fields as diagnostics.
- Do not subtract or order across different clock domains unless a derived
  artifact provides a transform, uncertainty, and an alignment status.
- Derived rows separate source timing from normalized timing. Read `source_time`
  or `time` for the source domain/value. For normalized timing, status alone is
  not enough: require a non-null `normalized_time.clock_domain` and either
  `normalized_time.time_ns` or `normalized_time.time_interval_ns`.
- Timeline row order is deterministic inspection order unless
  populated `normalized_time` fields and ordering metadata say a mapped
  normalized timestamp can be used. Do not treat
  `ordering.canonical_cross_source_order: false` as event chronology.
- Press `timing_clock.source_time_status_counts` has four important buckets:
  `canonical_event_time_metadata` is the current source-event path;
  `legacy_t_event_uptime_ms` is legacy-compatible source-event timing;
  `legacy_t_uptime_ms_fallback` is writer-time compatibility only; `missing`
  blocks source-event timing claims.
- Treat `legacy_wall_clock_bracketed` and `estimated` as lower-confidence
  timing claims, `unsupported_clock_domain` as not comparable, and
  `stale_inputs`, `missing_source`, `outside_range`, or `probe_failed` as
  blockers for the affected timing claim.
- Treat older labels such as `ime_uptime_ms`, `getevent_time_us`, and
  `host_wall_ms_bracketed_device_epoch_ms` as pre-vocabulary legacy labels, not
  canonical domains.
- Do not invent lower-level clock workflows during normal runs. Use saved
  `record` artifacts and `recording inspect`; raw ADB status files are for
  diagnostics.

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
Until a validated alignment transform exists, dismissal inference does not join
IME lifecycle events to raw `getevent` gestures. Records with
`clock_alignment_status: "unsupported_clock_domain"` are IME lifecycle evidence
only, not aligned getevent timing evidence. Current or older readable rows may
carry compatibility `time_delta_ms`; `time_delta_status:
"legacy_mixed_clock_heuristic"` is provenance only, not aligned timing.

To build an agent-readable cross-source recording timeline:

```bash
idk derive timeline --recording-dir "runs/$RUN_ID"
```

This writes `derived/timeline/index.json` and
`derived/timeline/events.jsonl`. Use the timeline as an index over source
records and evidence artifacts, not as the raw source of truth. Preserve clock
domains and source references when reasoning from it. Use
`timeline.artifact_diagnostics` from `recording inspect` or
`derived/timeline/index.json`; stable keys are
`missing_clock_domain_count`, `invalid_clock_domain_count`,
`mixed_clock_domain_claim_count`,
`mixed_clock_domain_without_alignment_count`,
`normalized_claim_without_domain_count`, and `unit_mismatch_count`.

To build timestamped event-frame windows:

```bash
idk derive video-map --recording-dir "runs/$RUN_ID"
```

This requires `ffprobe` from FFmpeg and a fresh `derived/timeline/` bundle. It
writes `derived/video_map/index.json`, `frames.jsonl`, `alignment.json`, and
`event_frames.jsonl`. Frame rows use schema `input_dynamics_video_frame.v1` in
the `media_pts_ns` clock domain. Event-frame rows use schema
`input_dynamics_event_video_frame_map.v1` and carry per-event
`mapping_status`, uncertainty, and candidate frame windows when mapping is
supported. Agents must not treat frame windows as visual interpretation; branch
on per-row status and uncertainty. Keep the whole `derived/video_map/`
directory with the same care as raw JSONL, screen video, screenshots,
accessibility dumps, and timeline artifacts.

After deriving, inspect again if freshness matters:

```bash
idk recording inspect --dir "runs/$RUN_ID"
```

The inspection output fingerprints source artifacts and reports stale video,
summaries, timelines, video frame indexes, full video-map readiness, and
clock-anchor readiness, but it does not rewrite validation or derived files.

Do not treat `valid_for_analysis: true` as enough for clock-dependent claims.
For video/evidence anchors, cross-source timeline claims, or ordering claims
that depend on canonical clocks, require `flags.canonical_clock_ready: true`,
`flags.needs_canonical_recording: false`, and no `record_with_video` or
`record_with_canonical_clocks` action.

5. Diagnostic-only: use lower-level status and layout commands when debugging
   the raw IME control surface:

```bash
idk status
idk layout
```

6. Diagnostic-only: if investigating the raw IME lifecycle outside the normal
   capture workflow, stop, pull, and validate logs manually:

```bash
idk stop
idk pull --out "runs/$RUN_ID"
idk validate "runs/$RUN_ID" --run-id "$RUN_ID"
```

If the CLI is unavailable during a normal run, fail clearly and report why.
Use `references/adb-control.md` only as a raw protocol reference for manual
diagnostics.

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
key and fails on unsupported characters, hidden keyboard state, controller
readiness failure, or missing IME input-scope readiness without partial typing.
`tap` and `press` use the same readiness gate. Use `keyboard ensure-visible` as
the explicit setup and recovery command. It uses the focused non-password
editable field first, or the only visible non-password editable field if none
is focused. Use `tap --code=<code>` only when there is no semantic command. Use
`touch tap --x <x> --y <y>` for diagnostic absolute screen coordinates. Use
`touch tap --hold-ms <ms>` for one-shot long presses; do not invent a separate
`touch hold` workflow. Use `touch swipe` and `touch path` only when a protocol
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
