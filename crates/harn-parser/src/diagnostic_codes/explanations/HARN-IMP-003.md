# HARN-IMP-003 — import graph contains a cycle

**Category:** Import resolution (IMP)  
**Variant:** `Code::ImportCycle` (import cycle)

## What it means

Import resolution failed at a deeper layer than `MOD` — the file, symbol, or
module graph cannot be constructed. Compilation cannot proceed.

Specifically: import graph contains a cycle.

## How to fix

- Add the missing module or symbol, or update the import path.
- Break import cycles by extracting the shared definitions into a third module.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
