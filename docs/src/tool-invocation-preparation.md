# Tool invocation preparation

A Harn-owned tool can declare `prepare`, a closure that accepts its normalized
model arguments and returns a portable object containing `operation`. The
operation object is the concrete input shown to consent and policy evaluators.
Other fields can bind the operation to current task facts. The model's input
schema and arguments stay separate from this private object.

Preparation runs before consent with an additional read-only capability ceiling.
Only pure computation and declared filesystem, environment, state, and host
reads are permitted. Process execution, network calls, credential access,
mutations, unknown host calls, and authority requests are refused before their
ordinary approval machinery can grant them. The restriction narrows the
existing scope; it does not replace host policy. Use
`harness.tools.git_repository_identity` for the fixed repository identity read,
rather than granting a generic process or Git tool.

The handler reads the retained object with `tool_invocation_binding()`. Before
each dispatch attempt, Harn re-runs the read-only preparation and compares the
whole object and original model arguments. A changed command, workspace, goal,
or argument rewrite is a terminal refusal requiring a newly prepared call.
An unchanged invocation may use the existing transient retry policy. Calling a
prepared tool through an adapter without an approved preparation context is
refused.

The tool author owns which facts must be bound and must execute the retained
operation. Resolving a new operation in the handler defeats that contract.
Operation observation grants no capability; command safety, execution ceilings,
Stop, and result validation retain their existing owners.
