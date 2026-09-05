The verification-judge seam now reads the same typed green terminal the
completion-judge seam already reads. When the deterministic gate reports
`verified_after_write`, the verification reading agrees, the turn is the sealed
final answer with a non-empty response and no deferred effect outstanding,
neither judge is called: one predicate answers for both slots, so the rule
cannot be half-applied. Previously the verification judge always ran there, and
a run that had already been proven green paid its call, its latency, and the
risk that it would not return. The catalog-declared `completion_review` light
scrutiny skip is now subordinate to that reading rather than deciding beside it;
it still decides every boundary the verification does not, and its receipt still
names itself. Every other boundary is unchanged: an unverified path, a red or
unrun verifier, a green verifier with no source write behind it, and a run with
no gate all call both judges exactly as before.
