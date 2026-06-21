# Input Dynamics CLI

The Rust `input-dynamics` CLI is the preferred host-side interface for agents
and scripted local runs. It wraps the ADB control surface, emits JSON, and keeps
raw broadcast command details out of normal workflows.

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

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-local-android

cargo run --quiet -p input-dynamics -- doctor
cargo run --quiet -p input-dynamics -- install
cargo run --quiet -p input-dynamics -- select-ime
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

Successful commands write JSON to stdout and return exit code 0. Handled
failures write JSON to stderr and return a non-zero exit code:

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
- `start --run-id <id>`: starts a session and records `external_run_id`.
- `stop`: stops the active session.
- `status`: returns current control status.
- `layout`: returns status including `keyboard_layout` when the IME is visible.
- `tap --label <label>` or `tap --code <code>`: taps a key from layout data.
- `list-logs`: lists log files.
- `clear-logs`: clears logs when no session is active.
- `pull --out <dir>`: pulls app-specific external log storage.
- `validate <path> --run-id <id>`: validates JSONL lifecycle and safety fields.

Use [adb-control.md](adb-control.md) when debugging the lower-level broadcast
surface directly.
