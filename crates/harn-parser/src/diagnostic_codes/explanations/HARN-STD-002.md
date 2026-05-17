# HARN-STD-002 — stdlib call is invalid

**Category:** Stdlib (STD)  
**Variant:** `Code::StdlibUsageInvalid` (stdlib usage invalid)

## What it means

A stdlib symbol is being used in a way Harn does not support, or has been
renamed / deprecated since the script was written. The stdlib is the live source
of truth for what's available.

Specifically: stdlib call is invalid.

## How to fix

- Switch to the supported / renamed stdlib API listed in the diagnostic help text.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
