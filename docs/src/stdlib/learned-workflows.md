# Learned workflows

`std/agent/learned` selects reviewed tools under execution qualification and
composes them into an existing agent loop. See [why qualification is required](../learned-workflows.md).

## Dispatcher

`learned_dispatcher(options: LearnedDispatcherOptions) -> LearnedDispatcher`
returns a decision definition, its execution identity, binding options, and two
selection functions:

| Function | Contract |
| --- | --- |
| `assess(llm, request, qualification)` | Requires matching qualification before inference, then applies the certified threshold. |
| `probe(llm, request)` | Selects without qualification for an explicitly isolated experiment. |

Options require `id`, `instructions`, `capabilities`, `policy`,
`evaluator_revision`, `binding`, `runtime_digest`, `grader_digest`, and
`population`. A `LearnedCapability` supplies `id`, `meaning`, the canonical
`ToolCatalogEntry`, `package_digest`, `project_digest`, `verification_digest`,
and `minimum_probability`. Empty required identities throw.

An optional `preflight_tool` checks bound arguments and current project state
before the mutating tool is selected. It must have a distinct name, the same
input schema, `annotations.readOnlyHint: true`, and
`policy.side_effect_level: "read_only"`. Harn enforces the declared effect
ceiling for local and served MCP handlers. A presentation hint alone supplies
no execution policy. The preflight returns canonical tool-result data matching
`LearnedPreflight`: `{kind: "ready"}` or
`{kind: "declined", reason: string}`. Missing or invalid results cause fallback.
The mutating handler must recheck mutable preconditions before writing.

The returned assessment is `selected`, `unqualified`, or a non-match from
[`std/decision`](decision.md). An empty capability list requires no inference.

After binding arguments, middleware asks the native evaluator whether the
parameter values preserve the request's names, values, and roles. This check
uses the dispatcher's evaluation policy. A mismatch, uncertainty, or evaluation
failure causes fallback before preflight or mutation. Normal execution requires
both the configured acceptance probability and the admitted qualification
threshold.

`learned_binding_request(fs, request, capability, arguments, policy)` returns
the typed `LearnedBindingRequest` shared by dispatch, fixtures, and evaluation.
It contains the question, serialized schema and arguments, request, and policy.

## Qualification

`learned_qualify(identity, trials, {risk}) -> LearnedQualification` delegates
threshold certification to `std/eval/selective_risk`. Each `LearnedTrial` has:

| Field | Meaning |
| --- | --- |
| `id` | Unique nonempty trial identifier. |
| `expected` | Expected capability identifier; empty for an incompatible request. |
| `selected` | Committed capability identifier; empty when abstaining. |
| `confidence` | Minimum of applicability, route, and binding-check probabilities. |
| `abstained` | Whether dispatch explicitly declined commitment. |
| `execution` | `not_run`, or `passed`/`failed` with a unique receipt and workspace digest. |

`learned_trial(observation: LearnedObservation) -> LearnedTrial` constructs a
trial from the assessment, explicit runtime disposition, expected label, and
independent execution evidence. Disposition is `abstained` with a nonempty
reason, or `committed` with the assessed capability and decision receipt.
Committed observations also require the answered `binding` evaluation and its
distinct `binding_receipt`. Its accepted answer must match the receipt named by
the runtime disposition. An applicability receipt cannot substitute for it.
The middleware emits one `disposition` checkpoint. Missing records are
unmeasured; absence of a mutation is insufficient to establish abstention.

Committed calls without passing execution evidence count as errors.
Abstained trials mustn't execute. Duplicate identifiers, reused receipts, and
empty execution identities throw.

Declare whether a study measures assessment alone or the combined dispatch
path before collecting evidence. Report assessment mistakes and coverage
alongside execution risk. Changing that protocol requires a new study;
constructing observations cannot repair an earlier failed report.

`learned_qualification_issue(identity, qualification) -> string` returns an
empty string when qualification is usable. Otherwise it returns
`missing_qualification`, `stale_qualification`, `insufficient_execution_evidence`,
`risk_not_certified`, or `inconsistent_qualification`.

`learned_qualification_threshold(identity, qualification) -> Result<float, string>`
returns the admitted threshold or the same refusal reason. Selection and
binding use this shared qualification owner.

## Agent composition

`learned_agent_options(harness, setup) -> LearnedAgentOptions` returns
`llm_caller` and `tool_caller` middleware for the ordinary agent loop. Setup
requires `request`, `dispatcher`, `qualification`, the original `caller`, and
`refresh_identity`; `tool_caller` is optional. Measurement failures cause
fallback to the original caller.

`learned_probe_agent_options(harness, setup, trial)` uses the same execution
path without qualification. The trial requires a nonempty `id`, the current
`workspace_root`, and a different `source_project_root`. The caller owns
workspace isolation and independent grading. Trial execution is recorded as
`qualification_trial`, never as qualified execution.

`learned_agent_options_deferred(harness, fallback, resolve, trial = nil)` resolves
setup once against the live tool registry after MCP bootstrap. The resolver
returns `ready` with setup or `unavailable` with a reason. This avoids copying
tool schemas or guessing MCP dispatch names before connection.

## Source measurements

`learned_dispatch_declaration` validates an executable's declared dispatch
meaning. `learned_capability_snapshot` binds its declaration and canonical tool
to package, project, and verifier file sets. Hosts own the file coordinates;
Harn owns validation and digest construction.
