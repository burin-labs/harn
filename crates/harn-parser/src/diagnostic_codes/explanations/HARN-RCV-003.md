# HARN-RCV-003 — rescue construct is invalid

**Category:** Recovery (try / rescue) (RCV)  
**Variant:** `Code::InvalidRescueConstruct` (invalid rescue construct)

## What it means

A recovery construct (`try`, `rescue`) is in an invalid position or is shaped in
a way Harn's structured-error handling does not support.

Specifically: rescue construct is invalid.

## How to fix

- Move the construct inside a function body, or wrap it in one.
- Use `try` / `rescue` only around expressions that can produce structured errors.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
