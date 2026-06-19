- **Scalar integer arithmetic now promotes to float on `i64` overflow instead
  of silently wrapping.** `a + b`, `a - b`, `a * b`, `a ** b`, and unary `-a`
  previously wrapped two's-complement (e.g. `i64::MAX + 1` became a large
  negative number); they now promote to `float`, matching the language's own
  aggregate policy — `sum`/`abs` already promote on overflow. In-range integer
  arithmetic is unchanged. The compile-time constant folder defers the same
  overflow cases to the runtime so folded and unfolded expressions agree.
