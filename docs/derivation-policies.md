# Derivation Policies

Derivation policies configure how recorded run data is interpreted by analysis
commands. They do not control generated input.

The bundled default policy is:

```text
policies/default-derivation-v1.json
```

The CLI embeds this file at build time, so release users do not need a repo
checkout to use the default policy.

## Use

After creating a recording with the current CLI:

```bash
input-dynamics derive dismissals --recording-dir "runs/$RUN_ID"
```

To use a local policy override:

```bash
input-dynamics derive dismissals \
  --recording-dir "runs/$RUN_ID" \
  --policy ./policies/custom-derivation.json
```

`derive dismissals` reads screen and keyboard coordinate facts from
`manifest.json`. Do not pass screen width, screen height, or keyboard top by
hand. If a recording is missing `coordinate_frame`, record it again with the
current CLI.

## Schema

Current schema:

```text
input_dynamics_derivation_policy.v1
```

Fields:

- `edge_ratio_ppm`: edge-start threshold as parts per million of screen width.
- `edge_inward_ratio_ppm`: required inward movement as parts per million of
  screen width.
- `max_edge_vertical_drift_ratio_ppm`: allowed vertical drift as parts per
  million of screen height.
- `tap_max_distance_px`: maximum movement for tap classification.
- `tap_max_duration_ms`: maximum duration for tap classification.
- `hide_correlation_window_ms`: window for matching gestures to IME hide events
  only when the events are already in a comparable clock domain or a validated
  alignment transform supplies the conversion and uncertainty. It is not
  permission to subtract raw `kernel_getevent_us` from Android uptime.

Example:

```json
{
  "schema": "input_dynamics_derivation_policy.v1",
  "id": "example-derivation-v1",
  "edge_ratio_ppm": 30000,
  "edge_inward_ratio_ppm": 150000,
  "max_edge_vertical_drift_ratio_ppm": 120000,
  "tap_max_distance_px": 32,
  "tap_max_duration_ms": 250,
  "hide_correlation_window_ms": 1000
}
```

Input profiles and derivation policies are separate. Input profiles describe
controller-generated input behavior. Derivation policies describe classifier
thresholds for recorded data.
