# Security And Sensitive Data

Input Dynamics Keyboard is a local research instrument. It can record raw
non-password keyboard interaction data, including key labels, output text,
input app package context, timing, and touch geometry.

## Reporting Issues

Do not attach raw input dynamics logs, pulled `input_dynamics_logs/` directories,
`*.jsonl` files, or `input_dynamics_control_status.json` to public issues or pull
requests.

For public reports, include:

- app version and build variant
- Android version and device model
- exact local steps to reproduce
- whether the issue affects debug, release, or both
- redacted command output when needed

If a bug requires sample records to diagnose, create a minimal synthetic sample
that preserves field names and event ordering without real typed content or
real package names.

## Privacy Boundary

The automatic suppression boundary is password-class fields. Non-password
fields are in scope for research collection according to the project protocol,
so logs should be treated as sensitive local research data.

The app must remain offline and must not add Internet permission.
