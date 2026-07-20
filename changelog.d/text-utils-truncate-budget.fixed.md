- **Truncated text no longer overruns its own budget.** Several surfaces took `max`
  units of text and *then* appended an ellipsis, producing output one to three units
  longer than the stated limit — most visibly in `ast/symbols` signature extraction,
  which capped signatures at 80 characters and emitted 81. Truncation now lives in one
  place (`harn_vm::text::truncate`) where the ellipsis is charged against the budget, so
  a result is never longer than the caller asked for. A zero budget yields empty output
  instead of a bare ellipsis, and the byte-budgeted variant still cuts on a UTF-8
  boundary.
