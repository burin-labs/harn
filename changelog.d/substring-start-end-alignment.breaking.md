- **`substring(s, start, end)` now takes an exclusive end index, not a length.**
  The free `substring(...)` builtin previously treated its third argument as a
  **length**, while the `.substring(...)` method, the `s[start:end]` slice
  operator, `list.slice`, `bytes_slice`, and the language spec all use an
  exclusive **end** index. The builtin now matches that single convention, so
  `substring("hello world", 6, 9)` returns `"wor"` and the two call forms agree.
  Both forms share one implementation, so they can no longer drift. Migrate any
  length-style calls: a "last N chars" `substring(s, len(s) - n, n)` becomes
  `substring(s, len(s) - n)` (omit the end to run to the string end), and a
  fixed-length slice `substring(s, i, n)` becomes `substring(s, i, i + n)`.
