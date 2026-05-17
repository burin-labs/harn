# HARN-MOD-005 — module imports expose colliding names

**Category:** Module / import (MOD)  
**Variant:** `Code::ModuleImportCollision` (module import collision)

## What it means

A module-level import declaration cannot be satisfied or has been authored in a
non-canonical form. The module graph must resolve cleanly before any further
checks.

Specifically: module imports expose colliding names.

## How to fix

- Confirm the import path resolves on disk and the imported symbol is exported.
- Run `harn lint --fix` to auto-sort / dedupe import groups.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
