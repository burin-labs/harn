# HARN-IMP-002 — imported symbol does not exist

## What it means

A selective import named a symbol the target module does not make available.
Two cases trigger this:

- The symbol genuinely does not exist in the target module, or import
  resolution failed at a deeper layer than `MOD` (the file or module graph
  cannot be constructed).
- The symbol **exists but is not exported**: it is a non-`pub` function in a
  module that marks something else `pub`. A module that marks at least one
  function `pub` opts into explicit exports, so its non-`pub` functions are
  private and cannot be imported by name — the same rule wildcard imports
  already follow, and the same rule TypeScript, Rust, and Go enforce.

## How to fix

- Add the missing module or symbol, or update the import path.
- If the symbol is meant to be part of the module's API, mark it `pub`.
- To exercise a private helper from a test, co-locate the test (a `pipeline`
  or `fn` in the same file sees module-private functions directly), rather
  than importing the private name.
- Break import cycles by extracting the shared definitions into a third module.
