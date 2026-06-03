- **The tree-sitter grammar now matches the canonical parser's operator
  precedence.** `??` was mis-ordered as looser than `||`/`&&`/`+`/`*` (it binds
  tighter than `+ -` and looser than `* / %`), and unary prefixes were tied with
  `**`. Structural tooling (Neovim highlighting, AST-based edits) now groups
  `a ?? b + c` as `(a ?? b) + c` and `-2 ** 2` as `-(2 ** 2)`, the same as the
  interpreter.
