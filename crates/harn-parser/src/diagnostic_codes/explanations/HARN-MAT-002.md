# HARN-MAT-002 — match expression contains a duplicate arm

**Category:** Pattern match (MAT)  
**Variant:** `Code::DuplicateMatchArm` (duplicate match arm)

## What it means

A `match` expression is incomplete, ambiguous, or otherwise invalid. Harn
enforces exhaustive matching to keep agent decision logic auditable.

Specifically: match expression contains a duplicate arm.

## How to fix

- Add arms covering the remaining variants, or use a wildcard `_` arm as the explicit fallback.
- Remove duplicate arms — each variant should appear at most once.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
