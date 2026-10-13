- `harn lint` and `harn check` type-check each file once. The linter now reads
  the type facts of the check that already ran with the file's imports,
  instead of type-checking the program a second time without them, so
  type-aware lint rules see bindings typed by imported declarations (for
  example, a redundant `== true` on an imported `bool` result is now
  reported). The editor's lint diagnostics read the same facts.
- `harn lint` runs files in parallel like `harn check` (`HARN_CHECK_JOBS`
  sets the worker count) and replays output in file order. A file that fails
  to lex or parse no longer stops the remaining files from being linted; the
  run still exits non-zero.
- `harn lint` parses each target once: the parse from building the module
  graph seeds the analysis. Analysis results are shared, not copied, between
  the cache and its readers, and the lint rules that read tokens share one
  tokenization of the file instead of each lexing it again.
- `HARN_CHECK_ANALYSIS_STATS=1` prints how many tokenizations, parses, and
  type checks a `harn lint` or `harn check` run performed.
