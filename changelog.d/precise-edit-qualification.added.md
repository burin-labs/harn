- `scripts/precise_qualify.harn` measures the precise edit tools on pinned
  repositories. It samples symbols in a seeded order, previews and applies
  each tool in a scratch clone, and runs the repository's compiler or
  typecheck. Each attempt is classified as applied correctly, applied
  wrongly, refused with its typed reason, or crashed. The versioned
  receipt and summary table list wrong edits first and report pending
  attempts, so an empty sample never reads as a pass.
