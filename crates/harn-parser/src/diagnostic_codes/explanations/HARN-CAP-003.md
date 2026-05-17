# HARN-CAP-003 — human approval argument is invalid

**Category:** Host capability (CAP)  
**Variant:** `Code::HitlInvalidApprovalArgument` (hitl invalid approval
argument)

## What it means

A host capability call (file I/O, network, HITL approval, tool host, etc.) is
shaped in a way Harn cannot statically validate. Capabilities are the trust
boundary between Harn scripts and the embedding host, so the check is strict by
design.

Specifically: human approval argument is invalid.

## How to fix

- Match the capability signature documented in the Harn capability spec.
- If approval / receipt handling is required, wire it through `human_approval` or the equivalent before calling the capability.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
