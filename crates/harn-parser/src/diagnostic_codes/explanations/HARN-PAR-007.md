# HARN-PAR-007 — the `start to end` range spelling was removed

Ranges are written with Rust-style operators. `start..end` excludes `end` and
`start..=end` includes it. The old `start to end` spelling (inclusive) and its
`exclusive` suffix no longer parse, and `to` and `exclusive` are ordinary
identifiers again.

## How to fix

- Replace `a to b` with `a..=b`.
- Replace `a to b exclusive` with `a..b`.
- To rewrite a whole tree, run
  `harn fix --apply --migrate-operator-syntax <path>`. The migration also
  inserts parentheses wherever the new `??` precedence would regroup an
  expression, so every rewritten file keeps its meaning.
