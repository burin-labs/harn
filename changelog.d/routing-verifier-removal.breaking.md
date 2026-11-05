- **`routing_policy` no longer runs a verifier chain.** The `escalate_on` and `max_refines_per_link` options, the
  `verifier_outcome` and `verifier_signals` fields on routing attempts, and the `<dispatch>.verifier_signal` event are
  removed. A policy now advances only on provider failure, budget, or race. `routing_policy` does not reject unknown
  keys, so a leftover `escalate_on` is silently ignored and never runs.

  Migration: check the candidate in the calling script and call again with a stronger model when the check fails.
