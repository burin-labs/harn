- **Multi-line `/* ... */` block comments now report their span at the opening `/*` instead of the closing
  `*/`.** The lexer had stamped the span's start line with the comment's *end* line, so every consumer that
  keys off it — the `harn fmt` comment map, LSP positions, and the `legacy-doc-comment` lint rule —
  misattributed multi-line block comments. The span now records the open line/column with `end_line` at the
  close, mirroring how multi-line strings are recorded.
- **`harn fmt` no longer drops a trailing same-line comment on a top-level statement or import.** A comment
  like `let x = 1 // note` at the top level was silently discarded; block bodies already preserved these, but
  `format_program` never attached them. Top-level items now keep their trailing comment inline.
