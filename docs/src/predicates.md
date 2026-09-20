# Predicate evaluation contract

`harness.llm.evaluate_predicate(id, question, input, policy)` declares a
probabilistic evaluation. Its frontend contract is available for checking and
tooling. **Execution currently refuses with an explicit unavailable-evaluator
error.** It makes no model request. Budget admission, receipts, replay, and model
execution are separate implementation steps.

Import the public types from `std/predicate`. This illustrative helper requires
a catalog route named `fixture` on provider `mock` with
`operations = ["text_generation", "decision"]`:

```harn,ignore
import "std/predicate"

fn assess(
  llm: HarnessLlm,
  input: {claim: string, observation: string},
) -> PredicateOutcome {
  const policy: PredicatePolicy = {
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
| `input` | Closed serializable type: primitives, closed records, typed lists, tuples, string-keyed maps, or unions of those types. |
| `policy` | Closed, compile-time constant `PredicatePolicy` record naming a catalog route that declares `decision`. |

The policy requires `backend: "structured_llm"`, string fields `provider`,
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

`PredicateOutcome` is a closed tagged union. Its kinds are `verdict`,
`low_confidence`, `refused`, `budget_cut`, `unavailable`, `replay_mismatch`, and
`cancelled`. Every variant has a `receipt` reference. Only `verdict` exposes
an accepted `value` with `verdict: bool`, `confidence: float`, and
`evidence: string`.

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
returns schema `harn.predicate_sites.v1` with a `sites` array. Each site contains
its ID, question SHA-256, canonical input-type SHA-256, outcome schema, declared
effects, and source path/line/column. The census includes transitive imports,
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

The [design explanation](design/probabilistic-branching.md) defines the remaining
runtime, replay, budget, and provider contracts.
