# HARN-ORC-007 — select construct is invalid

**Category:** Orchestration (ORC)  
**Variant:** `Code::InvalidSelectConstruct` (invalid select construct)

## What it means

An orchestration construct — agent / workflow / pipeline / tool definition, or a
`select` block — does not satisfy the structural rules Harn enforces. These
constructs carry runtime semantics that depend on a small set of well-formed
shapes.

Specifically: select construct is invalid.

## How to fix

- Re-read the orchestration construct's spec section and align the arity / type / structure.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
