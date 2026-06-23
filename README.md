# Input Dynamics Keyboard

Input Dynamics Keyboard is an Android IME research fork of HeliBoard for
measuring real human soft-keyboard interaction timing.

This is a research instrument, not an app-store distribution build. It is
intentionally application-neutral and focused on local keyboard instrumentation.

## Important Privacy Boundary

Use this keyboard only in consented, local research settings.

- The app is offline and must not request Internet permission.
- Logs are written locally; there is no automatic upload path.
- Password-class fields are the hard automatic suppression boundary.
- Non-password fields remain in scope and may include typed content, key labels,
  output text, input app package context, timing, and touch geometry.
- Raw JSONL exports are sensitive research data. Do not commit them or attach
  them to public issues.

## What This Fork Adds

- Explicit input dynamics logging enable/disable state.
- Explicit session start/stop state.
- JSONL event logs with `schema: input_dynamics_event.v1`.
- Optional caller-provided `external_run_id` on every record in a session.
- Session-level input provenance on `session_start`.
- Touch and key timing records from real soft-keyboard use.
- Password-field suppression.
- Request-correlated local ADB controls for session coordination.
- Live keyboard layout snapshots for calibration and layout validation without
  screenshot-based coordinate discovery.
- CLI observation bundles for current status, keyboard layout, accessibility
  hierarchy, and screenshot evidence when screen context is needed.
- Optional `record --with-evidence` start/end observation bundles for bounded
  runs that need screen context.
- Derived press summaries for per-key timing, landing geometry, pointer
  movement, and pressure/contact ranges from IME JSONL records.
- Derived run summaries for aggregate press counts, timing distributions,
  pointer/contact ranges, target package coverage, provenance, and source
  freshness.
- Derived recording timelines that index IME events, gesture derivations,
  dismissal inferences, and evidence bundle references without embedding raw
  evidence payloads.
- Read-only recording inspection that reports artifact presence, validation
  state, stale derived output, and next local CLI actions.
- AOSP uinput-backed CLI touch commands for local agent-driven key presses,
  layout-aware text entry, absolute taps, gesture paths, and edge-back keyboard
  dismissal.
- Optional `record --with-input-controller` manifests with uinput controller
  runtime metadata and cleanup results.
- Single-owner stateful CLI sessions; competing starts return a non-destructive
  busy result.
- Strict live-input readiness gates: agent key commands fail unless the uinput
  controller is ready and the IME has a known non-password input scope.
- Device-scoped CLI operation with `--serial` for multi-device hosts.

## Quick Start

Build the host CLI:

```bash
cargo build -p input-dynamics
```

Install the latest published debug APK, select the IME, and run a local
experiment capture:

```bash
RUN_ID=run-YYYYMMDD-HHMMSS-human-android

target/debug/input-dynamics doctor
target/debug/input-dynamics install
target/debug/input-dynamics select-ime
target/debug/input-dynamics touch doctor
target/debug/input-dynamics record --run-id "$RUN_ID" --out "runs/$RUN_ID"
target/debug/input-dynamics recording inspect --dir "runs/$RUN_ID"
```

If more than one Android device is connected, pass `--serial <adb-serial>` to
each `input-dynamics` command. Session runtime files and locks are keyed by
package and device serial.

The `record` command starts IME logging, captures an ADB touchscreen event
stream, writes normalized `adb/getevent.jsonl`, stops cleanly when you press
Enter, pulls logs, writes `manifest.json`, and writes `validation.json`.

To build and install a local debug APK instead:

```bash
./gradlew :app:testRunTestsUnitTest :app:assembleDebug
APK="$(ls -t app/build/outputs/apk/debug/*-debug.apk | head -n 1)"
target/debug/input-dynamics install --apk "$APK"
```

CLI reference: [docs/cli.md](docs/cli.md).

Raw ADB command reference: [docs/adb-control.md](docs/adb-control.md).

## APK Releases

Public APKs are distributed as GitHub Release assets, not committed binaries.
Release assets include the debug APK, SHA-256 checksums, and a permissions dump.
Published APKs are signed with the project APK key so agents can upgrade the
same package across releases. Release tags use the fork version, while APK
metadata and asset names include the HeliBoard base version for provenance. See
[docs/releases.md](docs/releases.md).

## Documentation

- [Documentation index](docs/README.md)
- [Host CLI for agents and local runs](docs/cli.md)
- [Input dynamics mode, privacy boundary, and schema](docs/input-dynamics-mode.md)
- [Derivation policies for recorded-run analysis](docs/derivation-policies.md)
- [ADB control and validation](docs/adb-control.md)
- [APK release process](docs/releases.md)
- [Contribution guidelines](CONTRIBUTING.md)
- [Security and sensitive data reporting](SECURITY.md)

## Agent Skill

This repo includes a publishable agent skill for local ADB-driven operation:
[skills/input-dynamics-keyboard](skills/input-dynamics-keyboard/SKILL.md).

After publishing the repo to GitHub, install it with:

```bash
npx skills add grapexy/input-dynamics-keyboard --skill input-dynamics-keyboard
```

## Git Hygiene

Do not commit:

- pulled `input_dynamics_logs/` directories
- `*.jsonl` files
- `input_dynamics_control_status.json`
- `input_dynamics_control_result_*.json`
- built APK/AAB artifacts
- signing keys or local SDK configuration

The `.gitignore` file includes these patterns for normal local workflows.

## Upstream And License

This repository preserves upstream HeliBoard history and remains based on
HeliBoard, OpenBoard, and AOSP Keyboard code.

HeliBoard, as a fork of OpenBoard, is licensed under GPL-3.0. See
[LICENSE](LICENSE). The included Apache-2.0 and CC-BY-SA-4.0 license files
cover inherited upstream materials that use those licenses.

Upstream projects credited by this fork include:

- [HeliBoard](https://github.com/Helium314/HeliBoard)
- [OpenBoard](https://github.com/openboard-team/openboard)
- [AOSP Keyboard](https://android.googlesource.com/platform/packages/inputmethods/LatinIME/)
- [LineageOS](https://review.lineageos.org/admin/repos/LineageOS/android_packages_inputmethods_LatinIME)
- [Simple Keyboard](https://github.com/rkkr/simple-keyboard)
- [Indic Keyboard](https://gitlab.com/indicproject/indic-keyboard)
- [FlorisBoard](https://github.com/florisboard/florisboard/)
