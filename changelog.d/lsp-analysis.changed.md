- The language server analyzes a document without holding the lock every
  request reads. Folding, completion, and navigation use the current syntax
  while type analysis runs, and reuse the same parse when it finishes.
- The language server keeps each document's module graph and reuses it while
  the document's import declarations are unchanged, instead of re-reading
  every imported module from disk on each edit, and reads project lint
  settings once per directory until a watched file or the configuration
  changes.
- Imported modules resolve against the unsaved text of open documents, and
  editing a module re-checks the open documents that import it. Namespace
  signature changes invalidate cached type results.
- A dependency edit during analysis causes another pass, and results from a
  closed document cannot overwrite a reopened document at the same URI.
