# Agent plane ownership

Harn has one public agent-loop entrypoint: `agent_loop`. `HarnessAgent` owns
session mutations, checkpoints, event emission, transcript projection, and
terminal classification. `HarnessLlm` owns one model request, and
`HarnessTools` owns dispatch. Keeping those responsibilities separate prevents
a convenience wrapper from creating another lifecycle or result vocabulary.

## Typed stages

The loop crosses `agent_stage(agent, session_id, stage, input?)` at each safe
injection boundary. `AgentStage` is a closed vocabulary covering iteration,
compaction, tool dispatch, command hold, daemon idle, and loop exit. The seam
accepts only `HarnessAgent`; model and tool handles stay in the stages that use
them. Every checkpoint event, queued injection drain, and session hook therefore
observes the same stage name.

## Specification and result contracts

`AgentSpec` is the intersection of six named records: model, execution,
capability, lifecycle, context, and observability. These records are a static
projection over one flat runtime value, not nested configuration and not a
second normalizer.

`AgentResult` describes the value returned by `agent_loop` and
`HarnessAgent.session_finalize`. Its `terminal` member is projected from the
Rust `AgentTerminalKind` owner and checked against it in the VM test suite. ACP
metadata, CLI protocol artifacts, A2A task status/metadata, replay run records,
and Harn callers consume that projection instead of classifying `final_status`
or `stop_reason` themselves. A completion judge that cannot accept before its
deadline or policy limit produces `completion_unverified`; A2A projects that to
`failed`. A2A suspension remains wire-compatible as `working` plus
`metadata.harn.pause` until the proposed paused state is standardized.

## Tool registries

`AgentModelSpec.tools` references `ToolRegistry` from `std/tools`. It no longer
contains a local structural copy. The typechecker and conformance suite verify
that registries cross package boundaries and remain assignable to `AgentSpec`.

## Package boundary

The agent contracts, LLM dialects, and provider catalog remain in `harn-vm`.
They share normalization, dispatch, replay, and terminal semantics with the
runtime, and every current Rust consumer needs that complete behavior. Harn
Cloud links `harn-vm` and `harn-serve`; Burin and 20eq consume the versioned
Harn toolchain rather than a dialect-only library.

Extracting a catalog or dialect crate would create another versioned projection
without an independent consumer or release cadence. The deeper boundary is the
small typed interface inside `harn-vm`: capability data selects one dialect,
`AgentSpec` configures one loop, `ToolRegistry` supplies its tools, and
`AgentResult` reports its terminal state. Generated catalog artifacts and drift
checks keep external projections aligned with the same release.

## Completion checkpoints

Built-in completion judges are terminal checkpoints, not worker turns. Harn
projects the immutable worker transcript into bounded effect and verification
evidence. Judge requests exclude callable tool schemas and raw transcript
replay. The judge request and response are not appended to
`harness.agent.messages(session_id)`.

The task, rubric, and output schema occupy the stable start of the request. The
changing user message carries the latest mutation, verification, problem, and
transcript-integrity evidence. This placement lets a provider reuse the stable
prompt prefix, but cache behavior is only a performance detail.

The `harn.completion_judge_cache.v1` checkpoint records cache eligibility, a
hash of the static prefix, and provider-reported cache reads and writes. These
fields are telemetry. They do not prove equivalent evidence and never reuse a
prior verdict.

Deadline admission uses monotonic remaining time, the configured operation
budget, and the terminal reserve. `prompt_token_estimate` is a rough
character-count estimate, not a provider tokenizer result, and does not decide
admission. The VM bounds the complete checkpoint operation, including local
setup and response parsing.

`harn.completion_judge_evidence_projection.v1` records what evidence entered
the request. `harn.completion_directive_receipt.v1` records the final action,
reason, admission result, and whether repair feedback reached the next turn.
`evidence_id` and transcript digests correlate these records and reveal changed
input; they never substitute for a new judgment.

A `continue` result injects repair feedback because it is meant to steer the
worker. An `accept` or `stop_unverified` result changes only events and terminal
state. Tests should assert the worker transcript around accepted judge calls
instead of inferring isolation from event counts.
