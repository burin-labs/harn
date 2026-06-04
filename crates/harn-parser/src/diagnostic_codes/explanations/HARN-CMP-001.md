# HARN-CMP-001 — the program failed to compile to bytecode

The program parsed and type-checked, but the bytecode compiler rejected it.
These are structural or codegen errors the type checker does not model yet —
for example an unsupported nested list/dict pattern in a `match` arm, a `break`
or `continue` outside a loop, `try*` outside a function, or a malformed string
interpolation hole.

`harn check` runs this compilation pass (discarding the bytecode) so that any
error which would stop `harn run` is reported up front, rather than only when
the program is executed.

## How to fix

- Read the message: it names the specific construct the compiler could not
  lower and usually how to rewrite it.
- For nested `match` patterns, bind the element with an identifier and match it
  in a nested `match`.
- For `break`/`continue`, ensure they appear inside a loop.
