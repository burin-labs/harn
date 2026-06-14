- **The tree-sitter-harn grammar now parses an attribute followed by a doc
  comment before a declaration**, e.g. `@complexity(allow)` on one line, a
  `/** ... */` (or `//`) comment on the next, then `pub fn ...`. Previously the
  external scanner emitted a line separator after the attribute's newline, the
  comment was lexed as `extras`, and the *second* newline (after the comment)
  arrived in a parser state that no longer accepted a separator before the
  declaration — producing a hard parse `ERROR` and failing
  `scripts/verify_tree_sitter_parse.py --strict`. The canonical lexer treats
  comments as trivia and `parse_attributed_decl` `skip_newlines()` swallows the
  whole run, so the construct is valid Harn; this was a tree-sitter grammar gap,
  not a malformed source file. Fixed by changing `attributed_declaration` to
  accept `repeat($._line_sep)` (rather than `optional`) after each attribute so
  the trailing separator on either side of the comment is absorbed. This was the
  v0.8.109 release `grammar-audit` blocker (it tripped on a vendored
  `.harn/packages/harn-slack-connector/src/lib.harn` whose `normalize_inbound`
  carried exactly this `@attr` + doc-comment shape). Added a corpus regression
  test.
