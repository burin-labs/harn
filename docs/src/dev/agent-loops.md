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
or `stop_reason` themselves. A2A suspension remains wire-compatible as
`working` plus `metadata.harn.pause` until the proposed paused state is
standardized.

## Tool registries

`AgentModelSpec.tools` references `ToolRegistry` from `std/tools`. It no longer
contains a local structural copy. The typechecker and conformance suite verify
that registries cross package boundaries and remain assignable to `AgentSpec`.

## Completion judge isolation

`done_judge` and `verify_completion_judge` run on a transcript projection. The
judge prompt includes the worker transcript, but the judge's own LLM request and
structured response are not appended to `harness.agent.messages(session_id)`.
Only legitimate worker turns and explicit runtime feedback injections mutate the
worker session transcript.

This keeps replay and follow-up turns deterministic: a judge veto may inject a
feedback message because that message is intended to steer the worker, while an
accepted judge decision only emits `judge_decision` and `typed_checkpoint`
events. Tests should assert transcript equality around accepted judge calls
instead of inferring isolation from event counts.
