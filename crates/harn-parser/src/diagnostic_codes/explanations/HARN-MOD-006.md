# HARN-MOD-006 — module re-exports conflict

**Category:** Module / import (MOD)  
**Variant:** `Code::ModuleReExportConflict` (module re export conflict)

## What it means

A module-level import declaration cannot be satisfied or has been authored in a
non-canonical form. The module graph must resolve cleanly before any further
checks.

Specifically: module re-exports conflict.

## How to fix

- Confirm the import path resolves on disk and the imported symbol is exported.
- Run `harn lint --fix` to auto-sort / dedupe import groups.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
