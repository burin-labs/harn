- **Hashed raw strings `r#"..."#`.** Raw string literals can now embed literal
  double quotes using Rust-style `#` delimiters (`r#"..."#`, `r##"..."##`, …),
  so quote-heavy regexes and patterns no longer need backslash escaping. The
  formatter picks the narrowest safe delimiter automatically.
- **`regex_captures` reports match positions.** Each match record now carries
  `start`/`end` (character offsets) and `line` (1-based), and the builtin
  accepts an optional `flags` argument (`i`, `m`, `s`, `x`) for parity with
  `regex_match`. This makes positional diagnostics (the equivalent of Python's
  `m.start()` / line-of-offset) expressible without re-scanning the input.
