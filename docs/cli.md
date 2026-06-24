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

`derive video-map` also requires `ffprobe` from FFmpeg. The repository Nix
development shell includes it.

## Device Selection

The CLI targets one Android device at a time. With one connected device, the CLI
selects it automatically and internally runs ADB commands with that serial. With
multiple connected devices, pass `--serial <adb-serial>` to every command:

```bash
adb devices
cargo run --quiet -p input-dynamics -- --serial "$SERIAL" doctor
cargo run --quiet -p input-dynamics -- --serial "$SERIAL" controller status
```

Stateful controller runtime files are keyed by package and ADB serial, so two
devices do not share controller sockets, state files, or ownership locks. The
mutable control files remain stable for current-controller lookup, while each
controller start also creates a preserved diagnostics invocation under
`<runtime>/<package>.<serial>.runs/`.

## Complete Observation Session

For human-operated complete observation, use the stateful session lifecycle.
`session start` starts IME logging, screen video, and a concurrent ADB
touchscreen event stream with `getevent`. `session stop` finalizes the run,
pulls IME logs, normalizes the event stream to JSONL, writes `manifest.json`,
and writes `validation.json`:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-human-android

cargo run --quiet -p input-dynamics -- doctor
cargo run --quiet -p input-dynamics -- install
cargo run --quiet -p input-dynamics -- select-ime
cargo run --quiet -p input-dynamics -- touch doctor
cargo run --quiet -p input-dynamics -- session start \
  --input-actor human \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID"
cargo run --quiet -p input-dynamics -- session status --run-id "$RUN_ID"

# Use the device, then finalize the run:
cargo run --quiet -p input-dynamics -- session stop --run-id "$RUN_ID"
cargo run --quiet -p input-dynamics -- recording inspect --dir "runs/$RUN_ID"
```

Video is enabled by default for complete sessions and stored under `video/`.
Treat `video/screen.mp4`, ADB event streams, screenshots, accessibility dumps,
and pulled JSONL as sensitive local run artifacts.

## Bounded Smoke Session

For smoke tests or externally bounded capture windows, use `session run
--duration-ms`. It starts the same umbrella lifecycle, waits for the requested
positive capture window after start succeeds, then finalizes through the same
stop path. It does not depend on stdin. Current complete-session examples use
`--input-actor human`; agent-owned complete sessions remain reserved until the
agent umbrella lifecycle exists.

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-smoke

cargo run --quiet -p input-dynamics -- session run \
  --input-actor human \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --duration-ms 10000
cargo run --quiet -p input-dynamics -- recording inspect --dir "runs/$RUN_ID"
```

The output directory has the same artifact layout as `session start/status/stop`.
Video is enabled by default. Add `--with-evidence` to capture start/end
observation bundles, and reserve `--no-video` for diagnostics or CI runs that do
not need video artifacts.

## Diagnostic Live Input

For live agent-driven input outside a full capture run, use the diagnostic
controller lifecycle. It selects the IME, starts IME logging, starts one
persistent AOSP uinput controller, and then accepts live `type`, `tap`, and
`press` commands:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android

cargo run --quiet -p input-dynamics -- controller start --run-id "$RUN_ID"
cargo run --quiet -p input-dynamics -- keyboard ensure-visible
cargo run --quiet -p input-dynamics -- type "ab a"
cargo run --quiet -p input-dynamics -- press delete
cargo run --quiet -p input-dynamics -- controller stop
```

This is a diagnostic live-input path, not a complete recording workflow.
`controller start` must report `input.ready_for_input: true`, and
`keyboard ensure-visible` must report an IME status with
`input_scope_ready: true` before agents send `type`, `tap`, or `press`.
These commands fail instead of injecting input when the controller is not ready,
the IME session is inactive, or the visible keyboard is not attached to a known
non-password field.

Only one diagnostic controller can be active for a package/device runtime at a
time. A competing `controller start` for the same package and ADB serial returns
JSON with `ok: false` and `busy: true` before changing IME or logging state.
Agents should treat that as a hard ownership conflict, inspect
`controller status`, and wait for the active controller to stop.

Diagnostic controller runs use the bundled `profiles/baseline-v1.json` input
profile by default for controller-driven input. To bind a local profile to the
whole controller run:

```bash
cargo run --quiet -p input-dynamics -- controller start \
  --run-id "$RUN_ID" \
  --input-profile ./profiles/custom.json \
  --input-profile-seed 12345
```

The IME JSONL `session_start` record and controller state include
`input_profile_source`, `input_profile_id`, `input_profile_schema`,
`input_profile_hash`, and `input_profile_seed`.

Controller diagnostics are automatic. `controller start`, `type`, `tap`,
`press`, and `controller stop` write a unified structured event journal at
`controller.events.jsonl` inside the current invocation directory. The journal
contains client and controller request lifecycle events, including request
writes, response reads/timeouts, uinput writes, state writes, response delivery,
and controller exit. Stdout/stderr and final controller state are stored in the
same invocation directory. No separate snapshot command is required before a new
controller run.

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

To preserve screen context at the beginning and end of a run, add
`--with-evidence`:

```bash
cargo run --quiet -p input-dynamics -- session start \
  --input-actor human \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --with-evidence
cargo run --quiet -p input-dynamics -- session status --run-id "$RUN_ID"
cargo run --quiet -p input-dynamics -- session stop --run-id "$RUN_ID"
```

This captures `observe all` bundles under `evidence/start/` and
`evidence/end/`. This is separate from the continuous video captured by
default. Use `--full-accessibility-evidence` only when a protocol needs
uncompressed accessibility hierarchy dumps.

Raw IME controls remain available only under the diagnostic `ime` namespace:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android

cargo run --quiet -p input-dynamics -- doctor
cargo run --quiet -p input-dynamics -- install
cargo run --quiet -p input-dynamics -- select-ime
cargo run --quiet -p input-dynamics -- touch doctor
cargo run --quiet -p input-dynamics -- ime start --run-id "$RUN_ID"
cargo run --quiet -p input-dynamics -- ime status
cargo run --quiet -p input-dynamics -- layout
cargo run --quiet -p input-dynamics -- ime stop
cargo run --quiet -p input-dynamics -- ime pull --out "runs/$RUN_ID"
cargo run --quiet -p input-dynamics -- validate "runs/$RUN_ID" --run-id "$RUN_ID"
```

These commands do not create a complete observation bundle. Mutating diagnostic
IME commands fail when an umbrella session runtime is active.

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
  "message": "keyboard is hidden (keyboard_view_not_shown); run `input-dynamics keyboard ensure-visible` or focus a non-password editable field first",
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
- `session start --input-actor human --run-id <id> --out <dir>`: starts a
  complete human-operated observation session with IME JSONL, screen video, ADB
  touchscreen event capture, runtime state, and finalization metadata. Add
  `--with-evidence` for start/end observation bundles. Use `--no-video` only
  for diagnostics or CI.
- `session run --input-actor human --run-id <id> --out <dir> --duration-ms
  <ms>`: runs a bounded capture window for smoke tests or externally bounded
  capture using the same session lifecycle and finalization path.
  `--duration-ms` must be positive.
- `session status [--run-id <id>]`: reads the active umbrella session runtime
  and process liveness without mutating state.
- `session stop --run-id <id>`: finalizes the active umbrella session, stops
  capture processes, pulls IME logs, writes validation, and clears runtime
  ownership. Omitting `--run-id` is non-mutating and returns the active run id.
- `ime enable-logging` / `ime disable-logging`: diagnostic raw IME logging
  toggles.
- `ime start --run-id <id>`: diagnostic raw IME-only logging session. Optional
  provenance flags are `--input-actor`, `--input-controller`, and
  `--input-cadence-policy`; defaults are `human`, null, and `manual`.
- `ime status`: returns current raw IME control status.
- `ime stop`: stops the active raw IME logging session.
- `ime list-logs`: lists app-specific external log files.
- `ime clear-logs`: clears raw IME logs when no IME session is active.
- `ime pull --out <dir>`: pulls app-specific external log storage.
- `controller start --run-id <id>`: diagnostic live-input lifecycle. It
  enables/selects the IME, enables logging, starts an IME session, starts a
  persistent local uinput controller, and binds an input profile. If another
  diagnostic controller is active or starting, it returns `busy: true` without
  changing the active run.
  Defaults are `--input-actor agent_adb`,
  `--input-controller input-dynamics-cli`, and
  `--input-cadence-policy input_profile`.
  - `--input-profile <path>`: local profile JSON for the whole controller run.
  - `--input-profile-seed <u64>`: explicit seed for reproducible sampled input.
- `controller status`: returns IME status plus local input-controller status.
  When the uinput controller is active, `input.state` includes the physical
  touchscreen profile hash, input profile summary, mirrored virtual touchscreen
  event path, Event Hub device metadata, Input Reader device metadata when
  Android exposes them, and compact controller command diagnostics:
  `current_command`, `last_command`, `last_error`, and `command_sequence`.
  `input.runtime.current_invocation` points to the active/latest diagnostics
  directory and event journal.
- `controller stop`: stops the local input controller, verifies normal runtime
  cleanup, verifies that the mirrored virtual touchscreen event path has
  disappeared when it was detected, then stops IME logging. If a previous
  controller process was interrupted, `controller stop` also removes stale
  runtime files and reports cleanup from the saved state. Final controller state
  and final controller lock are preserved in the diagnostics invocation
  directory.
- `layout`: returns status including `keyboard_layout` when the IME is visible.
  Layout output includes keyboard-view bounds, display size/rotation, and
  screen-space key centers for coordinate calibration.
- `layout --wait-visible` / `layout --wait-hidden`: waits for keyboard layout
  visibility state before returning.
- `keyboard ensure-visible`: establishes the single ready state required for
  logged live input. When the keyboard is already visible, it still requires
  `input_scope_ready: true`; a merely visible keyboard is not enough. Otherwise,
  with an active diagnostic controller, it captures the current accessibility
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
  through the active diagnostic controller. Fails unless the controller reports
  `ready_for_input: true` and the IME reports `input_scope_ready: true`.
- `press delete`, `press enter`, `press space`: taps common keys by semantic
  name through the active diagnostic controller. Uses the same readiness gate
  as `tap`.
- `type <text>`: types text through visible layout keys in the active
  diagnostic controller. The command plans the full string before pressing any
  key; unsupported characters, inactive controller state, hidden keyboard state,
  and missing input-scope readiness fail without partial typing. The active
  input profile can sample key-local landing ratios, hold duration, contact
  fields, and inter-key delay.
  `--inter-key-delay-ms`, default `40`, is used when the active controller does
  not provide an inter-key delay sample.
- `touch doctor`: checks AOSP uinput availability and reports the mirrored
  physical touchscreen profile used by the CLI.
- `touch tap --x <x> --y <y> [--hold-ms <ms>]`: sends an absolute screen tap
  through AOSP uinput. Use `--hold-ms` for one-shot long presses; there is no
  separate `touch hold` command.
- `touch swipe --from-x <x> --from-y <y> --to-x <x> --to-y <y>`: sends an
  absolute swipe through the active diagnostic controller.
- `touch path --points-json '<json>'` or `touch path --points-file <path>`:
  sends an absolute point path through the active diagnostic controller. Points
  may be `[{"x":1,"y":2}]` objects or `[[1,2]]` arrays.
- `validate <path> --run-id <id>`: validates JSONL lifecycle, safety fields,
  timestamp metadata, and clock-validation diagnostics.
- `getevent normalize --input <raw.log> --output <events.jsonl>`: parses
  `adb shell getevent -lt` output into neutral JSONL records with schema
  `input_dynamics_getevent.v1`.
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
- `derive video-map --recording-dir <dir>`: runs `ffprobe` on
  `video/screen.mp4`, reads the derived timeline, and writes a video map under
  `derived/video_map/`. The map includes encoded frame metadata, an explicit
  clock-transform contract, and one event-frame row per timeline event with
  status and uncertainty.
- `recording inspect --dir <dir>`: inspects a local recording directory without
  modifying it. Output includes artifact metadata, current validation state,
  derived timeline staleness, analysis-readiness flags, and suggested next CLI
  actions.

Use `type <text>` for ordinary text entry. Use semantic `press` commands for
common non-letter keys and corrections. `tap --code=-7` still works for delete,
and `tap --code -7` is also accepted, but semantic commands are easier for
agents to generate correctly. `type`, `tap`, `press`, `hide-keyboard`, `touch
swipe`, and `touch path` require an active diagnostic controller; `type`, `tap`,
and `press` also require visible keyboard layout state and IME
`input_scope_ready: true`. Run `keyboard ensure-visible` to establish that
state. Use `touch tap` only for low-level diagnostic absolute taps.

Controller-backed input and `touch` commands use AOSP `/system/bin/uinput` for
touchscreen input. They fail if the device does not expose that command instead
of falling back to a second touch implementation.

## Session Output

A finalized complete session creates:

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

Bounded `session run` manifests identify `session_command.name: "session run"`,
`bounded: true`, the requested `bounded_duration_ms`, `timer_policy:
"after_active"`, and `stop_trigger: "duration_elapsed"`.

Timing fields use explicit clock domains. Android input event timestamps are
in the Android uptime domain, `t_elapsed_realtime_ns` is a session/status
elapsed-realtime timestamp, raw `getevent` timestamps stay in the
`kernel_getevent_us` domain until aligned, and video frame timestamps will use
media PTS. Do not treat wall-clock fields as ordering truth; they are
diagnostic/provenance fields.

Screenrecord start/stop timing samples are captured through the same app
`STATUS` request-result path used by `input-dynamics ime status`. Each marker
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

When `session start` is run with `--with-evidence`, `manifest.json` includes
`evidence.enabled: true`, `evidence.policy: start_end`, and the start/end
observation bundle metadata. Each evidence phase is bracketed by the same
request-correlated `device_clock_probe` helper used for screenrecord anchors:
`before_evidence_start`, `after_evidence_start`, `before_evidence_end`, and
`after_evidence_end`. Accessibility dumps and screenshots may contain visible
screen content; keep them with the same care as raw run artifacts.

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
`needs_derivation`, `needs_timeline`, `has_video_frame_index`, and
`needs_video_frame_index`, `has_video_map`, and `needs_video_map`.

`validation.current.clock_validation` splits timestamp metadata diagnostics into
specific counters. Stable keys are
`timestamp_metadata_record_count`, `legacy_timestamp_metadata_missing_count`,
`missing_timestamp_role_count`, `missing_clock_domain_count`,
`invalid_clock_domain_count`, `invalid_timestamp_source_count`,
`invalid_timestamp_precision_count`, `timestamp_role_mismatch_count`,
`timestamp_field_reference_error_count`, `timestamp_unit_mismatch_count`,
`timestamp_order_violation_count`, and `mixed_clock_domain_claim_count`.
`invalid_timestamp_metadata_count` remains as a compatibility rollup.

The `clock` object classifies saved video and evidence anchors as
`bracketed`, `legacy_wall_clock_bracketed`, `missing_source`, `stale_inputs`,
`probe_failed`, `not_requested`, or `not_estimated`. Legacy wall-clock timing
remains readable, but it does not set `canonical_clock_ready`. Required missing
or stale video makes `valid_for_analysis` false and adds a
`session_with_video` action. Missing, legacy, stale, or malformed canonical
clock anchors add a session refresh action. `next_actions` contains local CLI
command templates an agent can use to refresh missing or stale artifacts.
Session refresh actions include placeholders such as `<new-run-id>` and
`<new-run-dir>`; fill them before running any command. Each item has `kind`,
`command`, and `reason`; branch on `kind`. Session refresh items also include
`workflow: session_start_status_stop_inspect` and a structured `commands` array
with `start`, `status`, `stop`, and `inspect` steps. The compatibility
`command` field mirrors the first `start` step only; agents should execute the
full `commands` sequence. Current action kinds are `validate`,
`session_with_video`,
`session_with_canonical_clocks`, `derive_presses`, `derive_summary`,
`derive_dismissals`, `derive_timeline`, and `derive_video_map`. It does not
probe the device or derive new clock alignment.

Treat `valid_for_analysis` as a base artifact/readability gate. For any
video/evidence anchor claim, cross-source timeline claim, or ordering claim that
depends on canonical clocks, also require `flags.canonical_clock_ready: true`,
`flags.needs_canonical_recording: false`, and no session refresh action in
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
- `timeline.artifact_diagnostics` and
  `clock.timeline.artifact_diagnostics` surface diagnostics from
  `derived/timeline/index.json`. Stable keys are
  `missing_clock_domain_count`, `invalid_clock_domain_count`,
  `mixed_clock_domain_claim_count`,
  `mixed_clock_domain_without_alignment_count`,
  `normalized_claim_without_domain_count`, and `unit_mismatch_count`.

After a finalized complete session:

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
output paths, warnings, clock-domain counts, normalized-status counts, artifact
clock diagnostics, and the clock-alignment status.

To create a video event-frame map:

```bash
input-dynamics derive video-map --recording-dir "runs/$RUN_ID"
```

This writes:

```text
derived/
  video_map/
    index.json
    frames.jsonl
    alignment.json
    event_frames.jsonl
```

`derive video-map` reads `manifest.json`, `video/screen.mp4`, and
`video/timing.json`, requires `derived/timeline/index.json` and
`derived/timeline/events.jsonl`, then invokes `ffprobe`. If the timeline bundle
is missing, run `derive timeline` first.

`frames.jsonl` contains one `input_dynamics_video_frame.v1` row per encoded
frame with integer `media_time.pts_ns` in the `media_pts_ns` clock domain,
source PTS tick/time-base provenance, keyframe flag, frame dimensions, encoded
packet size when available, and source-video fingerprint metadata. The frame
count comes from parsed frame rows, not nominal FPS or stream `nb_frames`.

`alignment.json` uses schema `input_dynamics_video_alignment.v1` and records
the transform from device timing anchors to media PTS as an interval with
reasons and uncertainty. Current canonical runs use
`device_elapsed_realtime_ns`; older readable runs may produce explicitly
`legacy_wall_clock_bracketed` mappings.

`event_frames.jsonl` uses schema `input_dynamics_event_video_frame_map.v1` and
emits one row per timeline event. Mapped rows contain a media PTS interval and
candidate frame window selected by interval overlap. Unsupported, legacy, or
outside-video rows are kept as rows with explicit `mapping_status`; agents must
branch on per-row status and uncertainty.

Treat `flags.has_video_frame_index` as readiness for frame metadata and
`flags.has_video_map` as readiness for event-frame windows. Neither flag means
the visual content has been interpreted. Canonical timing confidence is reported
separately through `flags.canonical_clock_ready` and
`flags.needs_canonical_recording`.

To use a local classifier-threshold policy:

```bash
input-dynamics derive dismissals \
  --recording-dir "runs/$RUN_ID" \
  --policy ./policies/custom-derivation.json
```

Input profiles and derivation policies are separate. `--input-profile` controls
controller-generated input. `--policy` controls recording interpretation.
Derived artifacts, including timeline bundles and everything under
`derived/video_map/`, are sensitive local recording data; keep them with the
same care as raw JSONL, screen video, screenshots, and accessibility dumps.

Use [adb-control.md](adb-control.md) when debugging the lower-level broadcast
surface directly.
