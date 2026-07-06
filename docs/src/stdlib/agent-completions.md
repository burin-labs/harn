# Agent Completions

`std/agent/completions` provides shared envelopes for inline suggestions,
next-edit predictions, and completion telemetry. Hosts still collect editor
state and draw UI; Harn owns the reusable shapes so IDEs, TUIs, replayers, and
harnesses do not invent incompatible JSON.

Import it with:

```harn
import {
  completion_context,
  completion_model_route,
  completion_policy,
  completion_policy_decision,
  completion_proposal,
  completion_shown_event,
  completion_decision_event,
  completion_trust_record,
  completion_usage_record,
} from "std/agent/completions"
```

Core helpers:

- `completion_context(input)` normalizes file path, language, stack, cursor,
  prefix/suffix, recent edits, LSP or IntelliSense facts, diagnostics, symbols,
  local scope, open files, and codebase snippets into
  `harn.completion.context.v1`.
- `completion_policy(input)` normalizes workspace or enterprise controls:
  enabled state, language/provider/model allow and deny lists, remote-model
  allowance, codebase-context allowance, response-time and prompt-size limits,
  prompt recording mode, trust-event emission, and retention.
- `completion_policy_decision(context, route, policy)` returns
  `{allowed, reason_code, reason, policy}` with stable machine-readable denial
  reasons.
- `completion_model_route(opts)` asks the model catalog for a coding-oriented
  route that can be overridden by the host.
- `completion_proposal(input)` normalizes inline insertions, replacements,
  next edits, and workspace edits into `harn.completion.proposal.v1`.
- `completion_shown_event(...)` and `completion_decision_event(...)` produce
  `harn.completion.event.v1` records for analytics and replay.
- `completion_trust_record(event)` projects a completion event into the
  TrustGraph record shape.
- `completion_usage_record(event)` projects completion usage into the common
  usage row shape used by host dashboards.

Example:

```harn
import {
  completion_context,
  completion_policy,
  completion_policy_decision,
  completion_proposal,
  completion_shown_event,
} from "std/agent/completions"

pipeline default() {
  const context = completion_context({
    surface: "editor",
    file_path: "src/main.rs",
    language: "rust",
    prefix: "const result = client.",
    suffix: "\nprintln!(\"{result:?}\");",
    symbols: [{name: "client", kind: "variable"}],
  })

  const policy = completion_policy({
    disabled_languages: ["markdown"],
    allow_remote_models: false,
    prompt_recording: "metadata",
  })

  const decision = completion_policy_decision(
    context,
    {provider: "ollama", model_id: "qwen2.5-coder"},
    policy,
  )
  if !decision.allowed {
    return decision.reason
  }

  const shown = completion_shown_event(
    completion_proposal({
      kind: "inline_insert",
      provider: "ollama",
      model_id: "qwen2.5-coder",
      insert_text: "send()?;",
    }),
    context,
    {input_tokens: 800, output_tokens: 12, cost_usd: 0.0},
    {policy: policy, prompt: "rendered private prompt"},
  )
  return shown.record_id
}
```

`prompt_recording: "metadata"` stores prompt length and hash. `"full"` stores
prompt text for replay. `"off"` records neither.
