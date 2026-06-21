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
- Touch and key timing records from real soft-keyboard use.
- Password-field suppression.
- Local ADB controls for session coordination.
- Live keyboard layout snapshots for calibration and layout validation without
  screenshot-based coordinate discovery.

## Quick Start

Build and install the debug APK:

```bash
./gradlew :app:testDebugUnitTest :app:assembleDebug
adb install -r app/build/outputs/apk/debug/InputDynamicsKeyboard_3.9-debug.apk
adb shell ime enable org.inputdynamics.ime.debug/helium314.keyboard.latin.LatinIME
adb shell ime set org.inputdynamics.ime.debug/helium314.keyboard.latin.LatinIME
```

Start and stop a local run:

```bash
PKG=org.inputdynamics.ime.debug
RUN_ID=run-YYYYMMDD-HHMMSS-human-android

adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.ENABLE
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.START --es run_id "$RUN_ID"
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.STATUS
adb shell am broadcast -n "$PKG/.control.InputDynamicsControlReceiver" -a org.inputdynamics.ime.action.STOP
```

Pull debug logs:

```bash
adb pull /sdcard/Android/data/org.inputdynamics.ime.debug/files/input_dynamics_logs/ .
```

Full command reference: [docs/adb-control.md](docs/adb-control.md).

## Documentation

- [Documentation index](docs/README.md)
- [Input dynamics mode, privacy boundary, and schema](docs/input-dynamics-mode.md)
- [ADB control and validation](docs/adb-control.md)
- [Contribution guidelines](CONTRIBUTING.md)
- [Security and sensitive data reporting](SECURITY.md)

## Git Hygiene

Do not commit:

- pulled `input_dynamics_logs/` directories
- `*.jsonl` files
- `input_dynamics_control_status.json`
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
