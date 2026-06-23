# Input Dynamics CLI

The Rust `input-dynamics` CLI is the preferred host-side interface for agents
and scripted local runs. It wraps the ADB control surface, emits JSON, and keeps
raw broadcast command details out of normal workflows.

For control commands, the CLI sends a unique `request_id` and waits until the
app writes a matching command-result JSON file. Stale status files are rejected
instead of being treated as fresh command output.

Build it from a repository checkout:

```bash
cargo build -p input-dynamics
```

Run it through Cargo during development:

```bash
cargo run --quiet -p input-dynamics -- doctor
```

Or run the built binary:

```bash
target/debug/input-dynamics doctor
```

## Device Selection

The CLI targets one Android device at a time. With one connected device, the CLI
selects it automatically and internally runs ADB commands with that serial. With
multiple connected devices, pass `--serial <adb-serial>` to every command:

```bash
adb devices
cargo run --quiet -p input-dynamics -- --serial "$SERIAL" doctor
cargo run --quiet -p input-dynamics -- --serial "$SERIAL" session status
```

Stateful session runtime files are keyed by package and ADB serial, so two
devices do not share controller sockets, state files, or ownership locks. The
mutable control files remain stable for current-session lookup, while each
controller start also creates a preserved diagnostics invocation under
`<runtime>/<package>.<serial>.runs/`.

## Common Workflow

For live agent-driven input, use the stateful session lifecycle. It selects the
IME, starts IME logging, starts one persistent AOSP uinput controller, and then
accepts live `type`, `tap`, and `press` commands:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android

cargo run --quiet -p input-dynamics -- doctor
cargo run --quiet -p input-dynamics -- install
cargo run --quiet -p input-dynamics -- touch doctor
cargo run --quiet -p input-dynamics -- session start --run-id "$RUN_ID"
cargo run --quiet -p input-dynamics -- keyboard ensure-visible
cargo run --quiet -p input-dynamics -- type "ab a"
cargo run --quiet -p input-dynamics -- press delete
cargo run --quiet -p input-dynamics -- session stop
```

This is the canonical live-input path. `session start` must report
`input.ready_for_input: true`, and `keyboard ensure-visible` must report an IME
status with `input_scope_ready: true` before agents send `type`, `tap`, or
`press`. These commands fail instead of injecting input when the controller is
not ready, the IME session is inactive, or the visible keyboard is not attached
to a known non-password field.

Only one stateful session can be active for a package/device runtime at a time.
A competing `session start` for the same package and ADB serial returns JSON
with `ok: false` and `busy: true` before changing IME or logging state. Agents
should treat that as a hard ownership conflict, inspect `session status`, and
wait for the active run to stop.

Stateful sessions use the bundled `profiles/baseline-v1.json` input profile by
default for controller-driven input. To bind a local profile to the whole
session:

```bash
cargo run --quiet -p input-dynamics -- session start \
  --run-id "$RUN_ID" \
  --input-profile ./profiles/custom.json \
  --input-profile-seed 12345
```

The IME JSONL `session_start` record and controller state include
`input_profile_source`, `input_profile_id`, `input_profile_schema`,
`input_profile_hash`, and `input_profile_seed`.

Controller diagnostics are automatic. `session start`, `type`, `tap`, `press`,
and `session stop` write a unified structured event journal at
`controller.events.jsonl` inside the current invocation directory. The journal
contains client and controller request lifecycle events, including request
writes, response reads/timeouts, uinput writes, state writes, response delivery,
and controller exit. Stdout/stderr and final controller state are stored in the
same invocation directory. No separate snapshot command is required before a
new session.

For agent observation, use `observe`. These commands do not start logging and
do not inject input:

```bash
cargo run --quiet -p input-dynamics -- observe state --with-accessibility
cargo run --quiet -p input-dynamics -- observe screenshot --out /tmp/screen.png
cargo run --quiet -p input-dynamics -- observe accessibility --out /tmp/accessibility.xml
cargo run --quiet -p input-dynamics -- observe all --out-dir /tmp/input-dynamics-evidence
```

`observe all` writes `status.json`, `layout.json`, `accessibility.xml`,
`screenshot.png`, `state.json`, and `index.json`. Accessibility dumps and
screenshots may contain visible screen content, so store them with the same care
as run artifacts.

For a complete bounded experiment capture, use `record`. It starts IME logging,
captures a concurrent ADB touchscreen event stream with `getevent`, records
screen video, normalizes the event stream to JSONL, stops the session, pulls
IME logs, writes `manifest.json`, and writes `validation.json`:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-human-android

cargo run --quiet -p input-dynamics -- doctor
cargo run --quiet -p input-dynamics -- install
cargo run --quiet -p input-dynamics -- select-ime
cargo run --quiet -p input-dynamics -- touch doctor
cargo run --quiet -p input-dynamics -- record --run-id "$RUN_ID" --out "runs/$RUN_ID"
```

When `--duration-ms` is omitted, press Enter in the terminal to stop capture
cleanly. For scripted smoke tests, pass `--duration-ms <ms>`.

Video is enabled by default and is part of the canonical recording path. It is
stored under `video/` and treated as sensitive local recording data. Use
`--no-video` only for diagnostics or CI environments where device screen
recording is intentionally unavailable.

To preserve screen context at the beginning and end of a run, add
`--with-evidence`:

```bash
cargo run --quiet -p input-dynamics -- record \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --with-evidence
```

This captures `observe all` bundles under `evidence/start/` and
`evidence/end/`. This is separate from the continuous video captured by
default. Use `--full-accessibility-evidence` only when a protocol needs
uncompressed accessibility hierarchy dumps.

For bounded agent-driven input, add `--with-input-controller`. This starts the
persistent uinput controller for the record window and writes controller
runtime metadata plus cleanup results into `manifest.json`. A second CLI
process can send `type`, `tap`, `press`, `touch swipe`, `touch path`, or
`hide-keyboard` commands while the timed record process is running:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-agent-android

cargo run --quiet -p input-dynamics -- record \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --duration-ms 10000 \
  --with-input-controller \
  --input-actor agent_adb
```

Lower-level session commands remain available for debugging:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android

cargo run --quiet -p input-dynamics -- doctor
cargo run --quiet -p input-dynamics -- install
cargo run --quiet -p input-dynamics -- select-ime
cargo run --quiet -p input-dynamics -- touch doctor
cargo run --quiet -p input-dynamics -- start --run-id "$RUN_ID"
cargo run --quiet -p input-dynamics -- status
cargo run --quiet -p input-dynamics -- layout
cargo run --quiet -p input-dynamics -- stop
cargo run --quiet -p input-dynamics -- pull --out "runs/$RUN_ID"
cargo run --quiet -p input-dynamics -- validate "runs/$RUN_ID" --run-id "$RUN_ID"
```

`install` downloads the latest published debug APK with GitHub CLI when `--apk`
is omitted. To install a local build instead:

```bash
APK="$(ls -t app/build/outputs/apk/debug/*-debug.apk | head -n 1)"
cargo run --quiet -p input-dynamics -- install --apk "$APK"
```

## Machine Output

Commands write JSON to stdout when they receive a structured command result.
`ok: true` returns exit code 0; `ok: false` returns a non-zero exit code.
Local CLI errors, such as invalid arguments or failed host commands, write JSON
or clap text to stderr:

```json
{
  "ok": false,
  "error": "keyboard is hidden (keyboard_view_not_shown); run `input-dynamics keyboard ensure-visible` or focus a non-password editable field first"
}
```

This lets agents branch on exit status and parse command results without
scraping human-oriented text.

## Commands

- global `--serial <adb-serial>`: selects the target device. Required when more
  than one device is connected.
- `doctor`: checks ADB visibility, selected device state, IME registration, and
  GitHub CLI presence.
- `install`: downloads or installs an APK.
- `select-ime`: enables and selects the IME.
- `enable-logging` / `disable-logging`: toggles logging.
- `start --run-id <id>`: enables logging, starts a session, and records
  `external_run_id`. Optional provenance flags are `--input-actor`,
  `--input-controller`, and `--input-cadence-policy`; defaults are `human`,
  null, and `manual`.
- `stop`: stops the active session.
- `status`: returns current control status.
- `session start --run-id <id>`: enables/selects the IME, enables logging,
  starts an IME session, starts a persistent local uinput controller, and binds
  an input profile. If another stateful session is active or starting, it
  returns `busy: true` without changing the active run.
  Defaults are `--input-actor agent_adb`,
  `--input-controller input-dynamics-cli`, and
  `--input-cadence-policy input_profile`.
  - `--input-profile <path>`: local profile JSON for the whole session.
  - `--input-profile-seed <u64>`: explicit seed for reproducible sampled input.
- `session status`: returns IME status plus local input-controller status. When
  the uinput controller is active, `input.state` includes the physical
  touchscreen profile hash, input profile summary, mirrored virtual touchscreen
  event path, Event Hub device metadata, Input Reader device metadata when
  Android exposes them, and compact controller command diagnostics:
  `current_command`, `last_command`, `last_error`, and `command_sequence`.
  `input.runtime.current_invocation` points to the active/latest diagnostics
  directory and event journal.
- `session stop`: stops the local input controller, verifies normal runtime
  cleanup, verifies that the mirrored virtual touchscreen event path has
  disappeared when it was detected, then stops IME logging. If a previous
  controller process was interrupted, `session stop` also removes stale runtime
  files and reports cleanup from the saved state. Final controller state and
  final session lock are preserved in the diagnostics invocation directory.
- `layout`: returns status including `keyboard_layout` when the IME is visible.
  Layout output includes keyboard-view bounds, display size/rotation, and
  screen-space key centers for coordinate calibration.
- `layout --wait-visible` / `layout --wait-hidden`: waits for keyboard layout
  visibility state before returning.
- `keyboard ensure-visible`: establishes the single ready state required for
  logged live input. When the keyboard is already visible, it still requires
  `input_scope_ready: true`; a merely visible keyboard is not enough. Otherwise,
  with an active stateful session, it captures the current accessibility
  hierarchy, taps the focused non-password editable field through the uinput
  controller, waits for visible layout state, then waits for
  `input_scope_ready: true`. If no editable field is focused, it may tap the
  only visible non-password editable field. It fails rather than guessing when
  there is no such field, more than one candidate, or no loggable input scope.
- `observe accessibility [--out <xml>] [--full]`: captures the current Android
  accessibility hierarchy with `uiautomator dump`. Without `--out`, the XML is
  included in JSON output. With `--out`, the XML is written to that path and
  stdout includes a compact summary.
- `observe screenshot --out <png>`: captures the current device screen with
  `screencap`.
- `observe layout [--wait-visible|--wait-hidden]`: reads the same keyboard
  layout state as `layout` under the observation namespace.
- `observe state [--with-accessibility] [--screenshot-out <png>]`: returns IME
  status, keyboard layout, and optional accessibility/screenshot evidence in
  one JSON object.
- `observe all --out-dir <dir>`: writes a complete observation bundle with
  status, layout, accessibility XML, screenshot PNG, state JSON, and bundle
  index JSON.
- `hide-keyboard`: dismisses the visible IME with a stateful uinput edge-back
  gesture and waits for hidden layout state. Options include
  `--method edge-back`, `--side left|right`, and ratio-based geometry overrides.
- `tap --label <label>` or `tap --code <code>`: taps a key from layout data
  through the active session controller. Fails unless the controller reports
  `ready_for_input: true` and the IME reports `input_scope_ready: true`.
- `press delete`, `press enter`, `press space`: taps common keys by semantic
  name through the active session controller. Uses the same readiness gate as
  `tap`.
- `type <text>`: types text through visible layout keys in the active session.
  The command plans the full string before pressing any key; unsupported
  characters, inactive controller state, hidden keyboard state, and missing
  input-scope readiness fail without partial typing. The active input profile
  can sample key-local landing ratios, hold duration, contact fields, and
  inter-key delay.
  `--inter-key-delay-ms`, default `40`, is used when the active controller does
  not provide an inter-key delay sample.
- `touch doctor`: checks AOSP uinput availability and reports the mirrored
  physical touchscreen profile used by the CLI.
- `touch tap --x <x> --y <y> [--hold-ms <ms>]`: sends an absolute screen tap
  through AOSP uinput. Use `--hold-ms` for one-shot long presses; there is no
  separate `touch hold` command.
- `touch swipe --from-x <x> --from-y <y> --to-x <x> --to-y <y>`: sends an
  absolute swipe through the active session controller.
- `touch path --points-json '<json>'` or `touch path --points-file <path>`:
  sends an absolute point path through the active session controller. Points may
  be `[{"x":1,"y":2}]` objects or `[[1,2]]` arrays.
- `list-logs`: lists log files.
- `clear-logs`: clears logs when no session is active.
- `pull --out <dir>`: pulls app-specific external log storage.
- `validate <path> --run-id <id>`: validates JSONL lifecycle and safety fields.
- `getevent normalize --input <raw.log> --output <events.jsonl>`: parses
  `adb shell getevent -lt` output into neutral JSONL records with schema
  `input_dynamics_getevent.v1`.
- `record --run-id <id> --out <dir>`: records a complete run with IME JSONL,
  ADB `getevent` raw and normalized touch streams, default screen video,
  manifest, pull, and validation output. Use `--duration-ms <ms>` for timed
  runs; otherwise press Enter to stop. Add `--with-input-controller` for
  bounded agent-driven runs that need persistent uinput controller metadata in
  the manifest. Add `--with-evidence` to capture start/end observation bundles
  containing accessibility XML, screenshot PNG, status, layout, state, and
  index JSON. Use `--no-video` only as an explicit diagnostic or CI escape
  hatch.
- `derive presses --recording-dir <dir>`: derives per-press summaries from IME
  JSONL records. The output includes key timing, hold/flight timing, landing
  geometry, pointer movement, and pressure/contact ranges. It does not infer
  raw `getevent` correlation; clock alignment is explicitly `not_estimated`.
- `derive summary --recording-dir <dir>`: derives a run-level JSON summary from
  `derived/press_summaries.jsonl`, including aggregate counts, timing/contact
  ranges, provenance, and source freshness metadata.
- `derive dismissals --recording-dir <dir>`: derives touch gestures and dismissal
  inferences from a recording. The command reads coordinate facts from
  `manifest.json` and uses the bundled derivation policy by default. Pass
  `--policy <path>` for a local policy override. Until a validated alignment
  transform exists, dismissal inference does not join IME lifecycle uptime to
  raw `getevent` gestures. Records are marked
  `clock_alignment_status: "unsupported_clock_domain"` when getevent
  correlation is unavailable.
- `derive timeline --recording-dir <dir>`: writes a cross-source recording
  timeline bundle under `derived/timeline/`. Timeline rows reference source
  records and evidence artifacts; raw streams remain canonical.
- `recording inspect --dir <dir>`: inspects a local recording directory without
  modifying it. Output includes artifact metadata, current validation state,
  derived timeline staleness, analysis-readiness flags, and suggested next CLI
  actions.

Use `type <text>` for ordinary text entry. Use semantic `press` commands for
common non-letter keys and corrections. `tap --code=-7` still works for delete,
and `tap --code -7` is also accepted, but semantic commands are easier for
agents to generate correctly. `type`, `tap`, `press`, `hide-keyboard`, `touch
swipe`, and `touch path` require `session start`; `type`, `tap`, and `press`
also require visible keyboard layout state and IME `input_scope_ready: true`.
Run `keyboard ensure-visible` to establish that state. Use `touch tap` only for
low-level diagnostic absolute taps.

`session` input and `touch` commands use AOSP `/system/bin/uinput` for
touchscreen input. They fail if the device does not expose that command instead
of falling back to a second touch implementation.

## Record Output

`record` creates:

```text
<out>/
  manifest.json
  validation.json
  ime/
    session-*.jsonl
    input_dynamics_control_status.json
  adb/
    getevent.raw.log
    getevent.jsonl
    getevent.stderr.log
  video/
    screen.mp4
    timing.json
    screenrecord.stdout.log
    screenrecord.stderr.log
    adb-pull-video.log
  derived/
    run_summary.json
  evidence/        # only when --with-evidence is used
    start/
      accessibility.xml
      screenshot.png
      status.json
      layout.json
      state.json
      index.json
    end/
      accessibility.xml
      screenshot.png
      status.json
      layout.json
      state.json
      index.json
```

`manifest.json` also includes `coordinate_frame`, derived from the recorded
physical touchscreen profile and keyboard layout snapshots. Analysis commands
use this recorded frame instead of accepting screen geometry on the command
line.

By default, `manifest.json` includes `video.enabled: true`,
`video.required: true`, `video.local_path`, `video.timing_path`,
`video.remote_path`, start/stop timing brackets, the raw screenrecord command,
cleanup status, and the pulled `screen.mp4` file fingerprint. The companion
`video/timing.json` repeats the video metadata next to the media file for
artifact-local inspection.

Timing fields use explicit clock domains. Android input event timestamps are
in the Android uptime domain, `t_elapsed_realtime_ns` is a record/status
elapsed-realtime timestamp, raw `getevent` timestamps stay in the
`kernel_getevent_us` domain until aligned, and video frame timestamps will use
media PTS. Do not treat wall-clock fields as ordering truth; they are
diagnostic/provenance fields.

Screenrecord start/stop timing samples are captured through the same app
`STATUS` request-result path used by `input-dynamics status`. Each marker
contains a validated `device_clock_probe` with schema
`input_dynamics_device_clock_probe.v1`, plus host wall and host process
monotonic brackets as diagnostics. The probe's canonical device timestamp is
`device_elapsed_realtime_ns`; uptime is Android uptime metadata and wall time is
diagnostic. If the request-correlated status result or probe is missing or
invalid, recording fails instead of falling back to shell wall time.

The public vocabulary is strict for writers and validators. Existing derived
artifacts that predate this vocabulary may still contain labels such as
`ime_uptime_ms`, `getevent_time_us`, or
`host_wall_ms_bracketed_device_epoch_ms`; treat those as legacy, not canonical.
Future records that carry event, capture, and write timestamps should describe
each timestamp role separately instead of relying on one record-level
`clock_domain`.

When `record` is run with `--with-evidence`, `manifest.json` includes
`evidence.enabled: true`, `evidence.policy: start_end`, and the start/end
observation bundle metadata. Each evidence phase is bracketed by the same
request-correlated `device_clock_probe` helper used for screenrecord anchors:
`before_evidence_start`, `after_evidence_start`, `before_evidence_end`, and
`after_evidence_end`. Accessibility dumps and screenshots may contain visible
screen content; keep them with the same care as raw run artifacts.

When `record` is run with `--with-input-controller`, `manifest.json` includes:

- `input_controller_runtime.summary.input_backend`
- `input_controller_runtime.summary.input_device_command`
- `input_controller_runtime.summary.input_profile`
- `input_controller_runtime.summary.physical_touchscreen_profile_hash`
- `input_controller_runtime.summary.physical_touchscreen`
- `input_controller_runtime.summary.virtual_touchscreen`
- `input_controller_runtime.summary.virtual_touchscreen_event_path`
- `input_controller_runtime.summary.command_sequence`
- `input_controller_runtime.summary.current_command`
- `input_controller_runtime.summary.last_command`
- `input_controller_runtime.summary.last_error`
- `input_controller_runtime.summary.cleanup`

The `adb/getevent.raw.log` stream is device-level touchscreen data.
`adb/getevent.jsonl` is a normalized form of that stream. It includes
`device_added`, `input_event`, and reconstructed `touch_frame` records. Keep ADB
device-level records separate from IME-owned JSONL records when analyzing
password-field suppression or keyboard-local privacy guarantees.

## Derived Output

Before deriving or analyzing a local recording, inspect it:

```bash
input-dynamics recording inspect --dir "runs/$RUN_ID"
```

This command is read-only. It reports the selected IME session JSONL file,
record counts, artifact fingerprints, video presence and staleness,
stored-versus-current validation state, timeline source staleness, and boolean
flags such as `valid_for_analysis`, `has_video`, `needs_video`,
`canonical_clock_ready`, `has_legacy_timing`, `needs_canonical_recording`,
`needs_validation`, `needs_press_summaries`, `needs_run_summary`,
`needs_derivation`, and `needs_timeline`.

The `clock` object classifies saved video and evidence anchors as
`bracketed`, `legacy_wall_clock_bracketed`, `missing_source`, `stale_inputs`,
`probe_failed`, `not_requested`, or `not_estimated`. Legacy wall-clock timing
remains readable, but it does not set `canonical_clock_ready`. Required missing
or stale video makes `valid_for_analysis` false and adds a canonical
`record_with_video` action. Missing, legacy, stale, or malformed canonical
clock anchors add a canonical recorder action. `next_actions` contains local
CLI commands an agent can run to refresh missing or stale artifacts. It does
not probe the device or derive new clock alignment.

Treat `valid_for_analysis` as a base artifact/readability gate. For any
video/evidence anchor claim, cross-source timeline claim, or ordering claim that
depends on canonical clocks, also require `flags.canonical_clock_ready: true`,
`flags.needs_canonical_recording: false`, and no recorder rerun action in
`next_actions`.

Derived artifacts preserve source timing and normalized timing separately.
Compatibility fields such as `t_uptime_ms`, `t_event_uptime_ms`, and
`t_getevent_us` remain readable, but current derived rows also include explicit
clock metadata:

- `source_time` or `time` names the source clock domain, source field, precision,
  status, and raw source value or interval.
- `normalized_time.status` describes the normalized-time claim. A row supports
  normalized ordering only when `normalized_time.clock_domain` and either
  `normalized_time.time_ns` or `normalized_time.time_interval_ns` are non-null
  and the row's ordering metadata allows that use. `unsupported_clock_domain`
  means the row is not comparable across sources. `legacy_wall_clock_bracketed`
  means old wall-clock timing is readable but not canonical. `bracketed` is used
  only when a saved artifact supplies an explicit bracket or transform.
- Timeline `ordering.canonical_cross_source_order: false` means row order is a
  deterministic inspection order unless the row carries a valid normalized-time
  claim.

After recording with the current CLI:

```bash
input-dynamics derive presses --recording-dir "runs/$RUN_ID"
```

This writes:

- `derived/press_summaries.jsonl`

Each `press_summary` row groups IME `pointer_sample`, `key_down`, `key_up`,
`key_commit`, and repeat/long-press/cancel records by `press_id`. It preserves
source line indexes, reports `hold_ms`, `down_to_commit_ms`,
`flight_since_previous_commit_ms`, key landing geometry, pointer movement, and
pressure/contact statistics. `clock_alignment.getevent` is `not_estimated`
because raw device events do not carry `press_id`. `timing_clock` reports
whether press timing came from current event-time metadata, legacy
`t_event_uptime_ms`, legacy writer-time fallback, or missing source timing.
`canonical_event_time_metadata` is the current source-event path;
`legacy_t_event_uptime_ms` is legacy-compatible source-event timing;
`legacy_t_uptime_ms_fallback` is writer-time compatibility only; `missing`
blocks source-event timing claims.

```bash
input-dynamics derive summary --recording-dir "runs/$RUN_ID"
```

This writes:

- `derived/run_summary.json`

`run_summary.json` aggregates `press_summary` rows into run-level press counts,
semantic key counts, hold/flight/pointer timing distributions, pressure and
contact ranges, landing summaries, target package coverage counts, session
provenance, and a fingerprint of the source `press_summaries.jsonl`.
`recording inspect` reports the summary as stale if the press-summary source
changes.

```bash
input-dynamics derive dismissals --recording-dir "runs/$RUN_ID"
```

This writes:

- `derived/touch_gestures.jsonl`
- `derived/dismissal_inferences.jsonl`

`dismissal_inferences.jsonl` does not infer a getevent-backed dismissal cause
until a validated alignment transform exists. Treat records with
`clock_alignment_status: "unsupported_clock_domain"` as IME lifecycle evidence
only, not as aligned getevent timing evidence. Current or older readable rows
may carry compatibility `time_delta_ms`; `time_delta_status:
"legacy_mixed_clock_heuristic"` is provenance only, not aligned timing.
`touch_gestures.jsonl` declares `kernel_getevent_us` source timing and
`unsupported_clock_domain` cross-source alignment until a transform exists.

To create a cross-source timeline index:

```bash
input-dynamics derive timeline --recording-dir "runs/$RUN_ID"
```

This writes:

```text
derived/
  timeline/
    index.json
    events.jsonl
```

`derived/timeline/events.jsonl` contains semantic IME events, derived touch
gestures, dismissal inferences, video start/stop markers when timing metadata
is present, and optional evidence bundle markers. It does not copy low-level
raw `getevent` rows by default and does not embed screenshot, accessibility, or
video payloads. Each row keeps source references, an explicit source clock
domain, `source_time`, `normalized_time`, and ordering metadata. Manifest-backed
evidence brackets use `device_elapsed_realtime_ns`; index-only evidence timing
remains readable as legacy `host_wall_ms` provenance.
`derived/timeline/index.json` records selected sources, counts, fingerprints,
output paths, warnings, clock-domain counts, normalized-status counts, and the
clock-alignment status.

To use a local classifier-threshold policy:

```bash
input-dynamics derive dismissals \
  --recording-dir "runs/$RUN_ID" \
  --policy ./policies/custom-derivation.json
```

Input profiles and derivation policies are separate. `--input-profile` controls
controller-generated input. `--policy` controls recording interpretation.
Derived timeline artifacts are sensitive local recording data; keep them with
the same care as raw JSONL, screenshots, and accessibility dumps.

Use [adb-control.md](adb-control.md) when debugging the lower-level broadcast
surface directly.
