# HARN-NAM-008 — builtin name cannot be resolved

**Category:** Name resolution (NAM)  
**Variant:** `Code::UnknownBuiltin` (unknown builtin)

## What it means

Name resolution failed: the identifier, field, or attribute referenced here does
not match anything in the visible scope. Harn cannot proceed without a binding
for it.

Specifically: builtin name cannot be resolved.

## How to fix

- Define the missing name, import it, or fix the typo.
- Confirm the symbol is exported by the module you're importing from.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
