- `harn-rules`: codemod `apply` no longer panics or corrupts a file when a rule's pattern produces
  nested/overlapping matches (e.g. `$X + $Y` over `a + b + c` matches both the outer and inner
  binary expression). The engine now keeps the outermost match and rewrites each region exactly once;
  `fix::splice` additionally skips any overlapping or out-of-range edit instead of panicking
  `replace_range` on a stale byte offset.
