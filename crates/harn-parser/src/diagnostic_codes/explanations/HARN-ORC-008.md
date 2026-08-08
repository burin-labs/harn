# HARN-ORC-008 — statement cannot be reached

## What it means

An orchestration construct — agent / workflow / pipeline / tool definition, or a
`select` block — does not satisfy the structural rules Harn enforces. These
constructs carry runtime semantics that depend on a small set of well-formed
shapes.

## How to fix

- Re-read the orchestration construct's spec section and align the arity / type / structure.
