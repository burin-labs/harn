- **Compaction can report a deferred classification.** `ClassificationStatus` adds `Deferred` when classification
  cannot finish after making partial progress. The receipt retains completed decisions and reports whether the
  remaining context meets the requested budget (#10179).

  Migration: Rust consumers matching `ClassificationStatus` exhaustively must handle `Deferred`. Preserve the
  remaining context and inspect `budget_met`; a deferred result does not authorize truncation to meet the budget.
