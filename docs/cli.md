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

## Common Workflow

For a complete bounded experiment capture, use `record`. It starts IME logging,
captures a concurrent ADB touchscreen event stream with `getevent`, stops the
session, pulls IME logs, writes `manifest.json`, and writes `validation.json`:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-human-android

cargo run --quiet -p input-dynamics -- doctor
cargo run --quiet -p input-dynamics -- install
cargo run --quiet -p input-dynamics -- select-ime
cargo run --quiet -p input-dynamics -- touch doctor
cargo run --quiet -p input-dynamics -- record --run-id "$RUN_ID" --out ".agents/experiments/$RUN_ID"
```

When `--duration-ms` is omitted, press Enter in the terminal to stop capture
cleanly. For scripted smoke tests, pass `--duration-ms <ms>`.

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

- `doctor`: checks ADB visibility, IME registration, and GitHub CLI presence.
- `install`: downloads or installs an APK.
- `select-ime`: enables and selects the IME.
- `enable-logging` / `disable-logging`: toggles logging.
- `start --run-id <id>`: enables logging, starts a session, and records
  `external_run_id`. Optional provenance flags are `--input-actor`,
  `--input-controller`, and `--input-cadence-policy`; defaults are `human`,
  null, and `manual`.
- `stop`: stops the active session.
- `status`: returns current control status.
- `layout`: returns status including `keyboard_layout` when the IME is visible.
- `layout --wait-visible` / `layout --wait-hidden`: waits for keyboard layout
  visibility state before returning.
- `hide-keyboard`: dismisses the visible IME and waits for hidden layout state.
- `tap --label <label>` or `tap --code <code>`: taps a key from layout data.
- `press delete`, `press enter`, `press space`: taps common keys by semantic
  name.
- `touch doctor`: checks AOSP uinput availability and reports the mirrored
  physical touchscreen profile used by the CLI.
- `touch tap --x <x> --y <y> [--hold-ms <ms>]`: sends an absolute screen tap
  through AOSP uinput.
- `list-logs`: lists log files.
- `clear-logs`: clears logs when no session is active.
- `pull --out <dir>`: pulls app-specific external log storage.
- `validate <path> --run-id <id>`: validates JSONL lifecycle and safety fields.
- `record --run-id <id> --out <dir>`: records a complete run with IME JSONL,
  ADB `getevent` raw touch stream, manifest, pull, and validation output. Use
  `--duration-ms <ms>` for timed runs; otherwise press Enter to stop.

Use semantic `press` commands for common non-letter keys. `tap --code=-7` still
works for delete, and `tap --code -7` is also accepted, but semantic commands
are easier for agents to generate correctly.

`tap`, `press`, and `touch tap` use AOSP `/system/bin/uinput` for touchscreen
input. They fail if the device does not expose that command instead of falling
back to a second touch implementation.

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
    getevent.stderr.log
  derived/
```

The `adb/getevent.raw.log` stream is device-level touchscreen data. Keep it
separate from IME-owned JSONL records when analyzing password-field suppression
or keyboard-local privacy guarantees.

Use [adb-control.md](adb-control.md) when debugging the lower-level broadcast
surface directly.
