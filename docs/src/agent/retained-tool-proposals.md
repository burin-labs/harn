# Retained tool proposals

An agent response can propose observations and mutations together. The
dispatcher finishes observations first, then executes the authored mutations
through ordinary dispatch if observations succeeded and controls and context
remain current. No extra confirmation turn is required. If a read is needed to
decide what to change, request that read in an earlier response before authoring
the mutation. Successful reads do not prove that an edit is semantically correct.

Calls held by a failed observation or a later verification or completion phase
remain explicit proposals. The dispatcher stores their arguments and returns a
proposal identifier in the deferred result. A failed observation reports
`reason: "observation_failed"`; a pending control reports `control_pending`, and
a changed run, accepted control revision or context reports `scope_changed`.

The agent receives a stable
`__harn_resolve_tool_call({proposal_id, action})` definition on every turn. Pending
identifiers are validated at dispatch and never appear in its schema, keeping
tool definitions unchanged as proposals are retained and consumed. The model chooses `resume` to
use the stored arguments or `discard` to abandon them. To change arguments it
discards the proposal and issues a new ordinary tool call. A proposal can be
consumed once.

Resume resolves the proposal to its original semantic tool name and arguments
at the existing dispatch boundary. The confirmation call supplies the new
native tool-call identifier. Current schema validation, policy, approval,
middleware, resource guards and failed-mutation barriers still apply.

A current response may resume an edit and request additional observations
together. Both fresh and resumed edits use the same continuation checks:
successful observations, both pending-control queues clear, and the original
run, accepted control revision and context unchanged. A failed read or changed
control/context prevents continuation. Verification or completion calls keep
their phase barriers. Authoring or confirming a call grants no authority to
execute it automatically in a later response.

Pending proposals belong to one run, public user-message boundary and accepted
control revision. Compaction invalidates them because the source proposal may
no longer be visible to the model. A terminal run retires any remaining
proposals after its completion decision. The model must author a new call after
invalidation.

The original proposed call receives one synthetic result. A later confirmation
receives its own result. `tool_batch_disposition` records dispatch decisions;
`tool_batch_proposal_resolution` records confirmation, discard, supersession or
invalidation without inventing another execution attempt. Pending counts derive
from retained call bodies, and execution accounting excludes synthetic results.
