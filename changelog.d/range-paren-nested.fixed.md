- **`harn fmt` keeps parentheses around a nested range.** A range (`a to b`)
  binds looser than ternary, comparison, and another range, so it can only
  appear bare at the top of an expression. The formatter dropped the parens when
  a range was nested as a ternary condition/branch, a binary operand, or another
  range's bound, producing output that either failed to re-parse
  (`c ? (a to b) : d` → `c ? a to b : d`) or silently changed meaning
  (`(a to b) < c` → `a to b < c`, i.e. `a to (b < c)`). Such ranges are now
  parenthesized at those sites.
