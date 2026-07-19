# Agent guardrails (`std/agent/guardrails`)

Input guardrails are the input-side pair to
[`agent_completion_gate`](./agent-judge.md): they decide whether a request should
stop before the main agent loop spends a model turn. Harn owns the orchestration
shape and event contract; callers own the policy classifier.

## `agent_input_guardrail`

```text
agent_input_guardrail(classifier?, options?) -> dict
```

Returns an options fragment you spread into `agent_loop`. The fragment carries an
`input_guardrail` closure plus `_input_guardrail` metadata. The guard runs before
the first main model turn. A tripwire emits `input_guardrail_verdict`, records a
zero-token assistant explanation, and terminates the loop with
`status: "input_guardrail"` and `stop_reason: "input_guardrail_tripwire"`.

```harn
import { agent_input_guardrail } from "std/agent/guardrails"

const guardrail_opts = agent_input_guardrail(
  { payload -> return {
    tripwire: payload.user_message.contains("private signing key"),
    reason: "private key exfiltration",
    label: "secret_exfiltration",
    confidence: 0.99,
  } },
  {confidence_threshold: 0.8},
)

agent_loop(task, nil, base_opts + guardrail_opts)
```

The classifier receives `{session_id, task, user_message, messages,
recent_context, provider, model}` and may return:

- `false`, `nil`, or `{tripwire: false}` to allow the loop.
- `true`, a string reason, or `{tripwire: true, reason, label?, confidence?}` to
  trip the guardrail.
- Provider-style fields such as `blocked`, `halt`, `unsafe`, `flagged`,
  `action: "block"`, or `verdict: "allow"`; the helper normalizes them into the
  same verdict shape.

If no classifier is supplied, the helper uses a structured `agent.input_guardrail`
checkpoint. Pass `provider` / `model` in `options` to pin a cheap route. With no
route override, normal Harn provider resolution applies.

| Option | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | Disable without removing the bundle |
| `confidence_threshold` | `0.0` | Minimum confidence required for a positive tripwire |
| `provider` / `model` | provider defaults | Route for the structured LLM classifier |
| `policy` / `system` | built-in prompt | Policy text for the structured LLM classifier |
| `max_tokens` | `256` | Classifier output cap |
| `temperature` | `0.0` | Classifier sampling temperature |
| `fail_open` | `true` | When the classifier errors, allow the main loop and emit an error verdict |

## Direct Preflight

```text
agent_input_guardrail_check(task, classifier?, options?) -> dict
```

Runs the same guardrail outside `agent_loop` and returns the normalized verdict:
`{tripwire, reason, label, confidence, confidence_threshold, classifier_kind,
model}`. Use this in scripts that want to branch explicitly before deciding
whether to call an agent loop.

## Event Contract

Every configured guard emits one `input_guardrail_verdict` event on the first
turn. Stable fields are `iteration`, `tripwire`, `reason`, `label`,
`confidence`, `confidence_threshold`, `classifier_kind`, `model`, and `error`.
This is intentionally separate from `scope_classifier_verdict`: scope alerts
remain about workspace anchoring, while input guardrails are generic policy and
cost controls.
