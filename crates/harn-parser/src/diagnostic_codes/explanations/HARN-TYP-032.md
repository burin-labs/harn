# HARN-TYP-032: Predicate outcome must be consumed

A predicate evaluation produces an outcome even when no verdict is available.
An unused binding, discard binding, or discarded expression would hide that
decision and its receipt.

Match the outcome, return it to a caller, or pass it to an outcome policy. A
same-named binding in another scope does not consume the original result.
This check establishes use, not the correctness of the caller's policy.
