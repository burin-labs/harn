# Retained tool proposals

An agent response can propose a read and an edit together. The dispatcher runs
the read first so the model can reconsider the edit using the result. It stores
the edit's arguments and returns a proposal identifier in the deferred result.

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

A current response may resume an edit and request additional reads together.
After those reads succeed, the dispatcher executes the confirmed edit without
requesting another confirmation. It rechecks pending controls and the original
run, accepted control revision and context first. A failed read or changed
control/context prevents continuation. Fresh edits remain retained, and resumed
verification or completion calls keep their phase barriers. Confirmation grants
no authority to execute automatically in a later response.

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
