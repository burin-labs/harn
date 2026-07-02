<!--
Automated PR gates (Demo gate, Changelog fragment) auto-detect from your diff
whether they apply and pass on their own when they don't. Do NOT add
`no-demo-needed` / `no-changelog-needed` preemptively — only add a label if a
gate actually fails and the entry is genuinely unnecessary (hygiene, pure
refactor, dep bump with no user-visible effect). Adding a label re-runs the
gate without cancelling other in-flight checks. See
.github/workflows/pr-gates.yml.
-->

## Description

<!--
Plain language, bullet points, succinct. Describe what this PR does
END-TO-END: the behavior change an agent, user, or downstream repo actually
sees — NOT a list of files or tests (the Files tab already shows those). If
the flow or data path is non-obvious, add a small Mermaid diagram in a
```mermaid fence.
-->

## Test plan

<!--
Not a raw count of passing tests — a green tally says nothing about whether
the tests bind to real behavior. Make the intuitive case for why this change
is correct:

- What you verified EMPIRICALLY — tight verification loops, throwaway evals,
  headless dogfoods, manual runs — and what actually happened.
- Which load-bearing parts you are confident about, and why.
- The honest blind spots: what remains unvalidated, and how/when it gets
  validated (eval cell, live probe, follow-up PR).

Manual test plans, automated checks, or both — whatever genuinely
demonstrates correctness.

Mechanism-contract gate (onramp tier): if this PR adds or changes a
termination / escalation / judge / guard / routing mechanism, a mechanism
contract must be present and GREEN — a manufactured mini-eval under
`conformance/tests/mechanisms/*.contract.harn` proving the mechanism fires on
its trigger, emits its observable effect, and does NOT fire on the negative
case. A GREEN contract is a precondition to starting an N>=5 meter run — not
a replacement for it (the meter still owns convergence claims; see
docs/eval/meter-stick.md). See conformance/tests/mechanisms/README.md.
-->
