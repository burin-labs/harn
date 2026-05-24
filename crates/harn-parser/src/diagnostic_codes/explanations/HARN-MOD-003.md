# HARN-MOD-003 — module imports are not in canonical order

## What it means

A module-level import declaration cannot be satisfied or has been authored in a
non-canonical form. The module graph must resolve cleanly before any further
checks.

## How to fix

- Confirm the import path resolves on disk and the imported symbol is exported.
- Run `harn lint --fix` to auto-sort / dedupe import groups.
