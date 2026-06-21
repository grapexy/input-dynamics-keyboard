# AGENTS.md — heliboard-research-ime

This repository is a local research fork of HeliBoard for measuring real human
soft-keyboard interaction timing on Android.

The project is application-neutral. Do not reference any specific website,
account system, or downstream research target in code, package names, app
labels, exported data, or documentation.

## Objective

Build a consented Android IME research mode that records close-to-millisecond
typing and touch dynamics from real human use.

Primary measurements:

- key down/up timing
- hold time and flight time
- touch coordinates relative to key bounds
- pointer pressure, size, and movement when available
- correction actions such as backspace, space, enter, and suggestion taps
- session-level pause and cadence structure

## Naming

Use neutral research naming:

- Recommended Android `applicationId`: `org.typingresearch.ime`
- Recommended app label: `Typing Research Keyboard`
- Recommended branch prefix: `research/`
- Recommended schema prefix: `typing_event.v`

Do not use names tied to a particular service, product, or website.

## Privacy Requirements

- Log as much keyboard interaction data as possible for non-password fields,
  including key codes, key labels, output text, popup key metadata, timing, and
  touch geometry.
- Disable logging only in password-class fields, including visible password,
  web password, and number password variations.
- Do not exclude OTP, email, phone, URI, number, no-suggestions,
  no-personalized-learning, private IME option, or incognito contexts unless a
  later written protocol explicitly changes that.
- Keep the app offline. Do not add Internet permission.
- Export data manually through local Android storage/share flows.
- Make collected JSONL pullable over ADB by default. Write research logs to
  app-specific external storage with internal app-private storage only as a
  fallback when external storage is unavailable.
- Keep raw exports out of git.

## Implementation Guidance

Keep changes small and auditable.

Start with:

1. A research logging toggle.
2. Explicit session start/stop state.
3. A JSONL writer in app-specific external storage.
4. Touch-event instrumentation at the keyboard view / pointer tracker layer.
5. Commit-event instrumentation for non-text semantic actions.
6. Manual export from settings.
7. ADB readback commands for debug builds.
8. Tests or validation notes for password-field suppression.

Candidate source areas:

- `app/src/main/java/helium314/keyboard/keyboard/MainKeyboardView.java`
- `app/src/main/java/helium314/keyboard/keyboard/PointerTracker.java`
- `app/src/main/java/helium314/keyboard/latin/LatinIME.java`
- `app/src/main/java/helium314/keyboard/latin/InputAttributes.java`
- `app/src/main/java/helium314/keyboard/latin/utils/GestureDataGathering.kt`

## Data Format

Use newline-delimited JSON with a schema field on every record.

Minimum event fields:

- `schema`
- `session_id`
- `event`
- `t_uptime_ms`
- `t_wall_ms`
- `pointer_id`
- `key_code`
- `key_label`
- `key_output_text`
- `key_class`
- `key_touch_x_ratio`
- `key_touch_y_ratio`
- `key_center_offset_x_px`
- `key_center_offset_y_px`
- `pressure`
- `size`
- `password_field`

Raw key identity/content is expected for non-password fields. Password-class
fields are the automatic suppression boundary.

## Development Notes

- Preserve upstream HeliBoard history.
- Keep `upstream` as the source remote and do not push local research changes
  there.
- Prefer additive research code over broad refactors.
- Document every schema change in `docs/`.
- Build and test the debug variant before installing on a device.
- Expected release readback pattern:
  `adb pull /sdcard/Android/data/org.typingresearch.ime/files/research_typing_logs/ .`
- Expected debug readback pattern after the package rename:
  `adb pull /sdcard/Android/data/org.typingresearch.ime.debug/files/research_typing_logs/ .`
  Internal fallback files, if any, can still be inspected with
  `adb shell run-as org.typingresearch.ime.debug ls files/research_typing_logs`.
