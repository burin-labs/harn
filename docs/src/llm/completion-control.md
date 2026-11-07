# Completion control

Agent loops combine mode-aware completion instructions, optional judges, and a
deterministic completion gate. These controls share the loop contract but can
be configured independently. The input guardrail and pre-turn scope classifier
are removed; check a request in the caller before starting `agent_loop`, or deny
it through tool policy.

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

With `repair_diagnostics.post_edit_reverify: true`, an edit following a failed
verification invokes the caller's completion verifier before completion can be
accepted. [Agent verification](../stdlib/agent-verification.md) defines the
verification evidence and optional terminal checks.

`turn_end_condition` adds a second gate after completion is detected. The loop projects
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
`turn_end_condition.max_invocations` is reached, or `max_verify_attempts` is exhausted.
Every judge call emits `JudgeDecision` with `session_id`, `iteration`,
`verdict`, `reasoning`, `next_step`, and `judge_duration_ms`, plus optional
`trigger`.

Set top-level `turn_end_condition.max_invocations` to a positive integer to cap repeated
turn-end-judge vetoes. Once reached, the loop stops with
`status: "completion_unverified"` and
`stop_reason: "turn_end_judge_cap_reached"`. The result carries
`{turn_end_condition: {invocations, vetoes, max_invocations, cap_reached}}`. Set it to
`0` to disable the terminal cap.

Use `turn_end_condition.cadence` to gate the judge. Omit it to judge every completion
candidate. `every: N` judges turns `N`, `2N`, and so on;
`min_iterations_before_first` skips the first K turns; and `when` accepts
`"always"`, `"stalled"`, or a closure receiving the same loop-state shape as
`loop_control`.

With `when: "stalled"`, a stall warning can fire the judge directly. An
`accept` action stops the loop with `stalled_turn_end_judge` before the repeated
tool call dispatches. A `continue` action also skips that pending call and
starts the next turn with the judge recovery. Generic stall feedback is used
only when the judge returned no recovery text. The corresponding
`JudgeDecision` event carries `trigger: "stalled"`.

```harn
import { AgentSpec } from "std/agent/options"

const judged_opts: AgentSpec = {
  loop_until_done: true,
  turn_end_condition: {
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
