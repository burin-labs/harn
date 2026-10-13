- File-language detection now has one owner, the new `harn-languages` registry
  crate, and the code index, project scanner, `get_file_outline`, `ast.*`,
  project fingerprint, grounding, and edit compass all read it. Reported
  language names follow the registry: `.tsx` and `.jsx` files report `tsx` and
  `jsx` instead of `typescript` and `javascript`, shell scripts report `bash`
  instead of `shell`, `Dockerfile` and `Makefile` report `dockerfile` and
  `makefile`, and scanner `language` fields carry registry names (`rust`)
  instead of file extensions (`rs`). Files the old tables disagreed on
  (`.hxx`, `.hh`, `.csx`, `.htm`, `.kts`, `.pyi`, `Dockerfile` in the scanner)
  are now indexed and scanned consistently.
- `get_file_outline` now uses the tree-sitter outline behind `ast.outline`, so
  items nest members under their types with real end rows, carry `signature`
  and `access_level`, and honor `max_depth`. Files without a structural
  outline return `status: "unsupported"` with a `reason` of
  `unknown_language`, `no_grammar`, or `no_symbol_projection` instead of a
  guessed outline, and unknown extensions no longer leak memory.
- The project scanner extracts symbols with tree-sitter for every language with
  a grammar; the line-oriented regex extractor remains only as the fallback for
  languages without one, such as Dart.
- The edit compass now treats `.harn` files as structural and rename-capable,
  matching the AST capability matrix.
