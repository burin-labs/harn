# HARN-CAP-006 — host capability call must use a static operation name

**Category:** Host capability (CAP)  
**Variant:** `Code::CapabilityCallStaticNameRequired` (capability call static
name required)

## What it means

A host capability call (file I/O, network, HITL approval, tool host, etc.) is
shaped in a way Harn cannot statically validate. Capabilities are the trust
boundary between Harn scripts and the embedding host, so the check is strict by
design.

Specifically: host capability call must use a static operation name.

## How to fix

- Match the capability signature documented in the Harn capability spec.
- If approval / receipt handling is required, wire it through `human_approval` or the equivalent before calling the capability.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
