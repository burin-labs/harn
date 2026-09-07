# Agent harness ownership

Status: proposed target design, 2026-09-06. The interfaces below describe intended
ownership. They are not a claim that every path is implemented or adopted.

An agent harness needs one owner for each decision. Grading must preserve what
the run did. This design puts those duties in five layers with small typed seams.
It extends the runtime's current contracts. It adds no callback framework.

## The five layers

| Layer | Responsibility |
| --- | --- |
| **runner** | Assemble context and tools, drive execution, accept controls and enforce resource ceilings |
| **stop decision** | Decide whether task evidence supports a proposed finish |
| **stall handler** | Observe progress and repeated outcomes, deliver bounded recovery, and stop incomplete attempts |
| **grader** | Evaluate a preserved terminal attempt against an independent case contract |
| **meter stick** | Admit measurements and determine what a comparison establishes |

Harn owns the runner, stop decision, stall handler, and the grader's execution
and resume machinery. Hosts supply task facts and case policy. The experiment
owner defines the meter stick's method and admission rules; Harn owns the shared
statistical functions. A host must not reimplement those mechanisms to apply its
case policy.

A run ending, a task being done and a passing grade are distinct facts.
The end of a model reply proves neither of the last two.
A grader that cannot run must not change why the run ended.

## Module map

Agent modules below are under `crates/harn-stdlib/src/stdlib/agent/`.

| Existing modules | Owning responsibility in the target design |
| --- | --- |
| `loop_run.harn`, `loop_turn_options.harn`, `postturn.harn`, `loop_tool_calls.harn` | runner |
| `transcript.harn`, `control.harn` | runner context and accepted controls |
| `completion_gate.harn`, `judge.harn`, `completion_evidence.harn`, `completion_requirements.harn` | stop decision |
| `completion_claim.harn` | Actor proposal and citation validation at the stop-decision boundary |
| `stall_*.harn` | stall handler and explicit checkpoint lifetime |
| `std/workflow` | runner lifetime across workflow stages |
| `std/eval` and replay primitives | grader support and meter stick statistics |

These modules exist today. That does not prove a host calls the generic gate
or supplies the facts its policy needs. A gate can also permit a stop when it
lacks facts. Permission to stop is not a passing check.

The target replaces duplicate stop rules and progress folds. A host callback
with a second set of rules would keep the same split behind a smaller interface.

## Proposed typed seams

These shapes state what each seam needs. They are not new type names or code
examples. Reuse the current types for calls, stops, controls and saved state.
Check facts once where they are made. Readers then trust that contract.

| Shape | Information | Invariant |
| --- | --- | --- |
| Completed turn | Session, task generation, evidence position, completed dispatches, accepted controls | Fold once, including tool-free turns and the final dispatch before return |
| Completed dispatch | Call identity, disposition, producer outcome, mutation status, verification role and completeness | Transport success cannot stand in for effect or verification success |
| Completion evidence | Requirements, mutation epoch, verifier observation and evidence position, required output/response facts | Stale evidence cannot discharge a newer requirement |
| Stop decision | Decision identity, cause, supported/unmet requirements, evidence references, bounded continuation state | Permission to end never fabricates success |
| Stall checkpoint | Session, consumed position, progress and recovery state | No duplicated observation or unrelated session inheritance |
| Sealed attempt | Attempt and source identity, terminal cause, immutable candidate and transcript references | Every grader evaluates the same candidate |
| Instrument result | Identity/revision, required/advisory role, pending/returned/invalid/abstained status, evidence | Absence is not a measured result |
| Measurement admission | Expected roster, case/treatment identity, grade references, exclusion reasons, telemetry coverage | An empty read cannot establish a measured zero |

These closed forms specify the target fields. They extend the owning schemas;
they are not parallel wire types or current Harn syntax. IDs use the runtime's
existing identities. Unions admit only the listed variants. `List<T>` allows an
empty list; `Option<T>` is `none` or `some(T)`.

```text
ActorIntent = continue | propose_done { evidence: List<EvidenceRef> }
  | report_blocked { reason: String, unmet: List<RequirementId> }

RunEvidence = {
  session_id: SessionId, task_generation: UInt, turn_id: TurnId,
  evidence_position: UInt, actor_intent: ActorIntent,
  dispatches: List<CompletedDispatch>, accepted_control: Option<ControlRef>
}
CompletedDispatch = {
  call_id: CallId, evidence_ref: EvidenceRef,
  disposition: executed | rejected | deferred | canceled,
  mutation: applied | unchanged | not_applied | not_applicable,
  verification: not_verifier | not_run | pending
    | passed { source_epoch: UInt }
    | failed { source_epoch: UInt, diagnostics: List<Diagnostic>, complete: Bool }
    | unknown { reason: String }
}

StopDecision = {
  decision_id: DecisionId, turn_id: TurnId, task_generation: UInt,
  result: continue { unmet: List<RequirementId>, feedback: Option<FeedbackRef> }
    | finish { status: done | blocked | incomplete,
        supported: List<RequirementId>, unmet: List<RequirementId>,
        evidence: List<EvidenceRef>, reason: String }
}

StallAction = retry { cause: StallCause, feedback: FeedbackRef, remaining: UInt }
  | escalate { cause: StallCause, target: AdmittedTarget, evidence: List<EvidenceRef> }
  | stop { cause: StallCause, unmet: List<RequirementId>, evidence: List<EvidenceRef> }
StallObservation = { position: UInt, checkpoint: StallCheckpoint,
  action: Option<StallAction> }

GraderVerdict = {
  attempt_id: AttemptId, snapshot_id: SnapshotId, revision: UInt,
  instruments: List<InstrumentResult>, vetoes: List<GradeVeto>,
  result: passed | failed | pending { instruments: List<InstrumentId> }
    | invalid { reason: String } | abstained { reason: String }
}
GradeVeto = { instrument_id: InstrumentId, requirement_id: RequirementId,
  reason: String, evidence: List<EvidenceRef> }
```

Each completed turn has exactly one committed `StopDecision`. Judge attempts
feed that decision; they are not extra stop decisions. A control can interrupt
an unfinished turn without inventing a completed turn. Typed actor intent is a
proposal, not proof. Cited evidence must exist and match the current task.

Clean `done` has no unmet requirements. It needs no invented gap or extra work.
A passing grade has no binding veto or pending required check. A stall stop means
incomplete work and cannot manufacture `done`. Invalid or pending grades do not
become actor failures. The case policy declares which vetoes are binding.

## runner

The runner owns dispatch and the lifetime of its facts. It saves completed facts
before a stage returns. Stages that share a session can carry its checkpoint.
A new session gets none of the old state. Restore must not replay a callback or
lose the last call. Progress rules also see turns with no tool calls.

An accepted stop prevents more effects. A steer starts a new task generation
and blocks stale completion. The runner owns grants and hard resource limits.
It does not need to diagnose model progress to enforce them.

Hosts return effect facts and show decisions. They do not guess what ran from
display text or keep a second transcript store.

## stop decision

One owner orders the checks and any bounded model review. A natural finish is a
proposal. A trusted check can prove its part of the task. Other unmet needs stay
open. A judge timeout cannot erase a check that ran and passed.

Pressure to finish never grants more access. Blocked and incomplete are honest
ways to end. When the retry budget runs out, the run may end with unmet needs.
It cannot mark those needs as met.

Host facts can mark a chat task, a required output, a review file, or a source
change. Those inputs must not hide a second set of stop rules. Any evidence the
actor cites must exist in the run and match the current task generation.

Extend the existing `CompletionClaim` and `task_complete` tool for actor intent.
The current implementation validates citations before judging, but is opt-in,
allows an implicit prose fallback, requires cited tool work, and rejects
`blocked_on` as an invalid completion. The cutover must support a truthful
read-only answer and a blocked terminal result without inventing tool evidence
or forcing more work. Remove the fallback and its switch when the typed path
covers those cases; do not add a second completion tool.

## stall handler

One native fold owns repeat detection, progress and bounded repair. Hosts must
not join raw events into a second history, hash their own signatures or build
fake native state.

An applied edit earns write credit. A rejected or unchanged edit does not.
A deferred check cannot clear a failure. A passing housekeeping command cannot
replace the declared check. A full set of errors that shrinks can prove progress.
A partial set cannot prove that fewer errors remain.

A clean check followed by a new edit starts a new verification episode.
Edits while a failure remains must not keep resetting the cap.
Changed error text alone cannot grant endless retries.

Arming, opportunity, decision and delivered feedback are distinct. A warning
does not prove the model received a correction. A stall stop means incomplete
work. Tests must distinguish useful repair from an early stop before a threshold
is chosen. This design chooses none.

## grader

The grader reads a sealed final candidate and its evidence. One coordinator owns
pending checks, safe resume and grade revisions. It uses the current replay and
eval tools instead of a second agent runner.

A missing required check leaves the attempt ungraded. A missing advisory check
can leave a grade valid only if the case contract allows it. It cannot count as
agreement. An invalid answer differs from a check that never ran.
Later checks may flag conflicts, but cannot change grades to make them agree.
A grading retry never replays actor effects or changes why the run ended.

## meter stick

The meter stick fixes the case roster, treatment, split, required checks and
valid outcomes. It reads complete grade records and reports pending, invalid
and excluded results apart. The shared stats module still owns the math.

A single model trial is a smoke read. Quality claims need repeated trials,
stated uncertainty and tests of the relevant failures. A stop reason or done
marker cannot stand in for a grade. Missing usage or provider facts stay unknown.
They do not become a zero total.

## Migration order

1. Ship the typed census and missing-read checks. Record source defaults apart
   from measured reachability.
2. Delete each retired option and its obsolete branch. Promote accepted behavior
   directly; do not retain a compatibility switch.
3. Consolidate completion requirements and adjudication into the stop decision.
   Extend the existing evidence contract with actor intent and completed facts;
   remove duplicate ordering and prose reconstruction with consumer adoption.
4. Adopt one stall handler, including checkpoint lifetime across stage forms.
   Choose its recovery threshold from recovered-stall and false-stop evidence,
   then delete competing folds, counters and thresholds.
5. Apply the five names to modules, events and documentation. Regenerate protocol
   projections from the owning schemas.

The clean-completion schema already accepts empty or absent gaps; the owning
regression also checks that an accepted completion makes one judge call.
Remaining adoption must preserve that behavior. Grading and measurement contracts
above constrain the cutover; they do not authorize a separate grading rewrite.

Consumer adoption requires an artifact containing the owner changes. A version
label or source census alone does not establish runtime behavior. Count integration
and retained behavioral tests when measuring the deletion.

## Published harness comparisons

These sources were inspected on 2026-09-06. They motivate the boundaries, not a
claim that another harness implements this design or proves a threshold.

| Harness | Published behavior | Design implication |
| --- | --- | --- |
| [SWE-agent](https://swe-agent.com/latest/reference/agent/) | Distinct exits for cost, context, timeout and repeated format failures can preserve a submission | Preserve terminal cause separately from candidate quality |
| [OpenHands](https://github.com/OpenHands/software-agent-sdk/blob/main/openhands-sdk/openhands/sdk/conversation/stuck_detector.py) | Detects repeated actions/observations, errors, monologue and alternating patterns within a bounded event window after the latest user message | Centralize progress history and reset its task boundary explicitly |
| [Aider](https://aider.chat/docs/usage/lint-test.html) | Configured lint/test commands supply diagnostics and nonzero exits for repair | Consume executed verifier facts rather than interpreting a completion claim |
| [Codex](https://openai.com/index/unrolling-the-codex-agent-loop/) | A turn can contain multiple model/tool iterations before its final assistant message | Keep turn termination separate from independent task grading |
| [Claude Code](https://code.claude.com/docs/en/hooks#stop) | A stop hook can require continuation; its active-hook field helps prevent indefinite repetition; user interrupts bypass the hook | Bound completion retries and preserve control priority |

## Evidence required for adoption

- Actual same-session stage handoff consumes final dispatch once; a distinct
  session receives no inherited evidence.
- Rejected and unchanged mutations earn no write progress; an applied mutation
  remains applied even when an ancillary operation reports an error.
- Failed verification survives successful housekeeping and deferred verification;
  an actually executed later pass clears it.
- Changed repair can recover; unchanged repetition stops incomplete; tool-free
  or no-op turns cannot gain permanent progress immunity.
- Accepted stop prevents another effect, and steer supersedes stale completion.
- A partially completed grading batch resumes exactly its pending instruments
  without rerunning actors or changing their terminal candidates.
- Missing required results and contradictory grade records defeat completeness
  claims; a known-positive read proves the measurement path is live.

Fixtures must exercise the owning runtime, actual dispatch and terminal evidence.
Synthetic checkpoints test a shape; test counts inventory cases. Neither alone
establishes adoption or improved convergence.
