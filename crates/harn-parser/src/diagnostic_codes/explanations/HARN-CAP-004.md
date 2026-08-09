# HARN-CAP-004 — capability result must be checked

## How to fix

- Match the capability signature documented in the Harn capability spec.
- If approval / receipt handling is required, wire it through `human_approval` or the equivalent before calling the capability.
