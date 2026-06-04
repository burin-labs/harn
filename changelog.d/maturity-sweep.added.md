- **`index_of(haystack, needle, from?)` string builtin** — the missing sibling
  of `starts_with`/`ends_with`/`contains`, char-indexed to pair with
  `substring` (returns `-1` when absent).
- **`error_is(error, category)` and `error_is_transient(error)` testing
  builtins** — parameterized over the full error-category taxonomy, so a harness
  can assert any category (`cancelled`, `budget_exceeded`, `server_error`, …)
  or the retry oracle directly. `is_timeout`/`is_rate_limited` are now the two
  pre-wired spellings of `error_is`.
