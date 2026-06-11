- **`ast.parse_errors` now flags tree-sitter grammar-limitation cascades.** When
  an `ERROR` node starts on line 1 and spans essentially the whole file — the
  fingerprint of well-formed source the grammar simply can't model, e.g.
  tree-sitter-scala 0.26 on Scala 3 indentation-based `match`/`case` — the
  response sets a top-level `cascade: true` and marks the offending error
  `spans_full_source: true`. Edit-validation gates use this to stop
  hard-rejecting correct creates/replaces on a grammar blind spot, instead of
  reporting a misleading `syntax error: line 1: ...`. Localized syntax errors are
  unaffected.
