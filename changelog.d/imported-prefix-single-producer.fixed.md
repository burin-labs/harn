- **`harn fix --capability-migrations-only` no longer aborts a whole pass on a
  call to an imported callee that needs several capabilities.** The
  imported-signature pass and the per-diagnostic pass both inserted at the same
  argument index, leaving two conflicting zero-width edits at one offset, and
  the overlap check refused the entire candidate — so an otherwise complete
  migration wrote nothing. The callee's declared signature now owns that
  insertion.
