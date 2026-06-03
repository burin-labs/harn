- **`-2 ** 2` now evaluates to `-4`, not `4`.** A unary minus on the base of an
  exponentiation now binds looser than `**`, so `-2 ** 2` parses as `-(2 ** 2)`,
  matching Python, Ruby, and ordinary math notation rather than the spreadsheet
  `(-2) ** 2` reading. The exponent operand still accepts a unary prefix
  (`2 ** -3` is `2 ** (-3)`), and `**` stays right-associative. Wrap the base in
  parentheses — `(-2) ** 2` — to keep the old result.
