# HARN-LNT-026 — prompt injection risk lint

**Category:** Lint (LNT)  
**Variant:** `Code::LintPromptInjectionRisk` (lint prompt injection risk)

## What it means

This is a lint, not a hard error. The code compiles, but Harn flags the pattern
as likely-incorrect, unidiomatic, or risky in a production agent. Lints can
typically be auto-fixed.

Specifically: prompt injection risk lint.

## How to fix

- Apply the lint's auto-fix where one is offered (`harn lint --fix`).
- Suppress the lint with an attribute only when the surrounding code is intentionally non-idiomatic.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
