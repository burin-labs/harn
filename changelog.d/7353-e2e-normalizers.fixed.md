- **CLI report rendering accepts incomplete provider data (#7353).** Evaluation,
  model, provider, batch, and trace commands now render missing or wrong-shaped
  optional fields through one shared fallback contract instead of failing type
  checks. `harn time run --eager-project-handlers` can explicitly validate every
  project trigger and hook before measuring program execution.
