# HARN-MAT-003 — match pattern is invalid

## What it means

A `match` expression is incomplete, ambiguous, or otherwise invalid. Harn
enforces exhaustive matching to keep agent decision logic auditable.

## How to fix

- Add arms covering the remaining variants, or use a wildcard `_` arm as the explicit fallback.
- Remove duplicate arms — each variant should appear at most once.
