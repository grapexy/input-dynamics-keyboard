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
devices do not share controller sockets, state files, or ownership locks.

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
cargo run --quiet -p input-dynamics -- layout --wait-visible
cargo run --quiet -p input-dynamics -- type "ab a"
cargo run --quiet -p input-dynamics -- press delete
cargo run --quiet -p input-dynamics -- session stop
```

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
captures a concurrent ADB touchscreen event stream with `getevent`, normalizes
that stream to JSONL, stops the session, pulls IME logs, writes
`manifest.json`, and writes `validation.json`:

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

To preserve screen context at the beginning and end of a run, add
`--with-evidence`:

```bash
cargo run --quiet -p input-dynamics -- record \
  --run-id "$RUN_ID" \
  --out "runs/$RUN_ID" \
  --with-evidence
```

This captures `observe all` bundles under `evidence/start/` and
`evidence/end/`. Use `--full-accessibility-evidence` only when a protocol needs
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
  "error": "keyboard layout is not available"
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
  event path, Event Hub device metadata, and Input Reader device metadata when
  Android exposes them.
- `session stop`: stops the local input controller, verifies normal runtime
  cleanup, verifies that the mirrored virtual touchscreen event path has
  disappeared when it was detected, then stops IME logging. If a previous
  controller process was interrupted, `session stop` also removes stale runtime
  files and reports cleanup from the saved state.
- `layout`: returns status including `keyboard_layout` when the IME is visible.
  Layout output includes keyboard-view bounds, display size/rotation, and
  screen-space key centers for coordinate calibration.
- `layout --wait-visible` / `layout --wait-hidden`: waits for keyboard layout
  visibility state before returning.
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
  through the active session controller.
- `press delete`, `press enter`, `press space`: taps common keys by semantic
  name through the active session controller.
- `type <text>`: types text through visible layout keys in the active session.
  The command plans the full string before pressing any key; unsupported
  characters fail without partial typing. The active input profile can sample
  key-local landing ratios, hold duration, contact fields, and inter-key delay.
  `--inter-key-delay-ms`, default `40`, is used when the active controller does
  not provide an inter-key delay sample.
- `touch doctor`: checks AOSP uinput availability and reports the mirrored
  physical touchscreen profile used by the CLI.
- `touch tap --x <x> --y <y> [--hold-ms <ms>]`: sends an absolute screen tap
  through AOSP uinput.
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
  ADB `getevent` raw and normalized touch streams, manifest, pull, and
  validation output. Use `--duration-ms <ms>` for timed runs; otherwise press
  Enter to stop. Add `--with-input-controller` for bounded agent-driven runs
  that need persistent uinput controller metadata in the manifest. Add
  `--with-evidence` to capture start/end observation bundles containing
  accessibility XML, screenshot PNG, status, layout, state, and index JSON.
- `derive dismissals --recording-dir <dir>`: derives touch gestures and dismissal
  inferences from a recording. The command reads coordinate facts from
  `manifest.json` and uses the bundled derivation policy by default. Pass
  `--policy <path>` for a local policy override.
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
swipe`, and `touch path` require `session start`; use `touch tap` only for
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
  derived/
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

When `record` is run with `--with-evidence`, `manifest.json` includes
`evidence.enabled: true`, `evidence.policy: start_end`, and the start/end
observation bundle metadata. Accessibility dumps and screenshots may contain
visible screen content; keep them with the same care as raw run artifacts.

When `record` is run with `--with-input-controller`, `manifest.json` includes:

- `input_controller_runtime.summary.input_backend`
- `input_controller_runtime.summary.input_device_command`
- `input_controller_runtime.summary.input_profile`
- `input_controller_runtime.summary.physical_touchscreen_profile_hash`
- `input_controller_runtime.summary.physical_touchscreen`
- `input_controller_runtime.summary.virtual_touchscreen`
- `input_controller_runtime.summary.virtual_touchscreen_event_path`
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
record counts, artifact fingerprints, stored-versus-current validation state,
timeline source staleness, and boolean flags such as `valid_for_analysis`,
`needs_validation`, `needs_derivation`, and `needs_timeline`. `next_actions`
contains local CLI commands an agent can run to refresh missing or stale
artifacts.

After recording with the current CLI:

```bash
input-dynamics derive dismissals --recording-dir "runs/$RUN_ID"
```

This writes:

- `derived/touch_gestures.jsonl`
- `derived/dismissal_inferences.jsonl`

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
gestures, dismissal inferences, and optional evidence bundle markers. It does
not copy low-level raw `getevent` rows by default and does not embed screenshot
or accessibility payloads. Each row keeps source references and an explicit
clock domain. `derived/timeline/index.json` records selected sources, counts,
fingerprints, output paths, warnings, and the clock-alignment status.

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
