---
name: input-dynamics-keyboard
description: Run and validate local Android Input Dynamics Keyboard sessions over ADB. Use when Codex needs to download or build the Android IME APK, install it on a device, enable or select it, start/status/stop a complete input dynamics session with screen video and an external run_id, run bounded session smoke captures, inspect keyboard layout, accessibility, or screenshot evidence, avoid screenshot-dependent keyboard automation, pull JSONL logs, or validate the password-field suppression boundary.
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
- Complete sessions capture screen video by default. Treat `video/screen.mp4` and
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
fails rather than guessing when multiple devices are visible. Controller runtime
state is keyed by package and ADB serial. Each controller start also creates a
preserved diagnostics invocation directory under the runtime root, exposed in
`controller status` as `input.runtime.current_invocation`.

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

3. For a complete human-operated observation session, use `session` with an
   external run id:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
idk session start --input-actor human --run-id "$RUN_ID" --out "runs/$RUN_ID"
idk session status --run-id "$RUN_ID"
```

Use the device, then finalize the run:

```bash
idk session stop --run-id "$RUN_ID"
idk recording inspect --dir "runs/$RUN_ID"
```

If `session status` reports any `failure_conditions`, still stop the matching
run id and inspect the recording directory. Prefer `recommended_argv` when
present. `video_ended_early` is only one example; required non-video capture
process failures use generic `required_process_*` codes. Do not analyze an
incomplete run as complete.

For start/end screen context, add `--with-evidence` to `session start`. Unless
`--no-video` is explicitly used, complete sessions include screen video,
request-correlated timing metadata, IME logs, ADB touchscreen capture,
normalization, validation, and a manifest after stop/finalization.

4. For a bounded smoke capture, use `session run --duration-ms`. This is for
   explicit smoke tests or externally bounded capture windows; it uses the same
   umbrella lifecycle and finalization path as `session start/status/stop`.
   Current complete-session examples use `--input-actor human`; agent-owned
   complete sessions remain reserved until the agent umbrella lifecycle exists:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-smoke
idk session run \
  --input-actor human \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --duration-ms 10000
idk recording inspect --dir "runs/$RUN_ID"
```

Do not use bounded `session run` as a semantic refresh for a human interaction
recording unless the task explicitly asks for a bounded smoke window.

5. For diagnostic live agent-driven input, start one controller run:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android
idk controller start --run-id "$RUN_ID"
```

`controller start` uses the bundled `profiles/baseline-v1.json` input profile
by default for controller-driven input. Its default provenance is
`input_actor=agent_adb`, `input_controller=input-dynamics-cli`, and
`input_cadence_policy=input_profile`. To bind a local profile to the whole
controller run, pass `--input-profile <path>` and optionally
`--input-profile-seed <u64>` for reproducible sampling:

```bash
idk controller start \
  --run-id "$RUN_ID" \
  --input-profile ./profiles/custom.json \
  --input-profile-seed 12345
```

Only one diagnostic controller can be active for a package/device runtime. If
`controller start` returns `ok: false` with `busy: true`, do not retry with
lower-level commands; inspect `idk controller status` and wait for the active
controller to stop.

Then use live commands while the diagnostic controller is active:

```bash
idk keyboard ensure-visible
idk type "ab a"
idk press delete
idk hide-keyboard --method edge-back --side right
idk controller stop
```

This is a diagnostic live-input path, not a complete recording workflow.
`controller start` must return `input.ready_for_input: true`, and
`keyboard ensure-visible` must return an IME status with
`input_scope_ready: true`, before calling `type`, `tap`, or `press`. If those
commands fail with `input_scope_state` or `session_lock.state` errors, do not
inject input through another mechanism; inspect `idk controller status` and
stop the controller if needed.

If a controller process is interrupted, use `idk controller stop` as the repair
path. It removes stale runtime files, stops IME logging, and reports whether
the saved virtual touchscreen event path is gone. For controller timing or
socket failures, inspect `idk controller status`; `input.state` exposes
`current_command`, `last_command`, `last_error`, and `command_sequence`.
Also inspect `input.runtime.current_invocation.invocation.events`; it is a
unified `controller.events.jsonl` journal with client and controller request
events, response timeouts/write failures, uinput writes, state writes, and
controller exit. Do not rely on manual `/tmp` snapshots for normal forensics.

If an old hidden session-lifecycle instruction returns
`error_code: "command_moved"`, follow `moved_to.argv` or
`suggested_next_command.argv`; do not retry the old namespace.

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
`flags.has_video_map`, `flags.needs_video_map`,
`flags.session_classification`, `flags.lifecycle_complete`,
`flags.lifecycle_incomplete`, `flags.lifecycle_active`,
`flags.lifecycle_in_progress`, `flags.needs_session_stop`, and
`flags.needs_session_repair`, `flags.video_ended_early`,
`flags.required_process_failed`, `flags.required_process_failure_codes`, and
`flags.needs_session_rerun` to decide the next step. Branch on
`session_classification` first: `complete` may continue to artifact/timing gates;
`active` follows `session_stop`; `in_progress` follows `session_status` and then
inspects again; `incomplete`, `aborted`, and `repair_required` are not complete
recordings and must not be derived or analyzed as if they were complete.
If `video_ended_early` or `required_process_failed` is true, preserve the failed
run for diagnostics and run a new capture through `session start`,
`session status`, `session stop`, and `recording inspect`.
Required missing or stale video makes `valid_for_analysis` false. The `clock`
object classifies saved video/evidence anchors as `bracketed`,
`legacy_wall_clock_bracketed`, `missing_source`, `stale_inputs`,
`probe_failed`, `not_requested`, or `not_estimated`. If `next_actions` is
non-empty, prefer those CLI command templates over ad hoc file inspection. Each
`next_actions` item has `kind`, `command`, and `reason`; branch on `kind`.
Session refresh actions include placeholders such as `<new-run-id>` and
`<new-run-dir>`; fill them before running any command. These actions also carry
`workflow: session_start_status_stop_inspect` and a structured `commands` array
with `start`, `status`, `stop`, and `inspect` steps. The compatibility
`command` field mirrors the first `start` step only; execute the full
`commands` sequence for a complete observation refresh.
Current action kinds are `validate`, `session_with_video`,
`session_with_canonical_clocks`, `session_rerun`, `session_stop`,
`session_status`, `session_repair_required`, `derive_presses`, `derive_summary`,
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
- `idk ime status` and recording video markers use the app `STATUS`
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
  session artifacts and `recording inspect`; raw ADB status files are for
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
`flags.needs_canonical_recording: false`, and no `session_with_video` or
`session_with_canonical_clocks` action.

5. Diagnostic-only: use lower-level status and layout commands when debugging
   the raw IME control surface:

```bash
idk ime status
idk layout
```

6. Diagnostic-only: if investigating the raw IME lifecycle outside the normal
   capture workflow, stop, pull, and validate logs manually:

```bash
idk ime stop
idk ime pull --out "runs/$RUN_ID"
idk validate "runs/$RUN_ID" --run-id "$RUN_ID"
```

If the CLI is unavailable during a normal run, fail clearly and report why.
Use `references/adb-control.md` only as a raw protocol reference for manual
diagnostics.

## Non-Screenshot Automation

When the keyboard view is visible, use `KEYBOARD_LAYOUT` instead of screenshots.
With the CLI, start the diagnostic controller, explicitly ensure keyboard
visibility when a non-password editable field should reopen it, and press common
keys by semantic name. Use `type <text>` for ordinary text entry:

```bash
idk controller start --run-id "$RUN_ID"
idk keyboard ensure-visible
idk touch doctor
idk type "ab a"
idk press delete
idk press enter
idk hide-keyboard --method edge-back --side right
idk controller stop
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
`type`, `press`, and `hide-keyboard`. The CLI routes controller-backed input
and `touch` commands through AOSP `/system/bin/uinput` and should fail rather
than switch to another touch backend. The active input profile can sample
key-local landing ratios, hold duration, contact fields, and inter-key delay.

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
