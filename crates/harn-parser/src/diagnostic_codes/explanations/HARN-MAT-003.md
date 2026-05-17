# HARN-MAT-003 — match pattern is invalid

**Category:** Pattern match (MAT)  
**Variant:** `Code::InvalidMatchPattern` (invalid match pattern)

## What it means

A `match` expression is incomplete, ambiguous, or otherwise invalid. Harn
enforces exhaustive matching to keep agent decision logic auditable.

Specifically: match pattern is invalid.

## How to fix

- Add arms covering the remaining variants, or use a wildcard `_` arm as the explicit fallback.
- Remove duplicate arms — each variant should appear at most once.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
