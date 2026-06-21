# Contributing

This repository is a research fork of HeliBoard. Contributions should keep the
research surface small, auditable, and application-neutral.

## Scope

Appropriate changes for this fork include:

- input dynamics logging controls and validation
- timing, touch, and key-event instrumentation
- password-field suppression tests
- local storage/export improvements
- documentation for build, validation, and schema behavior
- small fixes needed to keep the fork buildable

General keyboard features that are unrelated to the research fork should usually
be proposed upstream instead.

## Research Constraints

All changes must preserve these boundaries:

- Do not add Internet permission or automatic upload behavior.
- Do not add broad storage permissions for input dynamics logs.
- Keep logs in app-specific external storage, with internal app-private storage
  only as a fallback.
- Keep password-class fields as the hard automatic suppression boundary.
- Keep non-password fields in scope unless a written protocol changes that.
- Keep code, package names, labels, exported data, and documentation
  application-neutral.
- Do not commit raw input dynamics logs, JSONL exports, status JSON files, APKs, or
  signing keys.

## Schema Changes

Every JSONL record must keep a `schema` field. The current event schema is
`input_dynamics_event.v1`.

When changing exported fields:

- update [docs/input-dynamics-mode.md](docs/input-dynamics-mode.md)
- update [docs/adb-control.md](docs/adb-control.md) when command or
  validation behavior changes
- preserve existing fields unless there is a clear migration reason
- add tests or validation notes for password-field suppression
- document whether existing analysis scripts need to handle the change

## Development

Build and test the debug variant before submitting changes:

```bash
./gradlew :app:testRunTestsUnitTest :app:assembleDebug
```

For release packaging checks:

```bash
./gradlew :app:assembleRelease
```

Before opening a pull request, also run:

```bash
git diff --check
```

For Rust host CLI changes, run:

```bash
cargo ci-fmt
cargo ci-test
cargo ci-clippy
```

Prefer property-based tests for pure parsing, validation, and matching logic.
Keep small example tests where they clarify lifecycle behavior.

If the change affects ADB controls, validate against a device or emulator and
include the exact commands used.

## Pull Requests

Keep pull requests focused. A good pull request description includes:

- what changed
- why it is needed for the research fork
- how it preserves the privacy boundary
- validation commands and results
- any docs updated

Do not attach raw logs. Use synthetic or redacted examples when an exported
record shape is important for review.

## Upstream History

This fork preserves HeliBoard history. Keep upstream attribution and licenses
intact, and avoid broad refactors that make future upstream comparison harder.
