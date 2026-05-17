# HARN-NAM-007 — option key is not recognized

**Category:** Name resolution (NAM)  
**Variant:** `Code::UnknownOption` (unknown option)

## What it means

Name resolution failed: the identifier, field, or attribute referenced here does
not match anything in the visible scope. Harn cannot proceed without a binding
for it.

Specifically: option key is not recognized.

## How to fix

- Define the missing name, import it, or fix the typo.
- Confirm the symbol is exported by the module you're importing from.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
