# Input Profiles

Input profiles configure the stateful CLI input generator. They are local JSON
files that control how controller-driven key taps are sampled through the
canonical uinput path.

The bundled profile is:

```text
profiles/baseline-v1.json
```

Diagnostic `controller start` uses the bundled baseline profile by default for
controller-driven input. To use a local profile for the whole controller run:

```bash
input-dynamics controller start \
  --run-id "$RUN_ID" \
  --input-profile ./profiles/custom.json
```

For reproducible sampled runs, pass a seed:

```bash
input-dynamics controller start \
  --run-id "$RUN_ID" \
  --input-profile ./profiles/custom.json \
  --input-profile-seed 12345
```

Live commands such as `type`, `tap`, and semantic `press` inherit the active
controller profile. Lower-level absolute touch commands remain explicit
coordinate commands.

The complete human observation lifecycle is `session start`, `session status`,
and `session stop`. Human sessions use manual cadence and do not accept input
profiles. Agent complete sessions are reserved until that lifecycle is
implemented; until then, profiles apply only to diagnostic controller-driven
input.

## Provenance

The CLI validates the profile, computes a stable hash, chooses or accepts a
session seed, and records neutral provenance:

- `input_profile_source`: `bundled` or `local_file`
- `input_profile_id`
- `input_profile_schema`
- `input_profile_hash`
- `input_profile_seed`

The IME app records these fields on `session_start` and in control status. The
controller state also records the same profile summary. The profile file
contents are not copied into JSONL logs.

## Schema

Current schema:

```text
input_dynamics_profile.v1
```

Profiles are generator configurations with named parameters and typed
distributions. They are not fixed scripts.

Supported distribution primitives:

- `fixed`
- `uniform`
- `integer_uniform`
- `normal_with_bounds`
- `weighted_choice`

Initial public parameter names:

- `timing.inter_key_delay_ms`
- `timing.hold_ms`
- `tap.x_ratio_ppm`
- `tap.y_ratio_ppm`
- `movement.samples_per_press`
- `movement.max_drift_px`
- `contact.pressure`
- `contact.touch_major_px`
- `contact.touch_minor_px`
- `contact.orientation`
- `gesture.edge_back.duration_ms`

Unknown parameter names fail validation.

Example:

```json
{
  "schema": "input_dynamics_profile.v1",
  "id": "example-v1",
  "parameters": {
    "timing.inter_key_delay_ms": {
      "dist": "integer_uniform",
      "min": 35,
      "max": 85
    },
    "timing.hold_ms": {
      "dist": "integer_uniform",
      "min": 45,
      "max": 95
    },
    "tap.x_ratio_ppm": {
      "dist": "normal_with_bounds",
      "mean": 500000,
      "stddev": 40000,
      "min": 380000,
      "max": 620000
    }
  }
}
```

Profile values describe generator intent. IME JSONL records and device-level
`getevent` captures remain the source of truth for what Android actually
received and delivered.

Input profiles do not configure recorded-run analysis thresholds. Use
[derivation policies](derivation-policies.md) for classifier policy.
