# Probabilistic branching

Status: proposed, 2026-09-19. This explanation asks how a model's judgment can
control a Harn branch without hiding uncertainty, provider effects, or test
inputs. It proposes one typed predicate expression over existing runtime
services. It does not claim that the syntax is implemented or that any model
has passed an application quality gate.

The recommendation is an explicit `predicate { ... }` expression returning a
closed outcome. A caller handles that outcome with `match`, then uses an ordinary
`if` on an accepted verdict. A model answer cannot establish a type refinement,
grant authority, discharge an existing deterministic requirement, or turn a
missing observation into a negative answer.

## What the recent work changes

TypeSafe AI's September 15 introduction separates producing language from
answering bounded questions. Jev returns structured decisions and distributions;
the surrounding program composes them. Its published comparisons use particular
workflows and reference-model judgments. They are vendor evidence, not a
calibration certificate for a new application's predicates. Restricting the
answer space prevents out-of-schema answers; it does not prevent a confidently
wrong answer. [Release article][jev-intro]

The distinction matters for Harn: the useful abstraction is an observable model
effect with a typed answer and an abstention path. Temperature zero, a JSON schema,
and a plausible explanation cannot make that effect a deterministic proof.

### Prior art

The following survey was checked on 2026-09-19. Cost descriptions identify what
work is charged, rather than comparing unrelated provider tariffs. Test evidence
describes the inspected implementation or documentation, not a claim that its
whole test suite was run for this dossier.

| System | Syntax and answer type | Containment of nondeterminism | Testing evidence | Cost mechanism |
| --- | --- | --- | --- | --- |
| Jev / System One | `client.system_one(state=state, questions={"q": Noul(instructions=question)})`; `Noul` returns a yes-probability. `Choice` returns a label, distribution, and confidence; `Score` returns a rubric score and distribution. | Finite answer spaces; independent questions over explicit state; application thresholds. Noul has no separate confidence field. | The release describes workflow comparisons; the public SDK and LLM adapter expose typed responses. These do not prove a new predicate's accuracy. | Announced input-token billing, no output-token charge; batching shares state. No generation of explanatory strings. [Primitives][jev-primitives], [confidence][jev-confidence], [SDK][jev-sdk]. |
| Probably 0.1 | `if value feels "urgent" with confidence 80% { ... } otherwise maybe { ... } else { ... }`; internal choice or uncertainty, rather than a standalone boolean expression. | Explicit input; symmetric winning-probability threshold; bounded loops and calls; recorded decisions and random draws. | Downloaded tests use a provider that throws on unexpected calls; replay rejects missing, changed, and surplus effects. | Local runs make judge or text-generation calls; hosted examples replay recordings and refuse cache misses. [Playground][probably], [source archive][probably-source]. |
| DSPy | `dspy.Predict("text -> relevant: bool")`; class signatures support richer types. Historical `dspy.Assert(check, feedback)` takes a Python boolean, not an English condition. | Typed parsing, LM caching, bounded refinement/backtracking; assertions themselves do not make model answers deterministic. | `DummyLM` supplies explicit outputs; the assertions paper evaluates constraints and task quality. Current assertion docs mark that API deprecated in favor of refinement. | Inference calls plus retries; optimization also consumes teacher/candidate calls. [Signatures][dspy-signatures], [assertions][dspy-assertions], [cache][dspy-cache], [test model][dspy-dummy]. |
| Marvin | `@marvin.fn` on a Python function with a return annotation; `marvin.classify(text, Labels)` returns a declared label or enum. | Return-type validation and an explicit label set; the decorator can also receive conversational context. No language-wide replay contract is claimed. | The inspected AI tests assert boolean and enum answers and include model-based equality checks and flaky markers. Such tests assess model behavior, not hermetic replay. | Agent/model calls and any configured retries; type annotations do not bound cost. [Decorator][marvin-fn], [classification][marvin-classify], [tests][marvin-tests]. |
| LMQL | `argmax "... [ANSWER]" where ANSWER in ["yes", "no"]`; constrained string, with optional label distribution. | Decoder constraints, caching and explicit decoding strategy; constraints limit tokens, not truth. Inference certificates preserve model-call context. | The distribution test uses a seeded random model; application semantics still need a labeled corpus. | Generated/scored tokens, decoder work and provider requests; constraints can reduce rejected generations. [Constraints][lmql-constraints], [certificates][lmql-certificates], [test][lmql-test]. |
| Guidance | `lm += select(["yes", "no"], name="verdict")`; captured constrained choice. | Grammar/choice constraints compose with ordinary Python; they do not provide calibrated confidence or mandatory abstention. | `guidance.models.Mock` supports deterministic generated bytes. A mock tests control logic; live quality is a separate question. | Inference plus grammar/selection work; local compute or provider billing. [Select][guidance-select], [mock][guidance-mock]. |
| Instructor | `client.create(..., response_model=Verdict)`; a validated Pydantic result. | Validation and bounded retries; a failed validator can cause another model call. | Inspected retry-budget tests count physical calls, reject missing usage, and test exact boundaries. They also demonstrate that a successful response can cross a post-response token budget. | Every retry consumes tokens; validation is local. A post-response budget is not pre-dispatch cost admission. [Validation][instructor-validation], [budget tests][instructor-tests]. |
| Outlines | `model(prompt, Verdict)` with a Python type or schema; structured result or constrained text according to the generator/backend. | Constrained decoding or a structured provider interface; schema validity does not assert factual correctness. | Generator tests include mock clients and tests of structured-generation behavior. | Model inference plus constraint compilation/decoding; local accelerator time or provider tokens. [Documentation][outlines], [tests][outlines-tests]. |
| Semantic Kernel | `FunctionChoiceBehavior.Auto()` with typed registered functions; model-selected calls and arguments. | Registered function schemas, execution filters and surrounding program policy; a planning loop is broader than a predicate. | Inspected connector tests use HTTP response fixtures for function selection and arguments. | Each planning/model step and invoked tool; the loop needs an external bound. Current docs replace deprecated Stepwise/Handlebars planners with function calling. [Planning][sk-planning], [tests][sk-tests]. |

Probably is the closest language precedent. Its uncertainty branch is useful,
but an omitted `otherwise maybe` executes neither boolean branch. Harn should
require an explicit outcome disposition. Probably also permits probability
sampling in a `chaos` block; this proposal does not add random branch selection.
The source archive was inspected, including its interpreter and replay tests;
the hosted site's behavior alone is not evidence of live inference.

Jev's Noul probability and Choice confidence are different quantities. Choice
confidence summarizes a distribution; a binary adapter can instead derive
confidence in its selected verdict as `max(p_yes, 1 - p_yes)`. An LLM's
self-reported number has yet another provenance. The receipt must distinguish
them. A threshold does not establish equal error rates across these sources.

The public [System One LLM adapter][jev-adapter] is also instructive: it records
individual attempts and corrective retries. Its documented replay example sends
a request to a provider again. That is request reproduction, not the offline
execution required for Harn tests.

## What Harn already owns

This reading uses source revision `fb8d82ed5a3da77ea420118fdbdb0f77e9fd3a00`.
The installed language skill, public documentation, implementation, and focused
PR history were consulted. An inventory of existing capabilities is necessary
because another cache, judge loop, or run log would duplicate semantic owners.

| Existing owner | What is present | What this proposal must add |
| --- | --- | --- |
| [LLM call][harn-call] and [schema-as-type][harn-schema] | Typed options, schema emission from Harn types, canonical response/outcome/usage, structured-result diagnostics. The documented `response.data` still needs schema narrowing. | A closed predicate result, explicit input type identity, and refusal semantics at one language boundary. |
| [Typed checkpoint][harn-checkpoint] | Schema-bound output, validation, repair, attempt traces, and emitted checkpoint evidence. Defaults can permit multiple attempts. | A predicate profile with one physical request, no implicit repair, and a receipt on success, uncertainty, and refusal. |
| [Completion judge][harn-judge] and [requirements][harn-requirements] | Bounded judgment, model admission, deterministic verification, typed evidence roles, and pending requirement counts. | A reusable judgment below these policies, without taking over their completion decisions. |
| [LLM cache handler][harn-cache-handler] and [cache primitives][harn-cache] | Persistent result caching, canonical request keys, TTL, hit/miss events, and avoided-call evidence. Keyed checkpoints also compare an explicit identity. | A predicate identity including typed inputs and semantic policy, isolated test lookup, and single-flight execution for concurrent identical evaluations. |
| [Testbench][harn-testbench] and [tape][harn-tape] | Mocked model responses, controlled time, recorded external effects, request digests, and fidelity comparisons. | A predicate operation on that tape, strict consumption, and declared outcome fixtures that cannot fall through to live inference. |
| [Approval calibration][harn-calibration] and evaluation machinery | Existing grader policies, calibration data, replay and experiment primitives. | Predicate-specific held-out labels and calibration evidence. An existing calibrated grader does not automatically calibrate a new question. |

[Completion requirements history][harn-requirement-pr] supplies a concrete
constraint: separate adjudicators cannot erase one another's pending evidence.
The proposed result is evidence supplied to a policy, not authority to seal an
agent run. Existing Flow/type predicates remain deterministic narrowing tools;
probabilistic predicates must not reuse their proof semantics.

A previous classifier cutover exposed the testing failure this design must
prevent: replacing a deterministic rule with a model call left offline tests
exercising an empty fallback rather than the original decision. The durable
lesson is to inject an explicit typed classification into consumer tests and
measure model quality separately. No phrase matcher, fixture name, or desired
branch may manufacture the fixture verdict.

## Proposed expression and types

All Harn blocks below are proposed syntax or integration sketches, deliberately
excluded from runnable documentation snippets. They are design examples, not
instructions for the current release.

```harn,ignore
type FindingInput = {
  claim: string,
  observed_effect: string,
  changed_behavior: string,
  evidence_digest: string,
}

const finding: FindingInput = candidate
const result = predicate {
  id: "review.load_bearing.v1",
  question: "Does the observation support a material failure?",
  input: finding,
  policy: review_policy,
  harness: harness,
}

match result.kind {
  "verdict" -> {
    if result.value.verdict {
      queue_for_review(result.receipt)
    } else {
      retain_as_nonblocking_candidate(result.receipt)
    }
  }
  "low_confidence" -> { request_more_evidence(result) }
  "refused" -> { retain_unassessed(result) }
  "budget_cut" -> { retain_unassessed(result) }
  "unavailable" -> { retain_unassessed(result) }
  "replay_mismatch" -> { fail_replay(result) }
  "cancelled" -> { stop_without_branching(result) }
}
```

`predicate` is a contextual expression introducer, recognized before this closed
record body. Existing identifiers named `predicate` remain legal elsewhere.
The compiler supplies a source site and input-schema fingerprint. `id` and
`question` are compile-time strings; the input is evaluated once before
dispatch. Its closed, serializable Harn type is required. Open `dict`, `any`,
unvalidated `unknown`, functions, handles, cycles, and non-finite floats are
rejected at this boundary. No surrounding locals or conversation are captured.

The expression uses the supplied `Harness` for authority and the current run
record. It never creates credentials, grants, or a hidden session. The model
receives only the question, frozen input, and versioned evaluator instruction.
Untrusted text in the input is data; it cannot change policy or request tools.

The structured backend's one output schema is:

```harn,ignore
type PredicateVerdict = {
  verdict: bool,
  confidence: float,
  evidence: string,
}
```

All fields are required, extra fields are refused, confidence must be finite and
within `[0, 1]`, and evidence is bounded to 2,048 UTF-8 bytes. Confidence describes
the selected verdict, including a negative verdict. Evidence is a short cited
observation or rationale, never a request for hidden reasoning. Valid shape and
nonempty prose do not prove that a cited observation supports the judgment.

`PredicateOutcome` is a closed discriminated union. Every variant carries a
receipt reference; only `verdict` exposes an accepted `value`.

| Kind | Payload and meaning | Required disposition |
| --- | --- | --- |
| `verdict` | `value: PredicateVerdict`; schema valid and threshold met | The ordinary boolean branch may run. |
| `low_confidence` | `candidate: PredicateVerdict`, `threshold: float` | Abstain, request evidence, or invoke a separately budgeted policy. Neither boolean branch runs implicitly. |
| `refused` | Closed reason: `provider_refusal`, `schema_invalid`, or `output_truncated`; bounded diagnostics | Preserve an unassessed result. A schema error is not `false`. |
| `budget_cut` | Closed limit kind and requested/remaining quantities | Stop or defer. No retry is implicit. |
| `unavailable` | Closed reason: `model_unconfigured`, `unsupported_options`, `transport_failed`, `authority_denied`, `producer_cancelled`, or `cache_miss` | Report the unavailable evaluation. Never choose a default verdict. |
| `replay_mismatch` | Expected/actual identity and occurrence, without raw sensitive inputs | Fail replay/test infrastructure. |
| `cancelled` | Accepted control-event reference | Return control without running either branch. |

Callers can pass the whole outcome to a typed policy function. Extracting a
boolean without narrowing is a checker error. An exhaustive `match` can group
non-verdict kinds when they intentionally share a disposition. Neither casting
the outcome to `bool` nor using it as a type guard is supported. Ordinary Harn
escape hatches are not a security boundary; runtime policy still enforces the
same limits.

### Model execution

The initial implementation uses the existing structured LLM transport. A
`PredicatePolicy` fixes the provider route, resolved model ID, effort, threshold,
token bounds, deadline and admitted budget handle. There is no ambient actor-model
fallback. Effort is explicit; examples assume `low`. Temperature is exactly `0`.
An endpoint that cannot honor that combination returns `unsupported_options`,
rather than silently dropping a parameter. Model selection remains catalog-owned.

Provider calls use strict schema validation. Transport retries, schema retries,
LLM repair, tool use, conversation continuation, and provider failover are disabled
for this profile. One expression makes at most one physical request. A caller
wanting escalation declares another evaluation whose receipt and cost remain
visible. Existing checkpoint defaults must therefore be overridden at the owning
boundary, not merely documented away.

Native Jev integration is a separate provider capability follow-up. Its Noul
answer cannot satisfy a generated `evidence: string` contract directly. A future
adapter may produce an input-reference string mechanically and mark
`evidence_kind: "input_reference"`; an LLM result uses `"model_rationale"`.
It must not invent a rationale or make a second hidden text-model call. The
receipt records raw probability semantics and their conversion. No Jev route
support or cross-provider confidence equivalence is claimed by this dossier.

### Cache and determinism

The minimum identity is predicate text plus typed input plus resolved model ID.
That tuple alone is insufficient when effort, the evaluator instruction, or an
acceptance threshold changes. The complete key is a versioned, canonical digest
of:

```text
predicate text bytes + input schema fingerprint + canonical input value
+ provider route + resolved model ID/revision
+ evaluator version + output schema version + effort + temperature
+ confidence policy/calibration version + decoding/token options
```

Encoding is length-delimited and domain-separated, not string concatenation.
Record keys sort canonically; list order and declared scalar types remain
significant. The encoding specifies optional absence versus explicit null and
normalizes negative zero. Equivalent record insertion order cannot change the
key. Input evidence snapshots include their content digests, not mutable pointers.

The existing cache stores validated verdict candidates and their origin receipt.
It never stores a refusal, transport error, or budget cut as a successful answer.
Every use re-applies the current threshold and records its own outcome. Cache
scope defaults to the run. Cross-run reuse requires an explicit namespace,
retention policy, and immutable model revision; aliases without a stable revision
remain run-local. Namespace isolation follows the owning execution authority.

An in-flight key has one producer. Concurrent consumers await its validated
candidate and each receive a receipt; only the producer holds the provider-call
reservation. This coalescing is run-local; cross-run cache reuse applies only to
completed entries. If the producer is cancelled, waiters receive an unavailable
outcome unless their own run is also cancelled. They do not launch replacements.
A completed negative verdict is cacheable. A low-confidence
candidate is also reusable and remains uncertain; repeated evaluation must not
silently become resampling until the desired answer appears.

Temperature zero reduces sampling variation but does not promise reproducibility
across provider deployments, hardware, or uncached runs. Determinism means that
the same recorded inputs and recorded outcomes reproduce the same program
decisions. It is a replay contract, not a claim about repeated inference.

### Resource ceilings

The proposed initial profile has these ceilings, independent of provider price:

| Resource | Per evaluation | Per run predicate sub-budget |
| --- | --- | --- |
| Physical model requests | 1, including every attempted dispatch | 8, shared across nested workers |
| Input tokens, including evaluator and schema overhead | 8,192 maximum | At most the sum of admitted requests |
| Output tokens, including charged reasoning tokens | 256 maximum | At most the sum of admitted requests |
| Evaluations, including cache hits | 1 | 32 |
| Elapsed time | 10 seconds or the shorter remaining run deadline | Existing run deadline, never extended |
| Monetary allowance | Required finite `evaluation_cost_limit` | Required finite `run_cost_limit`, no greater than the parent grant |

No monetary default is inferred from an absent limit. Policy admission requires
both allowances and an enforceable provider token bound. With fixed catalog
input/output rates `r_in` and `r_out`, the request reservation is at most
`8192 * r_in + 256 * r_out`, including any declared request surcharge. An unknown
price, unbounded charge category, or unenforceable output cap refuses dispatch.

The shared runtime budget reserves that upper bound atomically before a request,
then settles canonical usage and releases the unused reservation. Unknown usage
retains the reservation and is marked unknown, never recorded as free. A
transport timeout may still be billable. Parent and predicate allowances both
apply; child runs cannot mint fresh allowance. A cache hit charges no new model
usage but consumes an evaluation slot. Cancellation releases only reservations
for work proven not to have dispatched.

This is an admission ceiling under the declared provider pricing and limits,
not a guarantee about an external billing service. An unexpected provider charge
is recorded as an accounting breach and stops further predicate dispatch.
No background refresh, speculative calls, or automatic batching are included.

### Receipts and the run record

The runtime appends `predicate_started` before dispatch and one settled
`predicate_evaluated` event before returning a usable result. Both use the
existing event journal. The run-record projection exposes
`predicate_evaluations`, joined to ordinary provider-call usage rather than
adding that usage a second time.

```text
PredicateReceipt v1
  evaluation_id, parent_run_id, source_site, predicate_id
  predicate_digest, input_type_digest, input_digest, request_key
  evaluator_version, output_schema_version, policy_digest
  requested_route, resolved_provider, resolved_model, model_revision?
  effort, temperature, confidence_kind, calibration_id?, evidence_kind
  source: live | cache | tape | fixture
  source_receipt_id?, tape_sequence?, fixture_id?
  outcome: PredicateOutcomePayload
  provider_call_ids, physical_attempt_count
  reservation, usage: canonical LlmUsage, accounting_status
  elapsed_ms, cache_state, control_event_id?
```

`confidence_kind` distinguishes `self_reported`, `binary_probability`, and a
versioned `calibrated` mapping. A receipt proves what was evaluated, returned,
and consumed; it does not certify the truth of the model's evidence. The caller's
existing action/completion receipt links the evaluation ID when the result
influences a decision. This makes an evaluated-but-unused predicate distinguishable
from a predicate that actually selected a branch.

Cache/tape/fixture receipts report zero new provider attempts and retain source
usage only as provenance. Run summaries include evaluated, accepted, uncertain,
refused, pending, and physical-request counts plus failing predicate IDs. An
empty evaluation list cannot be summarized as a passing predicate gate.

An accepted stop cancels evaluation and forbids later response delivery to the
branch. On recovery, a started but unsettled evaluation becomes interrupted
evidence; it is not repeated automatically. A receipt persistence failure fails
the run before branch execution. A process crash can leave a start record
without a settlement; the design does not claim an atomic transaction with the
provider's remote service.

Raw input and model text follow existing artifact access, retention, and
redaction policy. Public summaries use digests and bounded descriptions.
Replay requires authorized fixture/tape material; a digest alone cannot
reconstruct redacted content. Tampered candidate/schema/key data is refused
before reuse.

## Checking and hermetic tests

`harn check` verifies the closed expression fields, literal identity/question,
serializable input type, policy type and capability effect. The result is
`PredicateOutcome`, never `any` or `bool`. The ordinary union checker supplies
narrowing and exhaustive matching. It must reject a missing uncertainty arm,
an outcome used directly as an `if` condition, and any attempt to refine an
untrusted value's type from a probabilistic answer. It cannot prove that an
English question is suitable for machine judgment.

Compilation emits a manifest of predicate sites, text/schema digests, and their
effects. That manifest lets tooling identify hidden model work even through a
helper function. Runtime configuration supplies the actual model route and
resource grant; checking source neither reaches a provider nor prices a run.
Portable execution uses the same frontend and a declared capability suspension.
A backend without the evaluator refuses that capability; it does not interpret
the question itself.

`harn test` runs predicate effects in a runner-owned offline mode. Tests declare
either exact tape entries or explicit typed fixture outcomes. Neither ambient
credentials nor a warm production cache can affect a test. Missing, changed,
duplicate, or unconsumed fixture occurrences fail the test, even if application
code catches the returned `replay_mismatch`. Negative tests declare that mismatch
as an expected infrastructure failure explicitly.

```harn,ignore
with_predicate_fixtures(harness, [
  {
    predicate_id: "review.load_bearing.v1",
    question: "Does the observation support a material failure?",
    input: declared_finding,
    model: fixture_model,
    policy: fixture_policy,
    occurrences: 1,
    outcome: {
      kind: "verdict",
      value: {
        verdict: true,
        confidence: 0.93,
        evidence: "Observation E1 supports the claimed failure.",
      },
    },
  },
], fn() {
  const result = review_candidate(
    harness, declared_finding, fixture_policy,
  )
  assert_eq(result.action, "queue_for_review")
  assert_eq(result.predicate_receipts.len(), 1)
})
```

This fixture is a human-authored test input, not a predicted label. The runner
derives its identity from the declared question, type, value, model and policy,
not by reading the live expression's desired result. `fixture` provenance remains
visible. Tests of JSON parsing and schema refusal use raw response tapes through
the structured-call boundary rather than bypassing that boundary with typed
outcomes.

Fixture outcomes undergo the same range, schema and threshold checks as live
outcomes. A fixture cannot declare `verdict` with confidence below its policy's
threshold. Fixture policies and expected identities are declared independently
of production policy constructors. Raw-tape contracts also assert the outgoing
request's schema, model, effort, temperature and physical-attempt count.

The tape extends the existing versioned format with a predicate record for every
evaluation, including cache hits and failures. It binds the request identity,
occurrence and result, and preserves the original branch-relevant policy.
Replay consults the tape before any live cache or provider. Changed input,
question, model, effort, schema, or policy produces a mismatch. The tape records
both cache-hit and cache-miss decisions, so replay never depends on current TTL
or external cache contents. Format compatibility must be explicit to older
readers.

Hermetic coverage proves transport handling, branch selection, receipts, budget
admission and replay. Semantic quality requires a separate labeled evaluation
set: accepted, rejected, ambiguous and adversarial inputs; a frozen predicate
and model version; held-out calibration; error rates at the chosen threshold;
coverage/abstention rate; and cost/latency distributions. A single live smoke
read can show reachability only. Fixture success is never reported as model
accuracy.

## Three worked integrations

These examples derive from existing review, deliverable, and exception-census
pipelines. Their deterministic checks remain authoritative. The snippets show
the proposed semantic addition; no pipeline is converted by this dossier.

### A finding that matters

The existing review pipeline separates claim, quoted source, evidence command,
verification state, change witness, and blocking judgment. A predicate can
assess whether the observed consequence matters after the source and change
witness have been checked. It cannot mark an unexecuted command as verified or
invent causality from a file location.

The first syntax example is this integration. Given a witnessed lost update,
fixture `true` queues the candidate; given a witnessed cosmetic discrepancy,
fixture `false` retains a nonblocking candidate. Low confidence retains the
finding as unassessed. The final review policy still requires its own evidence
and prior-finding reconciliation before deciding whether a change can proceed.

Falsifier: a high-confidence positive answer over an unverified change witness
must not produce a verified blocking finding. The negative control removes the
witness check and must make that forbidden result observable.

### A deliverable that satisfies its description

The existing deliverable loop terminates through a successful terminal tool and
has a deterministic terminal callback for incomplete work. Predicate evaluation
belongs before submission, while the run can still spend its admitted budget.
It cannot replace artifact readback, verifier results, the acceptance ledger,
or the terminal callback.

```harn,ignore
type DeliverableInput = {
  requirement: string,
  artifact_digest: string,
  artifact_excerpt: string,
  observation_ids: list<string>,
}
const assessment = predicate {
  id: "deliverable.semantic_fit.v1",
  question: "Does this artifact substantively meet the requirement?",
  input: deliverable_input,
  policy: deliverable_policy,
  harness: harness,
}
const candidate = semantic_assessment(assessment)
return submit_to_existing_requirement_gate(
  candidate, artifact_facts, verifier_facts,
)
```

`deliverable_input` has the declared `DeliverableInput` type.
`semantic_assessment` exhaustively handles the same outcome union: only a
positive accepted verdict proposes a met semantic row; negative or uncertain
answers retain a pending row; operational failures retain an unassessed row.
It does not set the whole deliverable to complete.

A fixture positive over a complete explanation can satisfy the semantic row
when the existing gate accepts its evidence. The same positive with a missing
artifact or red verifier leaves completion blocked. A budget cut never releases
the requirement. The counterexample for the gate is a predicate-positive,
verifier-red submission; removing the deterministic gate must expose the false
completion.

### An exception rationale that supports its claimed boundary

The existing census deterministically joins executable exceptions to a typed
reviewed registry, checking duplicates, required fields, entrypoint consistency
and boundary declarations. Therefore “is this bypass reviewed?” is the wrong
model predicate. Review status is already a fact.

```harn,ignore
type ExceptionRationaleInput = {
  declared_boundary: string,
  required_capabilities: list<string>,
  rationale: string,
  observed_denial: string,
  evidence_digest: string,
}
const assessment = predicate {
  id: "exception.rationale_support.v1",
  question: "Does the observation support this exception rationale?",
  input: rationale_input,
  policy: advisory_policy,
  harness: harness,
}
return annotate_existing_census(census, assessment)
```

`rationale_input` has the declared `ExceptionRationaleInput` type. The annotation
can request human re-review when the evidence concerns another boundary. It
cannot add a registry row, remove a violation, authorize execution, or turn an
unreviewed exception into a reviewed one. The complete outcome, including
uncertainty and failure, remains on the annotation.

Positive and negative fixture labels exercise that advisory route. The decisive
negative control supplies an unregistered exception plus an accepted positive
predicate: the census must remain failing with the same violation. Removing
that separation must make the false approval visible.

## Alternatives and decision

| Alternative | Merit | Decision |
| --- | --- | --- |
| Ordinary library function | Existing checkpoints, typed schemas and cache wrappers can implement much of the behavior. It is the strongest alternative. | Reject as the complete authoring surface: ordinary calls do not give every site the proposed compiler-emitted predicate manifest and closed input/effect contract. Keep library code for orchestration beneath the expression. |
| A stdlib `predicate()` builtin | Central runtime enforcement and a compact call are feasible. | Reject a second public surface alongside syntax. One internal registered capability owns execution; the expression lowers to it. If the compiler cannot demonstrate additional site/type diagnostics, the syntax proposal should be rejected in review rather than justified by aesthetics. |
| Reuse the completion judge | Already handles structured judgment, budgets and evidence. | Reuse its lower-level transport/checkpoint owners, not completion policy. A standalone classification should not acquire agent-loop termination, gap arbitration, or requirement-ledger semantics. |
| Do nothing | Avoids language and tooling changes. | Reject because each caller would continue assembling cache identity, failure handling and replay requirements independently. The proposal is justified only if the three integrations share one enforced contract. |

The expression deliberately does not add fuzzy `while`, probability sampling,
implicit model escalation, arbitrary model-authored code, or a new provider
registry. Native Jev support and broader classifiers can follow the same
execution contract after capability and calibration evidence exists.

## Adversarial second pass

The first draft was reread against the following counterexamples before review.
These are design attacks, not claims that unimplemented runtime tests have passed.

### A hermetic green that measures nothing

Attack: prewarm a live cache, remove a fixture, and let a catch-all branch turn
the replay error into an ordinary unassessed result. The test can pass while its
expected decision never occurs. A second version declares a high-level fixture
verdict that violates the configured confidence threshold.

Changes: test mode ignores external caches, makes unexpected fixture consumption
a runner failure even when caught, and validates fixture outcomes against policy.
The branch test must assert a nonempty receipt count, the exact evaluation ID
on the consuming action receipt, and the action the branch actually performed.
Raw response tapes separately exercise the
provider schema boundary. The decisive negative control deletes the single
expected record while a live provider stub would answer successfully: the test
must fail and the stub's call count must remain zero. An extra unused fixture
also fails. A fixture-provided correct label still proves no model competence.

### One cheap expression that creates many requests

Attack: the structured checkpoint's repair defaults, transport retry policy,
concurrent duplicate calls, and child runs each consume allowance independently.
A cap checked after the answer arrives reports the excess but cannot prevent it.

Changes: one physical request is a transport-level maximum, including provider
client retries; monetary reservation precedes dispatch in the shared parent
ledger; unknown accounting retains its reservation. Single-flight ownership is
run-local, and cancellation cannot transfer an already spent reservation into a
new request. The 32-evaluation cap also bounds free cache-hit loops. The negative
control gives two concurrent requests allowance for only one: exactly one may
dispatch. Removing the atomic reservation must make the second dispatch visible.
A malformed response must consume one request and return refusal, with no repair.

### A probabilistic answer that launders authority

Attack: name a predicate “is this bypass reviewed?”, get `true`, and use that
answer to clear a registry violation. Similarly, an artifact-fit answer could
erase an unrun verifier or a review answer could manufacture verified causality.

Changes: the third example asks only about rationale support and remains
advisory. All examples keep the existing deterministic gate and name a forbidden
positive-model/negative-fact combination. A probabilistic verdict never refines
an input's type or represents a permission grant. Evidence of the selected action
is mandatory, so merely writing an unused receipt cannot satisfy adoption gates.
The checker cannot decide whether an English predicate duplicates a typed rule;
that remains a review obligation backed by the three regression contracts.

The second pass also tightened the syntax tradeoff: the proposed compiler work
must demonstrate site manifests and type diagnostics
that a library alone would not supply. Otherwise the library alternative wins.
That is a falsifiable reason for syntax, not a presumption that new keywords help.

## Implementation sequence

This sequence begins only after design approval. Each PR includes a changelog
fragment, hermetic evidence, a negative control and the current conformance gate.
Sizes are estimates of changed, hand-maintained lines including tests, not
targets; generated grammar output is additional. Dependencies are sequential.

| PR | Owning change and estimated size | Falsifier and negative control | Required gate |
| --- | --- | --- | --- |
| 1. Parser and checker | Contextual expression, typed site manifest, outcome union, capability lowering, spec and editor grammar. Roughly 700–1,200 lines. Until runtime support lands, execution refuses explicitly. | Positive and negative parser/type fixtures; deleting the outcome/type restriction must admit an illegal direct boolean or nonserializable input. No provider call is possible in this PR. | Focused parser/checker tests, `harn check`, conformance, Harn lint/format, tree-sitter and generated-spec drift. |
| 2. Runtime and receipt | One evaluator using existing structured transport, strict outcome conversion, reservations, cancellation, journal/projection. Roughly 900–1,500 lines. Raw provider tapes supply hermetic responses from the start. | A malformed response, no model, accepted stop or exhausted budget cannot select a branch; disabling admission must dispatch the forbidden request. Assert emitted and consumed receipt IDs through a helper call too. | Runtime mechanism contracts, `harn check`, conformance, run-record projection tests and binary drift. |
| 3. Cache and tape | Versioned typed key, isolated namespace, single-flight cache, predicate tape record and strict fixture consumption. Roughly 700–1,200 lines. | Same key makes one physical request; changed input/schema/model/policy misses; replay with deleted/extra entry fails with zero live calls. Disabling mismatch checking must expose incorrect replay. | Cache/replay/fidelity contracts, `harn check`, conformance and tape-version compatibility tests. |
| 4. Stdlib helpers | Typed outcome policies, declared fixture helper, generic review/deliverable/census examples. Roughly 350–650 lines. No existing rule replacement. | Positive predicate plus red deterministic evidence stays blocked in all three worked contracts; removing each deterministic guard exposes a false accept. | Owning Harn tests, `harn check`, conformance, strict public-return checks and mechanism contracts. |
| 5. Documentation and skill | Current-language reference, separate how-to, checked examples, language/testing skills and user-visible limits. Roughly 250–450 lines. | Published examples run from a clean offline fixture set; remove a fixture and the documented run fails for the named reason. | Documentation snippets, symbols/links, `harn check`, conformance and source drift. |

No new dependency is assumed. Any proposed dependency requires a cargo-deny pass
before that PR is accepted. One implementation PR advances at a time. Downstream
adoption follows a released dependency update and its own reviewed PR; it does not
change this sequence into permission to replace deterministic policy.

## Evidence gaps

No production model calls were made for this dossier. Semantic accuracy,
confidence calibration, latency and actual cost for the three questions are
unmeasured. The native Jev adapter, portable capability transport, concurrent
reservation behavior, and crash recovery remain implementation obligations.
The proposed grammar has not been added to the parser. Review must resolve
whether its compiler-visible benefits justify the language surface before any
implementation claim is made.

[jev-intro]: https://typesafe.ai/blog/introducing-system-one-models-and-jev
[jev-primitives]: https://docs.typesafe.ai/primitives
[jev-confidence]: https://docs.typesafe.ai/confidence
[jev-sdk]: https://github.com/typesafe-ai/typesafe-sdk-python/tree/2ce5c65f13646cab6e6f782328194c9d85f3300a
[jev-adapter]: https://github.com/typesafe-ai/system-one-adapter-python/tree/adffc2eab300a4fa3c0e92252d4ffd6ceaa53700
[probably]: https://probably-lang.southpolesteve.workers.dev/
[probably-source]: https://probably-lang.southpolesteve.workers.dev/probably-source.zip
[dspy-signatures]: https://github.com/stanfordnlp/dspy/blob/40a6e168914a7b81a78b1a081d93f26de18c8d0d/docs/docs/learn/programming/signatures.md
[dspy-assertions]: https://github.com/stanfordnlp/dspy/blob/40a6e168914a7b81a78b1a081d93f26de18c8d0d/docs/docs/learn/programming/7-assertions.md
[dspy-cache]: https://github.com/stanfordnlp/dspy/blob/40a6e168914a7b81a78b1a081d93f26de18c8d0d/dspy/clients/cache.py
[dspy-dummy]: https://github.com/stanfordnlp/dspy/blob/40a6e168914a7b81a78b1a081d93f26de18c8d0d/dspy/utils/dummies.py
[marvin-fn]: https://askmarvin.ai/api-reference/marvin-fns-fn
[marvin-classify]: https://askmarvin.ai/functions/classify
[marvin-tests]: https://github.com/PrefectHQ/marvin/blob/main/tests/ai/fns/test_fn.py
[lmql-constraints]: https://lmql.ai/docs/latest/language/constraints.html
[lmql-certificates]: https://lmql.ai/docs/lib/inference-certificates.html
[lmql-test]: https://github.com/eth-sri/lmql/blob/main/src/lmql/tests/test_distribution.py
[guidance-select]: https://guidance.readthedocs.io/en/latest/generated/guidance.select.html
[guidance-mock]: https://guidance.readthedocs.io/en/latest/generated/guidance.models.Mock.html
[instructor-validation]: https://python.useinstructor.com/learning/validation/basics/
[instructor-tests]: https://github.com/567-labs/instructor/blob/main/tests/v2/test_retry_budget.py
[outlines]: https://dottxt-ai.github.io/outlines/latest/
[outlines-tests]: https://github.com/dottxt-ai/outlines/blob/main/tests/test_generator.py
[sk-planning]: https://learn.microsoft.com/en-us/semantic-kernel/concepts/planning
[sk-tests]: https://github.com/microsoft/semantic-kernel/blob/main/dotnet/src/Connectors/Connectors.Google.UnitTests/Core/Gemini/Clients/GeminiChatClientFunctionCallingTests.cs
[harn-call]: ../llm/llm_call.md
[harn-schema]: ../migrations/schema-as-type.md
[harn-checkpoint]: https://github.com/burin-labs/harn/blob/fb8d82ed5a3da77ea420118fdbdb0f77e9fd3a00/crates/harn-stdlib/src/stdlib/stdlib_checkpoint.harn
[harn-judge]: ../stdlib/agent-judge.md
[harn-requirements]: https://github.com/burin-labs/harn/blob/fb8d82ed5a3da77ea420118fdbdb0f77e9fd3a00/crates/harn-stdlib/src/stdlib/agent/completion_requirements.harn
[harn-cache-handler]: https://github.com/burin-labs/harn/blob/fb8d82ed5a3da77ea420118fdbdb0f77e9fd3a00/crates/harn-stdlib/src/stdlib/llm/handlers.harn
[harn-cache]: ../stdlib/cache.md
[harn-testbench]: ../dev/testbench.md
[harn-tape]: ../dev/tape-format.md
[harn-calibration]: https://github.com/burin-labs/harn/blob/fb8d82ed5a3da77ea420118fdbdb0f77e9fd3a00/crates/harn-stdlib/src/stdlib/agent/approval_review_calibration.harn
[harn-requirement-pr]: https://github.com/burin-labs/harn/pull/8293
