- **`to_int` / `to_float` now trim surrounding whitespace before parsing.**
  `to_int("  42  ")` and `to_float(" 1.5\n")` returned `nil`; they now parse to
  `42` / `1.5`, matching the sibling `decimal(...)` builtin and Python/JS
  numeric coercion. Non-numeric strings still return `nil`.
