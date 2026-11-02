- **Replay validates complete fixture expectations (#10955).** Replay checks both recorded runs
  and the declared match or drift expectation in trace fixtures, rejects unknown fixture kinds,
  and reports how many expectations it compared.

  Migration: Rust consumers constructing `ReplayEvalReport` literals must provide the new
  `expectations_compared` field with the number of predicates actually evaluated. Consumers
  obtaining reports from the replay evaluator need no change.
