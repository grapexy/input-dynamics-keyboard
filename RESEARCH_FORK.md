# Human Typing Research IME

This repository is a local research fork of HeliBoard for measuring real human
soft-keyboard interaction timing on Android.

The fork is intentionally application-neutral. It should capture keyboard
interaction dynamics independently of any specific website, app, or service.

## Purpose

Collect high-resolution, consented typing interaction traces from real Android
keyboard use so timing and touch behavior can be studied empirically.

Primary measurements:

- key press and release timing
- hold time and flight time
- touch position within key bounds
- pointer pressure and size when available
- correction behavior such as backspace, space, enter, and suggestion taps
- session-level typing cadence and pause structure

## Privacy Constraints

- Log as much keyboard interaction data as possible for non-password fields,
  including key codes, key labels, output text, popup key metadata, timing, and
  touch geometry.
- Disable logging only in password-class fields, including visible password,
  web password, and number password variations.
- Keep OTP, email, phone, URI, number, and other ordinary non-password field
  types in scope for research collection.
- Keep the app offline; do not add Internet permission.
- Export data locally and manually.
- Store research logs in app-specific external storage by default so they are
  directly pullable with ADB.
- Store raw exports outside git.

## Implementation Direction

The fork uses a minimal instrumentation layer rather than broad refactors:

1. Research logging toggle and explicit session start/stop state.
2. Raw touch samples from `MainKeyboardView`.
3. Interpreted key timing from `PointerTracker`.
4. Append-only JSONL records in app-specific external storage.
5. Settings screen showing the exact log path and ADB pull command.
6. Password-field checks from `InputAttributes` before logging.

Candidate source areas:

- `app/src/main/java/helium314/keyboard/keyboard/MainKeyboardView.java`
- `app/src/main/java/helium314/keyboard/keyboard/PointerTracker.java`
- `app/src/main/java/helium314/keyboard/latin/LatinIME.java`
- `app/src/main/java/helium314/keyboard/latin/InputAttributes.java`
- existing gesture data utilities under
  `app/src/main/java/helium314/keyboard/latin/utils/`

## Data Format

Use newline-delimited JSON. Each line should be one event. Keep the schema
versioned from the first implementation.

Example:

```json
{"schema":"typing_event.v1","session_id":"local-random-id","event":"key_down","t_uptime_ms":123456789,"t_event_uptime_ms":123456700,"key_code":97,"key_label":"a","key_output_text":null,"key_class":"letter","key_touch_x_ratio":0.52,"key_touch_y_ratio":0.44}
```

Non-password key identity/content is in scope. Password-class fields remain the
automatic suppression boundary.

## ADB Readback

For release builds:

```bash
adb pull /sdcard/Android/data/org.typingresearch.ime/files/research_typing_logs/ .
```

For debug builds:

```bash
adb pull /sdcard/Android/data/org.typingresearch.ime.debug/files/research_typing_logs/ .
```

Use app-specific external storage as the primary research log directory:

```text
/sdcard/Android/data/org.typingresearch.ime/files/research_typing_logs/
/sdcard/Android/data/org.typingresearch.ime.debug/files/research_typing_logs/
```

That path keeps the workflow local, does not require Internet, and does not
require broad storage permissions. If external storage is unavailable, fall
back to internal app-private storage and expose that fallback through
`adb run-as`.
