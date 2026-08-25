# Completion control

Agent loops combine mode-aware completion instructions, optional judges, input
guardrails, and a deterministic completion gate. These controls share the loop
contract but can be configured independently.

## Completion instructions

The default nudge depends on the tool mode:

- Tagged text-tool stages ask for concrete tool progress and reserve
  `<done>##DONE##</done>` for real completion.
- Native-tool stages ask for concrete tool progress and treat final text with
  no tool calls as completion.
- No-tool sentinel stages ask for concrete progress and reserve bare
  `##DONE##` for completion.

With `loop_until_done: true`, the system prompt states the same mode-aware
completion rule and requires the agent to keep working until the task is done.

## Completion judges

`done_judge` adds a second gate after completion is detected. The loop projects
the lossless transcript into bounded effect and verification evidence, then
expects one strict object: `{verdict: "done" | "continue", detail}`. The
`detail` is limited to 240 characters. It records the strongest supporting
evidence for `done`, or the single most important gap and next action for
`continue`. Harn injects a continuation detail as recovery feedback.

The judge decides content only: whether the work is complete and honestly
reported. Whether the response carries a done sentinel or any other marker is
wire protocol, owned by the deterministic completion layer, and the judge
prompts say so. A missing marker is never grounds for a veto, and a `continue`
detail must name a substantive next action rather than a reformat or a
restatement. A judge that could veto on formatting would stall work that is
already verified and complete, since the loop has no way to satisfy such a
veto except by having the model repeat a token it has already declined to
emit.

A veto injects runtime feedback and the loop continues until the judge accepts,
`done_judge.max_invocations` is reached, or `max_verify_attempts` is exhausted.
Every judge call emits `JudgeDecision` with `session_id`, `iteration`,
`verdict`, `reasoning`, `next_step`, and `judge_duration_ms`, plus optional
`trigger`.

Set top-level `done_judge.max_invocations` to a positive integer to cap repeated
done-judge vetoes. Once reached, the loop stops with
`status: "completion_unverified"` and
`stop_reason: "done_judge_cap_reached"`. The result carries
`{done_judge: {invocations, vetoes, max_invocations, cap_reached}}`. Set it to
`0` to disable the terminal cap.

Use `done_judge.cadence` to gate the judge. Omit it to judge every completion
candidate. `every: N` judges turns `N`, `2N`, and so on;
`min_iterations_before_first` skips the first K turns; and `when` accepts
`"always"`, `"stalled"`, or a closure receiving the same loop-state shape as
`loop_control`.

With `when: "stalled"`, a stall warning can fire the judge directly. An
`accept` action stops the loop with `stalled_done_judge` before the repeated
tool call dispatches. A `continue` action also skips that pending call and
starts the next turn with the judge recovery. Generic stall feedback is used
only when the judge returned no recovery text. The corresponding
`JudgeDecision` event carries `trigger: "stalled"`.

```harn
import { AgentSpec } from "std/agent/options"

const judged_opts: AgentSpec = {
  loop_until_done: true,
  done_judge: {
    cadence: {every: 5, when: "always", max_invocations: 3},
  },
}
agent_loop(harness, task, system, judged_opts)
```

`when: "stalled"` does not fire on ordinary completion candidates. It lets
stall diagnostics request a completion check from an observed signal instead
of a fixed prompt.

Provider catalog rows may set `completion_review` to name how much scrutiny
that model's own terminal output needs. Omit it for `standard` (today's LLM
confirmation). `light` may skip the verification-slot LLM judge only when the
deterministic gate already passed and `stop_reason` is `sentinel`. The object
requires `evidence` pointing at the measurement that justified the row.
`max_judge_calls` optionally lowers the default verification-judge cap of 5
unless the session sets `verify_completion_judge.max_invocations`. Equivalence
groups must share one scrutiny. No shipping row is `light` yet.

Implementation details and trace schemas live in
[Agent plane ownership](../dev/agent-loops.md#completion-checkpoints).

## Input guardrail (`agent_input_guardrail`)

`agent_input_guardrail(classifier?, options?)` from `std/agent/guardrails`
builds the input-side bookend for `agent_completion_gate`. It runs before the
first main `agent_loop` model turn and spreads into loop options as
`input_guardrail`. A tripwire records an `input_guardrail_verdict` event, writes
a zero-token assistant explanation, and stops the loop with
`status: "input_guardrail"` and `stop_reason: "input_guardrail_tripwire"`.

```harn,ignore
import { agent_input_guardrail } from "std/agent/guardrails"

const guardrail_opts = agent_input_guardrail(
  { payload -> return cheap_policy_classifier(payload.user_message) },
  {confidence_threshold: 0.8},
)
agent_loop(harness, task, system, base_opts + guardrail_opts)
```

For an explicit preflight verdict instead of loop composition, use
`agent_input_guardrail_check(task, classifier?, options?)`. It returns the same
`{tripwire, reason, label, confidence}` shape.

## Completion gate (`agent_completion_gate`)

`agent_completion_gate(runtime, options)` returns an options fragment for
`agent_loop`. It checks host-supplied write and verification facts through
`verify_completion` and can add a bounded LLM judge.

```harn,ignore
import { agent_completion_gate } from "std/agent/completion_gate"

agent_loop(harness, task, system, base_opts + agent_completion_gate(
  harness.runtime, {
    facts: fn(ctx) { return host_completion_facts(ctx.session_id) },
    verify_command: fn() { return host_run_verify() },
    // optional bounded LLM judge, capped at 5 by default
    judge: true,
  }))
```

See [Completion gate (`std/agent/completion_gate`)](../stdlib/agent-judge.md)
for the option table, fact types, veto ladder, and bounded judge.
