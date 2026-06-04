- **List/dict accumulators are now O(1) amortized.** The common
  `xs = xs + [item]` / `xs += [item]` (and dict-merge) accumulator pattern no
  longer clones the whole collection on every step — a compiler optimization
  clears the binding's reference before the concat so the runtime extends the
  existing allocation in place. Building a 40 k-element list this way drops from
  ~18 s to ~0.5 s. Behavior is unchanged (including aliasing like `x = x + x`),
  and the scalar `i += 1` fast path is untouched.
