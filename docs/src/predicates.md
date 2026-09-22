# Decision evaluation contract

`harness.llm.evaluate(id, state, questions, policy)` declares a probabilistic
evaluation. It answers a whole question set over one shared state, in one
request, under one receipt. `harness.llm.evaluate_predicate(id, question,
input, policy)` is its single-boolean projection: the same evaluator, one
question named by the site.

`harn llm evaluate --model openrouter/typesafe/jev-1.13 --state-file state.txt
--questions questions.json --json` runs the same evaluator and prints its full
`{outcome, receipt}` result. The questions file maps IDs to `std/predicate`
records such as `{"safe":{"kind":"boolean","instructions":"Is this read only?"}}`.
The default native policy uses threshold 0.5 and $0.01 per-evaluation and run
ceilings; `--policy policy.json` supplies a complete explicit policy.

For corpus runners, `--request request.json --json` accepts exactly
`{site_id, state, questions, policy}`. Labels are not part of that request.
Typed refusals are successful command executions with a refusal outcome;
malformed input exits with status 2. A receipt distinguishes outer transport
requests from optional gateway-reported downstream attempts. It preserves the
provider's served model verbatim; a moving alias does not prove a revision.
If a gateway reports multiple downstream attempts, evaluation refuses, retains
the reserved cost as uncertain, and closes shared budget admission. The final
response's usage cannot establish the total cost of those hidden attempts.

Script evaluations persist full receipts in the run record's
`evidence.evaluation_receipts`, including zero-dispatch refusals. The journal
shares execution ownership across child VMs, retains at most 1024 receipts,
and reports overflow in `evidence.gaps`.

Execution makes at most one physical provider request per evaluation and
returns a closed outcome naming a receipt. A refusal the route's declared
limits imply is decided before dispatch and makes no request at all. Cache and
replay reuse are separate implementation steps.

## Saved receipt verification

`harn llm evaluate --request request.json --verify-receipt receipt.json --json`
checks a saved request against the receipt emitted by the evaluator. The receipt
file contains the original `receipt` object from the evaluation result. This
operation requires no credentials, spends no budget, and makes no provider call.

The JSON result has schema `harn.evaluation_verification.v1`, a `verified` boolean,
typed `refusals`, the normalized `request_identity`, and its `stable_request_id`.
Exit code 0 means the binding verified; 1 means it was refused; 2 means the input
could not be read or parsed. Unknown identity versions and changed evaluator
contracts return `unsupported_contract`. Older receipts remain readable evidence,
but cannot be upgraded by applying a newer identity contract.

Verification checks canonical input, normalized questions and rubric text,
requested policy, site, route, evaluator contract, and stable request identity.
JSON whitespace and object-key order do not change the binding. Stable identity
does not distinguish repeated invocations: `evaluation_id` is the stable request
identity, while `invocation_id` identifies each occurrence and its outcome handle.
Cache or tape provenance is separate and retains the original receipt in
`reused_from`; reused answers report zero current provider attempts and cost.
This check does not authenticate a provider, validate answer quality, or certify
accounting; consumers must evaluate those facts from the original receipt.

Import the public types from `std/predicate`. This illustrative helper requires
a catalog route named `fixture` on provider `mock` with
`operations = ["text_generation", "decision"]`:

Most decisions in a loop are not shaped like one yes-or-no question. Which of
these skills fits the prompt is a choice; how much damage a command could do is
a score; keep, reword, or drop for each of thirteen messages is thirteen
choices over one shared transcript. A decision model answers all of them in one
request, so they are declared together:

```harn,ignore
import {boolean, choice, score} from "std/predicate"

fn triage(
  llm: HarnessLlm,
  window: {messages: list<string>},
  policy: EvaluationPolicy,
) -> EvaluationOutcome {
  return llm.evaluate("compaction.triage.v1", window, {
    disposition: choice("Keep, reword, or drop this window?", {
      keep: "Still load-bearing for the current task",
      reword: "Useful but far longer than it needs to be",
      drop: "Superseded by later work",
    }),
    risk: score("How much blast radius?", [
      "none", "low", "medium", "high",
    ]),
    safe: boolean("Is this safe to run without asking?"),
  }, policy)
}
```

An `answered` outcome carries one answer per declared question, keyed by
question id. Each answer is typed from its own question: `disposition.choice`
is `"keep" | "reword" | "drop"`, so a `match` on it is exhaustive, and
`risk.level` is the literal union of the declared levels. A partial answer set
is never a smaller `answered`; it is `refused` with reason `schema_invalid`.

The question set is read at the call, because it types every answer and because
the site manifest records which questions a site asks. Write it as a dict
literal whose values are `boolean`, `choice`, or `score` calls with literal
labels. Anything the checker cannot read is `HARN-TYP-036`.

The single-boolean projection keeps its own shape:

```harn,ignore
import "std/predicate"

fn assess(
  llm: HarnessLlm,
  input: {claim: string, observation: string},
) -> PredicateOutcome {
  const policy: EvaluationPolicy = {
    backend: "structured_llm", provider: "mock", model: "fixture",
    effort: "low", temperature: 0.0, threshold: 0.8,
    evaluation_cost_limit: 0.0, run_cost_limit: 0.0,
  }
  return llm.evaluate_predicate(
    "finding.support.v1",
    "Does the observation support the claim?",
    input,
    policy,
  )
}
```

## Arguments

| Argument | Contract |
| --- | --- |
| `id` | Nonempty string literal, unique among sites in one source module. A helper may execute the same site repeatedly. |
| `question` | Nonempty string literal. Its bytes are part of the manifest identity. |
| `questions` | Dict literal of question ids to `boolean`, `choice`, or `score` calls with literal instructions and labels. Ids are unique within a site; labels are unique within a question. |
| `input`, `state` | Closed serializable type: primitives, closed records, typed lists, tuples, string-keyed maps, or unions of those types. |
| `policy` | Closed, compile-time constant `EvaluationPolicy` record naming a catalog route that declares `decision`. |

The policy requires a `backend` of `"structured_llm"` or `"native_decision"`, string fields `provider`
and `model`, and a floating-point `threshold`. Optional finite nonnegative
`evaluation_cost_limit` and `run_cost_limit` tighten inherited authority. Omitting
them does not create budget authority: native calls require an existing
conservative ledger ceiling and retain a finite per-call price bound. Install
that authority before the first model call; earlier unreserved calls cannot be
retroactively covered.
Checking validates this shape and the resolved `decision` operation. A route
with a supported structured transport and text generation derives structured
decision support at the catalog owner. An explicit unsupported schema override
prevents that derivation. The checker makes no provider request and does
not establish credential availability or a resource reservation. Unknown routes
and policies supplied only at runtime refuse literal-site admission; use
`evaluate_request` for typed runtime policy and vocabulary admission.
Which operations a route needs follows from the `decision_protocol` its
capability rule names. `structured_llm` dials the ordinary chat endpoint, so a
route using it needs `text_generation` as well. A route on a native protocol
(`typesafe_system_one`, `vercel_evaluate`, `openrouter_decisions`) needs
`decision` alone, and must not inherit a chat transport from its provider. An
explicit native contract takes precedence over derived structured support.

`effort` and `temperature` are optional chat options. Omit both for
`native_decision`; supplying either refuses with `unsupported_options` before
dispatch. Native routes use the catalog's TypeSafe, Vercel, or OpenRouter
decision protocol, never a chat endpoint. The HTTP client disables retries and
redirects. Provider rate limits and overload responses remain typed outcomes.
Vercel requests apply the catalog's documented zero-retention and no-training
routing restrictions. OpenRouter's native privacy controls remain unverified;
direct TypeSafe retention is account-scoped. The receipt separates applied
controls from gateway-reported routing facts.

Native evaluations reserve their complete request bound in the execution's
shared monetary ledger before dispatch. Concurrent evaluations share the same
allowance. A missing usage report or cancellation retains that reservation as
uncertain spending; only known usage releases unused allowance.

An unvalidated `any`, `unknown`, bare `dict` or `list`, open record, recursive
type, function, or capability handle cannot be an input. Validate external
data before calling the evaluator. Taking the evaluator method as a function
value or calling it through optional method access is rejected; a typed helper
retains the literal source site instead.
The method receiver must retain its static type; erasing it to `any` or an
untyped map does not bypass site checking. Dynamically computed property calls
do not declare admitted sites. Runtime admission owns the route's declared
limits and question kinds; checking establishes neither.

## Runtime vocabulary and routes

`harness.llm.evaluate_request(id, state, questions, policy)` accepts a typed
question map and policy computed at runtime, for registries such as tools and
skills. The site id remains literal and unique, and the state, questions and
policy require closed serializable types. Its answers use the generic
`EvaluationAnswer` union: match the outcome and answer kinds before reading
their values. Runtime vocabulary does not produce a compile-time union of
choice labels.

The source manifest marks these sites `runtime_admission: true`; their empty
question census and null question digest mean not yet bound, not no questions.
The execution receipt binds the actual question set, input, policy and route.
The same evaluator validates catalog support, authority, limits and budgets
before sending at most one request. Static `evaluate` retains its literal
question and route admission and its precisely typed answer labels.

Structured decision eligibility is resolved from the route's text-generation
operation and typed transport strategy: native schema, tool schema, format
schema, or prompt validation. Prompt validation is Harn's completed-response
validation, not provider-native enforcement. Absent transport declarations
retain prompt compatibility; explicit unsupported or unknown declarations
refuse. The catalog, static admission, and execution share this owner. Native
decision protocols retain precedence and their published limits.

Structured calls use ordinary shared monetary admission. Without conservative
authority, `evaluation_cost_limit` is an adaptive pre-call projection including
messages, tools, and the provider-projected output schema. It is not a hard
invoice ceiling. An explicit `run_cost_limit` requests conservative admission;
that mode reserves a supported upper bound and refuses unsupported billing
shapes. A completed receipt records `cost_admission` as `adaptive_projection`
or `conservative_upper_bound`. Native decisions require conservative authority.

Structured receipts carry the authoritative settled `usage`, including reported
cache fields, on valid and malformed completed responses. Missing telemetry
stays unavailable, and native receipts do not invent cache measurements. The
identity's `structured_output_strategy` distinguishes provider schema enforcement
from Harn prompt validation; native protocols leave it null.

## Outcome

`EvaluationOutcome` and `PredicateOutcome` are closed tagged unions over the
same refusals. `EvaluationOutcome` accepts through `answered`, carrying one
answer per question; `PredicateOutcome` accepts through `verdict`, carrying
`verdict: bool`, `confidence: float`, and `evidence: string`. Both carry
`low_confidence`, `refused`, `budget_cut`, `unavailable`, `replay_mismatch`,
`cancelled`, `state_too_large`, `question_invalid`, `rate_limited`, and
`overloaded`. Every variant has a `receipt` reference.

`state_too_large` and `question_invalid` are checked against the route's
declared window and limits before dispatch, so they make zero provider
requests. `rate_limited` and `overloaded` are the provider's 429 and 529
mapped onto the union; neither retries implicitly, because the caller's policy
decides. A batched `low_confidence` carries every candidate answer, the
threshold, and the ids of the questions that fell under it.

An answer's `confidence` names its own provenance in `confidence_kind`. A
boolean's confidence is `max(p, 1 - p)` in its selected verdict
(`binary_probability`); a choice or score carries the vendor's summary of the
distribution (`distribution_shape`); a structured LLM reports its own number
(`model_rationale`). These are different quantities and one threshold does not
equalize their error rates. None of them is calibrated until a calibration
report says otherwise.

Structured answers retain the model's named verdict, choice, or score level
even at low confidence. Their probability fields are synthesized compatibility
projections, never evidence for changing the named answer; their receipt's raw
probabilities are empty because the model measured no distribution. Native
distributions continue to select their highest-probability label. A native
response that also names a contradictory label refuses instead of silently
changing that answer.

```harn
import "std/predicate"

fn disposition(result: PredicateOutcome) -> string {
  match result.kind {
    "verdict" -> {
      if result.value.verdict {
        return "candidate"
      }
      return "not supported"
    }
    _ -> { return "unassessed" }
  }
}
```

An outcome cannot serve directly as a boolean condition. Match its kind before
reading its verdict. The checker rejects unused plain bindings, discard
bindings, and discarded outcome expressions; returning an outcome transfers
its handling to the caller. These checks establish explicit use, not whether
the caller's policy is correct. Model judgments never establish type proofs or
grant authority.
Variant-specific property reads, indexed reads, and destructuring also require
narrowing. Common `kind` and `receipt` fields remain available on every outcome.

## Fitting the ceiling

`state_too_large` tells a caller the state did not fit. It does not make it
fit, and the cases that hit it first are the ones where retrying cannot help:
a session that needs compacting has by definition outgrown the window, so the
whole transcript can never be judged at once.

`evaluation_windows` in `std/predicate` splits a list into windows that each
fit a budget:

```harn
import { evaluation_windows } from "std/predicate"

const windowing = evaluation_windows(harness.llm, items, {
  anchor: latest_user_message,
  budget_tokens: 28000,
  overlap_items: 2,
})
```

Each window carries its index range into the original list, the number of
items it holds, and the size it was planned at. `anchor` is repeated in every
window and counted against every window's budget. `overlap_items` repeats that
many trailing items at the front of the next window, so a question needing
local context does not lose it at a seam. `primary_first_index` is where a
window's own items start, after the repeated ones. The primary ranges
partition the list, so per-item answers join back by index with exactly one
answer per item.

The helper measures with `harness.llm.estimate_state_tokens`, which is the
same call the ceiling compares against the route's window. That is the whole
point of the helper: a chars-per-token approximation produces windows that
measure fine where they are built and are refused where they are sent, which
is the failure the ceiling already reports and the helper exists to prevent.
An item that cannot fit the budget on its own throws, because there is no
window that would hold it.

For native Jev routes, reserve room for the questions too: the published
32,000-token ceiling covers state plus the longest question, and 64,000 covers
state plus all questions. Admission measures both before dispatch. Receipts
keep the state, longest-question and total-request estimates separately.

A native decision route has no equivalent escape. Its admission bounds encoded
input and question count as a condition of being usable, so an evaluation site
whose declared input type has no finite size bound is reported at check time
as `HARN-LNT-079`.

## Site manifest

`harn check --json` includes `files[].predicate_manifest`. A successful analysis
returns schema `harn.predicate_sites.v2` with a `sites` array. Each site contains
its ID, a question-set SHA-256, a `questions` census naming each question's id,
kind, instructions SHA-256 and option count, the canonical input-type SHA-256,
the outcome schema, declared effects, and source path/line/column. The
question-set digest is length-delimited and order-significant, so a renamed or
relabelled question cannot reuse another set's identity. The census includes transitive imports,
so checking an entry file also reports sites declared in its helpers. The
check-result cache preserves the manifest and tracks imported source changes,
model operations, wire identities, and aliases. Removing `decision` support
invalidates a previously successful check.

An empty `sites` array means the checked import closure declares no evaluation sites.
`null` means the file failed checking and has no complete census. A site is a
source declaration, not evidence that a model ran. Runtime inputs, credentials,
and model answers do not appear in this manifest.

## Diagnostics

| Code | Meaning |
| --- | --- |
| `HARN-TYP-030` | Input or policy has no closed serializable type. |
| `HARN-TYP-031` | An outcome is used as a boolean. |
| `HARN-TYP-032` | An outcome is discarded. |
| `HARN-TYP-033` | Site identity is not literal/unique, or indirect invocation hides the site. |
| `HARN-TYP-034` | A variant field is read before narrowing the outcome. |
| `HARN-TYP-035` | A model lacks a required operation, or its route cannot be determined at check time. |
| `HARN-TYP-036` | A question set is not a readable literal, or its ids or labels are not unique. |
| `HARN-LNT-079` | A native decision route is given an input whose declared type has no finite size bound. |

The [design explanation](design/probabilistic-branching.md) defines the remaining
runtime, replay, budget, and provider contracts.
