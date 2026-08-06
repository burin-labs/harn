`harn check` now applies every diagnostic inside a destructuring pattern's
default expression. Defaults previously ran a binary-op-only check, so a call
with too few arguments, an unknown callee, or a bad capability method was
invisible there — `{flag = false || probe(info)}` type-checked clean and threw
at run time.
