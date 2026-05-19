# HARN-CAP-201 — harness capability denied by active sandbox profile

**Category:** Host capability (CAP)  
**Variant:** `Code::SandboxCapabilityDenied` (sandbox capability denied)

## What it means

A `harness.fs.*`, `harness.env.*`, `harness.random.*`, or
`harness.net.*` method was rejected by the active sandbox profile.
Examples:

- `harness.fs.write_text("/etc/passwd", ...)` from a script whose
  `workspace_roots` only include `./build/` — the path is outside the
  permitted set.
- `harness.net.get("https://example.com")` from a script whose egress
  allowlist does not include `example.com`.
- Any `harness.*` capability under `SandboxProfile::OsHardened` when the
  required platform mechanism is unavailable.

The runtime raises the rejection as a typed `tool_rejected` error so
the harness method surface stays narrow — every script-visible
capability tightens the same way at the same boundary, instead of each
ambient builtin growing its own bespoke deny diagnostic.

When the requested operation can be evaluated against the profile
ahead of time (literal path or URL argument, well-known method on a
sub-handle), `harn check` and `harn lint` surface the same diagnostic
at static-check time so callers don't have to actually execute the
script to discover the denial.

## How to fix

- Widen the active sandbox profile via `CapabilityPolicy::workspace_roots`
  or the egress allowlist if the request is legitimate. The relevant
  policy lives in `~/.config/harn/policy.toml` or the per-script
  `CapabilityPolicy` overlay.
- Use `harn graph --json` to inspect which capabilities your script
  actually needs, then narrow the call site to those.
- For tests, switch from `Harness::real()` to `Harness::mock()` /
  `Harness::null()` so the call is recorded without touching the host.

## Stability

This code is stable. Its identifier, category, and meaning will not
change without a deprecation cycle. Cross-language tooling and IDE
integrations can dispatch on it directly.
