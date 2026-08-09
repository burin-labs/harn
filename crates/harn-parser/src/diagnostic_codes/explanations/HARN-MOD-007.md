# HARN-MOD-007 — imported module failed to compile

## What it means

An `import` in this file resolves to a module that could not itself be lexed or
parsed. Because the target never produced an AST, none of its symbols exist, so
callers would otherwise see every imported name reported as "undefined" at their
own call sites — pointing you at the wrong file. This diagnostic instead names
the broken module and surfaces its real lex/parse error.

## How to fix

- Open the imported module named in the message and fix the lex/parse error
  reported there (the message includes its failing line and column).
- Re-run `harn check` on the imported module directly to confirm it compiles,
  then re-check this consumer.
