- AST edit builtins (`ast.apply_node`, `ast.batch_apply`,
  `ast.insert_at_anchor`) no longer corrupt invalid-UTF-8 bytes. The read path
  decoded the whole file with `String::from_utf8_lossy` and the callers wrote
  the decoded buffer back, so any non-UTF-8 byte anywhere in the file (e.g. a
  Latin-1 byte or a `\x80` in a comment or byte-string) was silently rewritten
  to the 3-byte U+FFFD encoding — even in regions the edit never touched. The
  edit pipeline now reads, parses (tree-sitter over raw bytes), splices, and
  writes raw bytes, so bytes outside the edited span pass through verbatim.
  Lossy decoding is retained only for display-only previews/diagnostics and the
  read-only `ast.search` text bindings.
