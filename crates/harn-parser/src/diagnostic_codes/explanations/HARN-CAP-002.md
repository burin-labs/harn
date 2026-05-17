# HARN-CAP-002 — human approval construct is missing policy

**Category:** Host capability (CAP)  
**Variant:** `Code::HitlMissingApprovalPolicy` (hitl missing approval policy)

## What it means

A host capability call (file I/O, network, HITL approval, tool host, etc.) is
shaped in a way Harn cannot statically validate. Capabilities are the trust
boundary between Harn scripts and the embedding host, so the check is strict by
design.

Specifically: human approval construct is missing policy.

## How to fix

- Match the capability signature documented in the Harn capability spec.
- If approval / receipt handling is required, wire it through `human_approval` or the equivalent before calling the capability.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
