# HARN-LNT-045 — require file header lint

**Category:** Lint (LNT)  
**Variant:** `Code::LintRequireFileHeader` (lint require file header)

## What it means

This is a lint, not a hard error. The code compiles, but Harn flags the pattern
as likely-incorrect, unidiomatic, or risky in a production agent. Lints can
typically be auto-fixed.

Specifically: require file header lint.

## How to fix

- Apply the lint's auto-fix where one is offered (`harn lint --fix`).
- Suppress the lint with an attribute only when the surrounding code is intentionally non-idiomatic.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
