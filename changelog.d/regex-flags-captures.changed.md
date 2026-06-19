- **Regex builtin docs and conformance now pin flag and capture semantics.**
  `regex_match`, `regex_replace`, and `regex_captures` document and test
  inline `(?is)` flags, trailing `i`/`s` flags, newline-spanning lazy captures,
  and the exact `regex_captures` result shape.
