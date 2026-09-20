# Decision evaluation contract

`harness.llm.evaluate(id, state, questions, policy)` declares a probabilistic
evaluation. It answers a whole question set over one shared state, in one
request, under one receipt. `harness.llm.evaluate_predicate(id, question,
input, policy)` is its single-boolean projection: the same evaluator, one
question named by the site.

Their frontend contract is available for checking and tooling. **Execution
currently refuses with an explicit unavailable-evaluator error.** It makes no
model request. Budget admission, receipts, replay, and model execution are
separate implementation steps.

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
    risk: score("How much blast radius?", ["none", "low", "medium", "high"]),
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

The policy requires a `backend` of `"structured_llm"` or `"native_decision"`, string fields `provider`,
`model`, and `effort`, and floating-point fields `temperature`, `threshold`,
`evaluation_cost_limit`, and `run_cost_limit`. Checking validates this shape and
the route's declared `decision` operation. A text-generation capability alone
does not grant decision support. The checker makes no provider request and does
not establish credential availability or a resource reservation. Unknown routes
and policies supplied only at runtime refuse admission.
The current `structured_llm` backend also requires `text_generation`; a native
decision-only route cannot inherit a chat transport from its provider.

An unvalidated `any`, `unknown`, bare `dict` or `list`, open record, recursive
type, function, or capability handle cannot be an input. Validate external
data before calling the evaluator. Taking the evaluator method as a function
value or calling it through optional method access is rejected; a typed helper
retains the literal source site instead.
The method receiver must retain its static type; erasing it to `any` or an
untyped map does not bypass site checking. Dynamically computed property calls
do not declare admitted sites. The current runtime refuses all evaluation, and
the executor must require an admitted artifact site before enabling execution.

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

The [design explanation](design/probabilistic-branching.md) defines the remaining
runtime, replay, budget, and provider contracts.
