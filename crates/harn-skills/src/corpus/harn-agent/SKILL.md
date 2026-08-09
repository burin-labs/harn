---
name: harn-agent
short: Agent runtime, lifecycle, capabilities, and supervision.
description: Build controllable agent loops with explicit authority, evidence, and recovery.
when_to_use: Use when authoring agent_loop, worker, supervisor, completion, or approval behavior.
---

# Harn agents

Use this skill for `agent_loop`, agent sessions, workers, supervisors, tools,
completion, and lifecycle controls.

Pair it with [[harn-orchestration]] for workflows, [[harn-testing]] for
deterministic evidence, and [[harn-product-quality]] for user-facing behavior.

## Ownership

- Harn owns loop, lifecycle, transcript, replay, lineage, and audit semantics.
- Hosts expose concrete capabilities and native approval UX.
- Keep semantic policy in the runtime or harness, not duplicated in host prose.
- Give each behavior one owner and project its state to every client.
- Prefer a deep agent module with a small typed interface.
- Do not infer lifecycle state by parsing assistant text.

## Session identity

- Give durable work a stable session id.
- Preserve parent/child lineage for delegated work.
- Carry workspace anchors and capability scope explicitly.
- Resume from durable state rather than reconstructing from UI messages.
- Treat transcript compaction as a state transition with continuity evidence.
- Keep secrets and provider credentials out of transcripts.
- Record the initiator and reason for lifecycle transitions.

## Capabilities and approval

- Grant only the capabilities required for the task.
- Child policy intersects the parent ceiling; delegation must not widen it.
- Let agents act autonomously inside approved, reversible scope.
- Request approval for genuine ambiguity, destructive action, production
  impact, exceptional spend, or new authority.
- Do not request approval for every ordinary tool call.
- Make a rejected or expired approval a typed terminal or waiting state.
- Route mutations through host-owned capabilities so undo and audit remain
  native to the product.

## Loop design

- Build options with `AgentLoopOptions`, `agent_options`, or `agent_preset`.
- Bound iterations, tool duration, concurrency, tokens, and cost.
- Use adaptive iteration budgets only when progress signals justify extension.
- Register typed tools with closed input and result shapes.
- Feed dispatch errors back as structured observations.
- Use `stop_after_successful_tools` for terminal tools.
- Prefer lifecycle events and progress records over recurring prose nudges.
- Keep model routing and fallback policy explicit.
- Built-in presets use named catalog ladders. Override routing with one owner
  (`provider` + `model`, inline `models`, or `ladder`) rather than splicing a
  caller route into the preset's catalog route.
- Use a completion judge only for a claim it can actually evaluate.

## Control events

Stop, wait, stand down, and pivot are controls, not suggestions.

- Stop prevents future tool and model work after the current cooperative seam.
- Wait suspends with durable resume conditions when known.
- Stand down returns a typed handoff and closes delegated work cleanly.
- Pivot replaces the active objective and suppresses stale completion.
- Acknowledge the control and emit the resulting state.
- Bound long-running tools so a control reaches a turn boundary promptly.
- Detect double resume and terminal-session resume attempts.
- Test controls while tools or workers are active.

## Progress and liveness

- Emit `agent_progress` after observable progress, not on a timer.
- Distinguish queued, running, waiting, blocked, stopped, failed, and complete.
- Persist enough progress to survive reconnect and process restart.
- Report the next active step and any external dependency.
- A spinner or recent token is not liveness evidence.
- Verify forward progress, interruption, recovery, and a terminal state.
- Do not declare completion while landing, release, or required verification
  remains.

## Completion

- Define completion against the user outcome, not lack of tool calls.
- Name a plausible falsifier for the completion claim.
- Gate on required artifacts, checks, receipts, or landed state.
- Use deterministic checks before an LLM judge.
- Calibrate model judges against representative accepted and rejected cases.
- Cap repeated vetoes and expose the reason for `verify_capped`.
- Preserve residual risk in the handoff.

## Recovery

- Put retry, backoff, timeout, and fallback policy in a composable caller or
  harness.
- Make retry eligibility explicit from typed errors.
- Use idempotency keys for externally visible operations.
- Persist checkpoints before irreversible or expensive steps.
- Prefer worktree-backed autonomous mutation to ambient working-directory state.
- Make restart, reconnect, and partial-success behavior observable.
- A lucky prompt is not a recovery strategy.

## Delegation

- Delegate a bounded outcome with scope, budget, evidence, and return contract.
- Keep the parent responsible for integration and final completion.
- Use durable coordination channels for replayable cross-agent facts.
- Use handoffs for one recipient and channels for publish/subscribe.
- Graceful stop should return a recursive typed handoff.
- Verify child evidence rather than trusting completion prose.

## Verify

- Check the script, lint it, and format-check it.
- Exercise the loop through its public interface.
- Test denied capability and approval paths.
- Test stop, wait, resume, stand-down, and pivot during active work.
- Inject tool, provider, timeout, and restart failures.
- Confirm session id, lineage, receipts, and progress survive replay.
- Verify cost, concurrency, and iteration ceilings.
- Run the canonical product path when the loop is user-facing.
