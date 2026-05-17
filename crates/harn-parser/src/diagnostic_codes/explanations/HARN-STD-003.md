# HARN-STD-003 — builtin call has invalid arity

**Category:** Stdlib (STD)  
**Variant:** `Code::BuiltinArity` (builtin arity)

## What it means

A stdlib symbol is being used in a way Harn does not support, or has been
renamed / deprecated since the script was written. The stdlib is the live source
of truth for what's available.

Specifically: builtin call has invalid arity.

## How to fix

- Switch to the supported / renamed stdlib API listed in the diagnostic help text.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
