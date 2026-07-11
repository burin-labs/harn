- `harn check` over a directory now checks files on a parallel worker pool
  (override with `HARN_CHECK_JOBS=<n>`; `1` restores the serial driver) and
  memoizes resolved-module parsing across the preflight/mock-host/import/bundle
  scans, so the shared import closure is parsed once per run instead of once
  per importing file. Whole-tree check on a 616-file pipeline tree drops from
  ~137s to ~4s with byte-identical diagnostics. A lex/parse failure in one
  file no longer stops text-mode `harn check` from checking the remaining
  files (the run still exits non-zero).
