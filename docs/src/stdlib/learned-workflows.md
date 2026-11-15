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

The returned assessment is `selected`, `unqualified`, or a non-match from
[`std/decision`](decision.md). An empty capability list requires no inference.

## Qualification

`learned_qualify(identity, trials, {risk}) -> LearnedQualification` delegates
threshold certification to `std/eval/selective_risk`. Each `LearnedTrial` has:

| Field | Meaning |
| --- | --- |
| `id` | Unique nonempty trial identifier. |
| `expected` | Expected capability identifier; empty for an incompatible request. |
| `selected` | Selected capability identifier; empty when abstaining. |
| `confidence` | Confidence used by the declared threshold family. |
| `abstained` | Whether the selector declined execution. |
| `execution` | `not_run`, or `passed`/`failed` with a unique receipt and workspace digest. |

Accepted selections without passing execution evidence count as errors.
Abstained trials mustn't execute. Duplicate identifiers, reused receipts, and
empty execution identities throw.

`learned_qualification_issue(identity, qualification) -> string` returns an
empty string when qualification is usable. Otherwise it returns
`missing_qualification`, `stale_qualification`, `insufficient_execution_evidence`,
`risk_not_certified`, or `inconsistent_qualification`.

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
