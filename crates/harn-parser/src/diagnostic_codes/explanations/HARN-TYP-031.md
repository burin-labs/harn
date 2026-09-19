# HARN-TYP-031: Predicate outcome cannot be used as a boolean

A predicate outcome includes uncertainty, refusal, unavailable evaluation,
budget exhaustion, replay mismatch, and cancellation. Treating the record as a
truthy value would select a branch without an accepted verdict.

Match `result.kind`, then use `result.value.verdict` inside the `verdict` arm.
Give the remaining kinds an explicit disposition. A model verdict does not
prove a type refinement or grant permission.
