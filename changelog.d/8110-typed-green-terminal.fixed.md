A run whose declared verifier passed over a source write no longer pays for a
completion judge that has nothing left to decide. When the deterministic
completion gate reports `verified_after_write`, the reading it summarized agrees,
and the turn is the sealed final answer with no deferred effect outstanding, the
completion judge is not called: the directive seals as accepted with outcome
`skipped_verified_after_write` and the run terminates naturally. Previously the
judge was invoked on every such boundary, which cost a provider call and its
latency on the agreeing case, could overwrite proven success with a refusal
naming no gap class, and on one measured headless run never returned before the
wall clock. Every other boundary is unchanged: an unverified path, a red or
unrun verifier, a green verifier with no source write behind it, and a run with
no gate at all all call the judge exactly as before.
