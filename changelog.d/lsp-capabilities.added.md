- LSP: the language server now reacts to external `.harn` file changes (git checkout, another editor, codegen)
  by re-validating open documents via `workspace/didChangeWatchedFiles`, so cross-file diagnostics no longer
  go stale.
- LSP: added `textDocument/rangeFormatting` ("Format Selection"), which reuses the whole-document formatter
  and confines its edits to the selected lines.
- LSP: completion items now attach builtin and keyword documentation lazily through `completionItem/resolve`
  instead of computing every item's docs up front.
