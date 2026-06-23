<!--
Automated PR gates (Demo gate, Changelog fragment) auto-detect from your diff
whether they apply and pass on their own when they don't. Do NOT add
`no-demo-needed` / `no-changelog-needed` preemptively — only add a label if a
gate actually fails and the entry is genuinely unnecessary (hygiene, pure
refactor, dep bump with no user-visible effect). Adding a label re-runs the
gate without cancelling other in-flight checks. See
.github/workflows/pr-gates.yml.
-->

## Summary

## Notes for reviewers

<!--
Mechanism-contract gate (onramp tier). If this PR adds or changes a
termination / escalation / judge / guard / routing mechanism, a mechanism
contract must be present and GREEN — a manufactured mini-eval under
`conformance/tests/mechanisms/*.contract.harn` proving the mechanism fires on
its trigger, emits its observable effect, and does NOT fire on the negative
case. This mirrors the tracked-flag reachability-probe requirement: a new lever
must prove it engages before it ships. A GREEN contract is a precondition to
starting an N>=5 meter run — not a replacement for it (the meter still owns
convergence claims; see docs/eval/meter-stick.md). Delete this comment if no
such mechanism is touched. See conformance/tests/mechanisms/README.md.
-->

