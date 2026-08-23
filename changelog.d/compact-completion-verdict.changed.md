The structured completion judge (`done_judge` and `verify_completion_judge`)
now returns one bounded object, `{verdict: "done" | "continue", detail}`, in
place of the previous five-field verdict. `detail` is capped at 240 characters
and carries the supporting evidence on `done` or the single most important gap
and next action on `continue`.

The old contract paired two unbounded string arrays with two unbounded prose
fields, so a judge model with a small output envelope could spend its entire
budget restating a decision it had already made and get cut off mid-array. The
structured call then failed and the round trip produced no verdict, and a retry
reproduced the same overrun because the schema, not the sampling, asked for the
long answer. The bounded contract makes that overrun unreachable by
construction.

The completion judge now also judges content only. Its prompts hand wire format
back to the deterministic completion layer: response markers and sentinels are
enforced there, their absence is never grounds for a veto, and a `continue`
must name a substantive next action rather than a reformat or a restatement.
The judge's user prompt previously still taught the superseded field names,
which both contradicted the schema it was sent and invited the long answer the
bounded contract exists to prevent.

Responses in the superseded shape still decode, so cached and replayed judge
responses keep their decision instead of reading as an absent verdict. The
`judge_decision` agent event drops the now-unreachable `specific_gaps` and
`accepted_evidence` fields; the same audit content reaches consumers through
`reasoning` and `next_step`. On that event, an approval from the model judge
now reports `verdict: "done"` rather than `verdict: "accept"`, matching the
value the fail-open path already emitted. The completion directive's own
`action` contract is unchanged and still reports `accept`.
